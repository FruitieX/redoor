//! Browser-account management API for the shared MCP machine-token service.

use super::{
    agents::list_agent_snapshots,
    machine_tokens::{
        CreateMachineToken, CreatedMachineToken, MachineCredentialMetadata, MachineScope,
    },
    state::ServerState,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use jiff::Timestamp;
use redoor_core::log;
use redoor_protocol::api::{
    ErrorResponse,
    mcp::{
        CreateMcpTokenRequest, McpTokenListResponse, McpTokenMetadataResponse, McpTokenScope,
        McpTokenSecretResponse, RevokeMcpTokenResponse, UpdateMcpTokenRequest,
        UpdateMcpTokenResponse,
    },
};
use redoor_protocol::wire::log::Level;
use std::collections::BTreeSet;
use uuid::Uuid;

/// Prevents browsers and intermediaries from retaining token-management responses.
fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store, max-age=0"),
    );
    response
}

/// Maps the exported REST vocabulary onto the canonical authorization enum.
fn machine_scope(scope: McpTokenScope) -> MachineScope {
    match scope {
        McpTokenScope::DevicesRead => MachineScope::DevicesRead,
        McpTokenScope::FilesRead => MachineScope::FilesRead,
        McpTokenScope::FilesWrite => MachineScope::FilesWrite,
        McpTokenScope::FilesDelete => MachineScope::FilesDelete,
        McpTokenScope::CommandsExecute => MachineScope::CommandsExecute,
        McpTokenScope::AgentsManage => MachineScope::AgentsManage,
    }
}

/// Converts canonical scope data back into a generated browser contract.
fn response_scope(scope: MachineScope) -> McpTokenScope {
    match scope {
        MachineScope::DevicesRead => McpTokenScope::DevicesRead,
        MachineScope::FilesRead => McpTokenScope::FilesRead,
        MachineScope::FilesWrite => McpTokenScope::FilesWrite,
        MachineScope::FilesDelete => McpTokenScope::FilesDelete,
        MachineScope::CommandsExecute => McpTokenScope::CommandsExecute,
        MachineScope::AgentsManage => McpTokenScope::AgentsManage,
    }
}

/// Omits owner and verifier details while retaining grant identity for rotation checks.
fn metadata_response(metadata: MachineCredentialMetadata) -> McpTokenMetadataResponse {
    McpTokenMetadataResponse {
        credential_id: metadata.credential_id.to_string(),
        grant_id: metadata.grant_id.to_string(),
        name: metadata.name,
        scopes: metadata.scopes.into_iter().map(response_scope).collect(),
        device_ids: metadata.device_ids.into_iter().collect(),
        created_at: metadata.created_at.to_string(),
        expires_at: metadata.expires_at.map(|v| v.to_string()),
        revoked_at: metadata.revoked_at.map(|v| v.to_string()),
        last_used_at: metadata.last_used_at.map(|v| v.to_string()),
    }
}

/// Wraps one-time output with cache prevention before it crosses HTTP.
fn secret_response(created: CreatedMachineToken) -> Response {
    no_store(
        Json(McpTokenSecretResponse {
            token: created.secret,
            credential: metadata_response(created.metadata),
        })
        .into_response(),
    )
}

/// Uses browser fetch metadata and same-host Origin comparison to reject CSRF.
fn validate_mutation_origin(headers: &HeaderMap) -> Result<(), &'static str> {
    if headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "cross-site")
    {
        return Err("Cross-site MCP token mutation rejected");
    }
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return Ok(());
    };
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return Err("Mutation Origin cannot be validated");
    };
    let origin_host = origin
        .split_once("://")
        .map(|(_, rest)| rest.trim_end_matches('/'));
    if origin_host != Some(host) {
        return Err("Mutation Origin does not match this server");
    }
    Ok(())
}

/// Returns only the authenticated account's safe credential metadata.
pub(crate) async fn list_handler(State(state): State<ServerState>) -> Response {
    match state
        .machine_tokens
        .list_for_owner(state.auth.username().to_string())
        .await
    {
        Ok(tokens) => no_store(
            Json(McpTokenListResponse {
                tokens: tokens.into_iter().map(metadata_response).collect(),
            })
            .into_response(),
        ),
        Err(error) => internal_error(error),
    }
}

/// Validates devices, scopes, and expiry before issuing one new secret.
pub(crate) async fn create_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<CreateMcpTokenRequest>,
) -> Response {
    if let Err(message) = validate_mutation_origin(&headers) {
        return forbidden(message);
    }
    let expiry = match request.expires_at {
        Some(value) => match value.parse::<Timestamp>() {
            Ok(value) => Some(value),
            Err(_) => return bad_request("expiry must be an RFC 3339 timestamp"),
        },
        None => None,
    };
    let devices = request.device_ids.into_iter().collect::<BTreeSet<_>>();
    let known = match list_agent_snapshots(&state).await {
        Ok(agents) => agents
            .into_iter()
            .map(|agent| agent.id.to_string())
            .collect::<BTreeSet<_>>(),
        Err(error) => return internal_error(anyhow::anyhow!(error)),
    };
    if let Some(unknown) = devices.iter().find(|device| !known.contains(*device)) {
        return bad_request(&format!("unknown device id '{unknown}'"));
    }
    let input = CreateMachineToken {
        owner_id: state.auth.username().to_string(),
        name: request.name,
        scopes: request.scopes.into_iter().map(machine_scope).collect(),
        device_ids: devices,
        expires_at: expiry,
    };
    match state.machine_tokens.create(input).await {
        Ok(created) => secret_response(created),
        Err(error) => bad_request(&error.to_string()),
    }
}

/// Saves editable policy for an owned active credential without issuing a new secret.
pub(crate) async fn update_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(token_id): Path<String>,
    Json(request): Json<UpdateMcpTokenRequest>,
) -> Response {
    if let Err(message) = validate_mutation_origin(&headers) {
        return forbidden(message);
    }
    let Ok(token_id) = Uuid::parse_str(&token_id) else {
        return bad_request("token id must be a UUID");
    };
    if request.name.trim().is_empty() || request.name.len() > 100 {
        return bad_request("token name must contain 1 to 100 characters");
    }
    let devices = request.device_ids.into_iter().collect::<BTreeSet<_>>();
    let known = match list_agent_snapshots(&state).await {
        Ok(agents) => agents
            .into_iter()
            .map(|agent| agent.id.to_string())
            .collect::<BTreeSet<_>>(),
        Err(error) => return internal_error(anyhow::anyhow!(error)),
    };
    if let Some(unknown) = devices.iter().find(|device| !known.contains(*device)) {
        return bad_request(&format!("unknown device id '{unknown}'"));
    }
    match state
        .machine_tokens
        .update_for_owner(
            state.auth.username().to_string(),
            token_id,
            request.name,
            request.scopes.into_iter().map(machine_scope).collect(),
            devices,
        )
        .await
    {
        Ok(Some(credential)) => no_store(
            Json(UpdateMcpTokenResponse {
                credential: metadata_response(credential),
            })
            .into_response(),
        ),
        Ok(None) => not_found(),
        Err(error) if error.to_string().contains("active MCP credential") => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: error.to_string(),
            }),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

/// Immediately revokes one credential owned by the browser account.
pub(crate) async fn revoke_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(token_id): Path<String>,
) -> Response {
    if let Err(message) = validate_mutation_origin(&headers) {
        return forbidden(message);
    }
    let Ok(token_id) = Uuid::parse_str(&token_id) else {
        return bad_request("token id must be a UUID");
    };
    match state
        .machine_tokens
        .revoke_for_owner(state.auth.username().to_string(), token_id)
        .await
    {
        Ok(true) => no_store(Json(RevokeMcpTokenResponse { revoked: true }).into_response()),
        Ok(false) => not_found(),
        Err(error) => internal_error(error),
    }
}

/// Atomically replaces one active credential while preserving its stable grant.
pub(crate) async fn rotate_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(token_id): Path<String>,
) -> Response {
    if let Err(message) = validate_mutation_origin(&headers) {
        return forbidden(message);
    }
    let Ok(token_id) = Uuid::parse_str(&token_id) else {
        return bad_request("token id must be a UUID");
    };
    match state
        .machine_tokens
        .rotate_for_owner(state.auth.username().to_string(), token_id)
        .await
    {
        Ok(Some(created)) => secret_response(created),
        Ok(None) => not_found(),
        Err(error) if error.to_string().contains("active MCP credential") => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: error.to_string(),
            }),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

/// Produces a consistent validation response.
fn bad_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            error: message.to_string(),
        }),
    )
        .into_response()
}
/// Rejects untrusted browser request origins.
fn forbidden(message: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ErrorResponse {
            error: message.to_string(),
        }),
    )
        .into_response()
}
/// Hides whether a credential belongs to a different owner.
fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: "MCP token not found".to_string(),
        }),
    )
        .into_response()
}
/// Logs storage failures without serializing credential data.
fn internal_error(error: anyhow::Error) -> Response {
    log!(Level::Warning, "MCP token management failed: {error}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: "MCP token storage failed".to_string(),
        }),
    )
        .into_response()
}
