//! Authenticated, stateless MCP 2026-07-28 HTTP endpoint.

mod discovery;
mod dispatch;
mod error;
mod operations;
mod policy;
mod schema;
mod tools;
mod transport;

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Extension, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    limit::RequestBodyLimitLayer,
};

use super::{
    agent_helpers::AgentFilePath,
    machine_tokens::{MachinePrincipal, MachineTokenService},
    raw::{RawQueryParams, upload::RawUploadQueryParams},
    state::ServerState,
};
use redoor_core::log;
use redoor_protocol::wire::log::Level;
use schema::{JsonRpcRequest, JsonRpcResponse};

/// Inline JSON request cap leaves large file transfer work to streaming endpoints in section 2.
const REQUEST_LIMIT: usize = 256 * 1024;
/// Per-credential burst budget limits accidental loops while remaining generous for MCP clients.
const REQUESTS_PER_MINUTE: usize = 120;
/// Per-credential concurrency protects server control work from one machine principal.
const CONCURRENT_REQUESTS: usize = 8;

/// MCP-only state keeps bearer policy independent of browser and agent authentication.
#[derive(Clone)]
pub(crate) struct McpState {
    tokens: MachineTokenService,
    allowed_origins: Arc<Vec<String>>,
    limits: Arc<Mutex<HashMap<uuid::Uuid, PrincipalLimit>>>,
    /// Retains principal-owned asynchronous work independently of HTTP request lifetimes.
    operations: operations::OperationRegistry,
}

/// Mutable limiter state is keyed by credential rather than spoofable client identity.
struct PrincipalLimit {
    requests: VecDeque<Instant>,
    concurrency: Arc<Semaphore>,
}

/// Holding the owned permit in request extensions releases capacity after response completion.
#[derive(Clone)]
struct ConcurrencyPermit(#[allow(dead_code)] Arc<OwnedSemaphorePermit>);

impl McpState {
    /// Shares the durable service and explicitly configured browser origins with route middleware.
    pub(crate) async fn new(
        tokens: MachineTokenService,
        allowed_origins: Vec<String>,
        data_dir: &std::path::Path,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            tokens,
            allowed_origins: Arc::new(allowed_origins),
            limits: Arc::new(Mutex::new(HashMap::new())),
            operations: operations::OperationRegistry::in_directory(
                data_dir.join("mcp/operations"),
            )
            .await?,
        })
    }

    /// Applies a rolling per-minute budget and a non-blocking concurrency bound.
    async fn acquire(&self, credential_id: uuid::Uuid) -> Result<ConcurrencyPermit, StatusCode> {
        let now = Instant::now();
        let semaphore = {
            let mut limits = self.limits.lock().await;
            let limit = limits
                .entry(credential_id)
                .or_insert_with(|| PrincipalLimit {
                    requests: VecDeque::new(),
                    concurrency: Arc::new(Semaphore::new(CONCURRENT_REQUESTS)),
                });
            while limit
                .requests
                .front()
                .is_some_and(|seen| now.duration_since(*seen) >= Duration::from_secs(60))
            {
                limit.requests.pop_front();
            }
            if limit.requests.len() >= REQUESTS_PER_MINUTE {
                return Err(StatusCode::TOO_MANY_REQUESTS);
            }
            limit.requests.push_back(now);
            limit.concurrency.clone()
        };
        semaphore
            .try_acquire_owned()
            .map(Arc::new)
            .map(ConcurrencyPermit)
            .map_err(|_| StatusCode::TOO_MANY_REQUESTS)
    }
}

/// Builds a separately layered route so wildcard browser CORS never reaches MCP.
pub(crate) fn router(state: ServerState) -> Router<ServerState> {
    let mcp_state = state.mcp.clone();
    let allowed_origins = mcp_state
        .allowed_origins
        .iter()
        .filter_map(|origin| HeaderValue::from_str(origin).ok())
        .collect::<Vec<_>>();
    let protocol = Router::new()
        .route("/mcp", post(handler))
        .layer(RequestBodyLimitLayer::new(REQUEST_LIMIT));
    protocol
        .merge(Router::new().route(
            "/mcp/files/{device}/{*path}",
            get(download_file).put(upload_file),
        ))
        .layer(middleware::from_fn_with_state(
            mcp_state.clone(),
            authenticate,
        ))
        .layer(middleware::from_fn_with_state(mcp_state, validate_origin))
        .layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(allowed_origins))
                .allow_methods([Method::POST, Method::GET, Method::PUT])
                .allow_headers([
                    header::AUTHORIZATION,
                    header::CONTENT_TYPE,
                    header::ACCEPT,
                    header::HeaderName::from_static("mcp-protocol-version"),
                    header::HeaderName::from_static("mcp-method"),
                    header::HeaderName::from_static("mcp-name"),
                ]),
        )
}

/// Streams an authorized file with existing range/backpressure semantics.
async fn download_file(
    State(state): State<ServerState>,
    Extension(principal): Extension<MachinePrincipal>,
    Path((device, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if policy::require_device(
        &principal,
        super::machine_tokens::MachineScope::FilesRead,
        &device,
        &serde_json::Value::Null,
    )
    .is_err()
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    super::raw::raw_agent_handler(
        Path(AgentFilePath {
            agent: device,
            path: Some(path),
        }),
        Query(RawQueryParams {
            download: Some("1".to_string()),
            one_time_token: None,
        }),
        State(state),
        headers,
    )
    .await
    .into_response()
}

/// Streams an authorized upload without imposing the JSON MCP body limit.
async fn upload_file(
    State(state): State<ServerState>,
    Extension(principal): Extension<MachinePrincipal>,
    Path((device, path)): Path<(String, String)>,
    Query(query): Query<RawUploadQueryParams>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if policy::require_device(
        &principal,
        super::machine_tokens::MachineScope::FilesWrite,
        &device,
        &serde_json::Value::Null,
    )
    .is_err()
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    super::raw::upload::raw_agent_put_handler(
        Path(AgentFilePath {
            agent: device,
            path: Some(path),
        }),
        Query(query),
        State(state),
        headers,
        body,
    )
    .await
    .into_response()
}

/// Authenticates a bearer credential afresh and never considers cookies or agent secrets.
async fn authenticate(State(state): State<McpState>, mut request: Request, next: Next) -> Response {
    let secret = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty());
    let principal = match secret {
        Some(secret) => state.tokens.authenticate(secret).await,
        None => Ok(None),
    };
    let Ok(Some(principal)) = principal else {
        return bearer_error(
            StatusCode::UNAUTHORIZED,
            "invalid_token",
            "A valid MCP bearer token is required",
        );
    };
    let permit = match state.acquire(principal.credential_id).await {
        Ok(permit) => permit,
        Err(status) => return bearer_error(status, "rate_limited", "MCP request limit exceeded"),
    };
    request.extensions_mut().insert(principal);
    request.extensions_mut().insert(permit);
    next.run(request).await
}

/// Rejects browser origins unless explicitly configured, while native origin-less clients remain valid.
async fn validate_origin(State(state): State<McpState>, request: Request, next: Next) -> Response {
    let Some(origin) = request.headers().get(header::ORIGIN) else {
        return next.run(request).await;
    };
    let Ok(origin) = origin.to_str() else {
        return StatusCode::FORBIDDEN.into_response();
    };
    if !state
        .allowed_origins
        .iter()
        .any(|allowed| allowed == origin)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

/// Parses one bounded request, validates mirrored metadata, dispatches, and audits the outcome.
async fn handler(
    State(state): State<ServerState>,
    Extension(principal): Extension<MachinePrincipal>,
    Extension(_permit): Extension<ConcurrencyPermit>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let started = Instant::now();
    let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&body);
    let value = match parsed {
        Ok(value) => value,
        Err(_) => {
            return audit_error(
                principal.credential_id,
                None,
                None,
                started,
                error::McpError::parse("Invalid JSON"),
            );
        }
    };
    let (request, version) = match JsonRpcRequest::validate(value) {
        Ok(request) => request,
        Err(error) => return audit_error(principal.credential_id, None, None, started, error),
    };
    let tool = request
        .params
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    if let Err(error) = transport::validate_headers(&headers, &request, &version) {
        return audit_error(
            principal.credential_id,
            Some(&request.method),
            tool.as_deref(),
            started,
            error,
        );
    }
    match dispatch::request(&state, &principal, &request).await {
        Ok(result) => {
            audit(
                principal.credential_id,
                &request.method,
                tool.as_deref(),
                started,
                "success",
            );
            (
                StatusCode::OK,
                Json(JsonRpcResponse::complete(request.id, result)),
            )
                .into_response()
        }
        Err(error) => audit_error(
            principal.credential_id,
            Some(&request.method),
            tool.as_deref(),
            started,
            error,
        ),
    }
}

/// Emits a standards-shaped bearer challenge without including credential material.
fn bearer_error(status: StatusCode, code: &str, message: &str) -> Response {
    let mut response = (
        status,
        Json(serde_json::json!({ "error": code, "error_description": message })),
    )
        .into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_str(&format!("Bearer realm=\"redoor-mcp\", error=\"{code}\""))
            .expect("static bearer challenge must be valid"),
    );
    response
}

/// Audits rejected protocol requests and converts them into their specified HTTP response.
fn audit_error(
    credential_id: uuid::Uuid,
    method: Option<&str>,
    tool: Option<&str>,
    started: Instant,
    error: error::McpError,
) -> Response {
    audit(
        credential_id,
        method.unwrap_or("invalid"),
        tool,
        started,
        &format!("error:{}", error.code),
    );
    error.into_response()
}

/// Records only credential identity and request diagnostics, never headers, arguments, or secrets.
fn audit(
    credential_id: uuid::Uuid,
    method: &str,
    tool: Option<&str>,
    started: Instant,
    outcome: &str,
) {
    log!(
        Level::Info,
        "MCP request: credential_id={}, method={}, tool={}, duration_ms={}, outcome={}",
        credential_id,
        method,
        tool.unwrap_or("-"),
        started.elapsed().as_millis(),
        outcome
    );
}
