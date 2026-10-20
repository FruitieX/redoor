//! REST handlers for access token management and a token's view of itself.

use std::collections::BTreeSet;

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Response},
};
use jiff::Timestamp;
use redoor_protocol::{
    api::tokens::{
        AccessTokenListResponse, AccessTokenSecretResponse, CreateAccessTokenRequest,
        DeleteAccessTokenResponse, DeviceScope, UpdateAccessTokenRequest,
    },
    ids::AgentId,
};
use uuid::Uuid;

use super::store::{IssuedToken, TokenPolicy};
use crate::http::{
    access::Principal, agents::list_agent_snapshots, error::ApiError, state::ServerState,
};

/// Route: `GET /api/v1/tokens` lists every token's metadata.
pub(crate) async fn list(State(state): State<ServerState>) -> Response {
    no_store(Json(AccessTokenListResponse {
        tokens: state.tokens.list().await,
    }))
}

/// Route: `POST /api/v1/tokens` mints a token and returns its secret once.
pub(crate) async fn create(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<CreateAccessTokenRequest>,
) -> Result<Response, ApiError> {
    validate_mutation_origin(&headers)?;
    let expires_at = request
        .expires_at
        .as_deref()
        .map(|value| {
            value
                .parse::<Timestamp>()
                .map_err(|_| ApiError::bad_request("Token expiry must be an RFC 3339 timestamp"))
        })
        .transpose()?;
    require_known_devices(&state, &request.devices).await?;
    let issued = state
        .tokens
        .create(
            TokenPolicy {
                name: request.name,
                permissions: request.permissions,
                devices: request.devices,
            },
            expires_at,
        )
        .await?;
    Ok(secret_response(issued))
}

/// Route: `PUT /api/v1/tokens/{token_id}` replaces the policy without changing the secret.
pub(crate) async fn update(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(token_id): Path<String>,
    Json(request): Json<UpdateAccessTokenRequest>,
) -> Result<Response, ApiError> {
    validate_mutation_origin(&headers)?;
    let id = parse_id(&token_id)?;
    require_known_devices(&state, &request.devices).await?;
    let metadata = state
        .tokens
        .update(
            id,
            TokenPolicy {
                name: request.name,
                permissions: request.permissions,
                devices: request.devices,
            },
        )
        .await?;
    Ok(no_store(Json(metadata)))
}

/// Route: `POST /api/v1/tokens/{token_id}/rotate` issues a new secret for the same token.
pub(crate) async fn rotate(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(token_id): Path<String>,
) -> Result<Response, ApiError> {
    validate_mutation_origin(&headers)?;
    let issued = state.tokens.rotate(parse_id(&token_id)?).await?;
    Ok(secret_response(issued))
}

/// Route: `DELETE /api/v1/tokens/{token_id}` revokes a token immediately.
pub(crate) async fn delete(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(token_id): Path<String>,
) -> Result<Response, ApiError> {
    validate_mutation_origin(&headers)?;
    state.tokens.delete(parse_id(&token_id)?).await?;
    Ok(no_store(Json(DeleteAccessTokenResponse { deleted: true })))
}

/// Route: `GET /api/v1/tokens/current` lets a token learn its own permissions (CLI `status`).
pub(crate) async fn current(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
) -> Result<Response, ApiError> {
    let id = current_id(&principal)?;
    let metadata = state
        .tokens
        .get(id)
        .await
        .ok_or_else(|| ApiError::not_found("Access token not found"))?;
    Ok(no_store(Json(metadata)))
}

/// Route: `DELETE /api/v1/tokens/current` lets a token revoke itself (CLI `logout`).
pub(crate) async fn delete_current(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
) -> Result<Response, ApiError> {
    state.tokens.delete(current_id(&principal)?).await?;
    Ok(no_store(Json(DeleteAccessTokenResponse { deleted: true })))
}

/// The `current` routes only make sense for a token, not a browser session.
fn current_id(principal: &Principal) -> Result<Uuid, ApiError> {
    principal.token_id().ok_or_else(|| {
        ApiError::bad_request("This request is not authenticated with an access token")
    })
}

/// Parses a path id so malformed ids are a client error rather than "not found".
fn parse_id(value: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(value).map_err(|_| ApiError::bad_request("Token id must be a UUID"))
}

/// Rejects explicit device ids the server has never seen, which are almost always typos.
async fn require_known_devices(state: &ServerState, devices: &DeviceScope) -> Result<(), ApiError> {
    let DeviceScope::Only { device_ids } = devices else {
        return Ok(());
    };
    let known = list_agent_snapshots(state)
        .await?
        .into_iter()
        .map(|agent| agent.id)
        .collect::<BTreeSet<AgentId>>();
    match device_ids.iter().find(|device| !known.contains(device)) {
        Some(unknown) => Err(ApiError::bad_request(format!(
            "Unknown device id '{unknown}'"
        ))),
        None => Ok(()),
    }
}

/// Session-authenticated mutations reject cross-site requests, since the cookie would
/// otherwise let another site mint tokens in the user's name.
fn validate_mutation_origin(headers: &HeaderMap) -> Result<(), ApiError> {
    if headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "cross-site")
    {
        return Err(ApiError::forbidden("Cross-site token mutation rejected"));
    }
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(());
    };
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::forbidden("Mutation Origin cannot be validated"))?;
    let origin_host = origin
        .split_once("://")
        .map(|(_, rest)| rest.trim_end_matches('/'));
    if origin_host != Some(host) {
        return Err(ApiError::forbidden(
            "Mutation Origin does not match this server",
        ));
    }
    Ok(())
}

/// Wraps a newly issued secret, which must never be cached.
fn secret_response(issued: IssuedToken) -> Response {
    no_store(Json(AccessTokenSecretResponse {
        token: issued.secret,
        metadata: issued.metadata,
    }))
}

/// Keeps token responses out of browser and proxy caches.
fn no_store(body: impl IntoResponse) -> Response {
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    response
}
