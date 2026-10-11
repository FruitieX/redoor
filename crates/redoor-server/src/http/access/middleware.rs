//! REST authentication and coarse authorization, run after routing so the route table can
//! key on the matched route template and read its `{agent}` parameter.

use axum::{
    RequestPartsExt,
    body::Body,
    extract::{MatchedPath, RawPathParams, Request, State},
    http::{HeaderMap, StatusCode, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};
use redoor_protocol::ids::AgentId;

use super::{
    Principal,
    routes::{self, Access},
};
use crate::http::{error::ApiError, state::ServerState};

/// Authenticates the caller, applies the route's access rule and hands the principal to the
/// handler. Layered with `Router::layer`, so it only runs for matched browser routes.
pub(crate) async fn access_control(
    State(state): State<ServerState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(matched) = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned())
    else {
        return next.run(request).await;
    };
    let access = routes::classify(request.method(), &matched).unwrap_or_else(|| {
        warn_unclassified(request.method(), &matched);
        Access::SessionOnly
    });
    if access == Access::Public
        || routes::is_one_time_token_raw_request(request.method(), request.uri())
    {
        return next.run(request).await;
    }
    let principal = match authenticate(&state, request.headers()).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let (mut parts, body) = request.into_parts();
    if let Err(error) = authorize(&principal, access, &mut parts).await {
        return error.into_response();
    }
    parts.extensions.insert(principal);
    next.run(Request::from_parts(parts, body)).await
}

/// A bearer token alone decides when present, so a stale cookie can never upgrade a rejected
/// token; otherwise the browser session cookie is used.
async fn authenticate(state: &ServerState, headers: &HeaderMap) -> Result<Principal, Response> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if let Some(secret) = bearer {
        return match state.tokens.authenticate(secret.trim()).await {
            Some(grant) => Ok(Principal::Token(grant)),
            None => Err(unauthorized("Access token is invalid, expired or revoked")),
        };
    }
    if state.auth.session_valid(headers).await {
        return Ok(Principal::Session);
    }
    Err(unauthorized("Authentication required"))
}

/// Applies the coarse route rule; `AnyPrincipal` and `Handler` routes decide in the handler.
async fn authorize(
    principal: &Principal,
    access: Access,
    parts: &mut Parts,
) -> Result<(), ApiError> {
    match access {
        Access::Public | Access::AnyPrincipal | Access::Handler => Ok(()),
        Access::SessionOnly => principal.require_session(),
        Access::Device(permissions) => {
            let agent = parts
                .extract::<RawPathParams>()
                .await
                .ok()
                .and_then(|params| {
                    params
                        .iter()
                        .find(|(name, _)| *name == "agent")
                        .map(|(_, value)| AgentId::from(value))
                });
            match agent {
                Some(agent) => principal.require(permissions, &agent),
                // A device rule on a route without `{agent}` is a table bug: fail closed.
                None => principal.require_session(),
            }
        }
    }
}

/// Builds the 401 body every client already understands.
fn unauthorized(message: &str) -> Response {
    ApiError::status(StatusCode::UNAUTHORIZED, message).into_response()
}

/// Reminds developers in debug builds that a new API route needs a table entry; release
/// builds stay quiet because the session-only fallback is already safe.
fn warn_unclassified(method: &axum::http::Method, matched: &str) {
    #[cfg(debug_assertions)]
    {
        use std::{
            collections::BTreeSet,
            sync::{Mutex, OnceLock},
        };
        static WARNED: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
        let key = format!("{method} {matched}");
        let first = WARNED
            .get_or_init(Mutex::default)
            .lock()
            .is_ok_and(|mut warned| warned.insert(key.clone()));
        if matched.starts_with("/api/") && first {
            redoor_core::log!(
                redoor_protocol::wire::log::Level::Warning,
                "Route has no access table entry, treating it as session-only: {key}"
            );
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = (method, matched);
}

#[cfg(test)]
mod tests {
    use axum::{
        Router,
        extract::{MatchedPath, RawPathParams, Request},
        middleware::{self, Next},
        response::Response,
        routing::get,
    };

    /// Mirrors what `access_control` reads, reporting it back in response headers.
    async fn probe(request: Request, next: Next) -> Response {
        let matched = request
            .extensions()
            .get::<MatchedPath>()
            .map(|matched| matched.as_str().to_owned())
            .unwrap_or_default();
        let (mut parts, body) = request.into_parts();
        let agent = axum::RequestPartsExt::extract::<RawPathParams>(&mut parts)
            .await
            .ok()
            .and_then(|params| {
                params
                    .iter()
                    .find(|(name, _)| *name == "agent")
                    .map(|(_, value)| value.to_owned())
            })
            .unwrap_or_default();
        let mut response = next.run(Request::from_parts(parts, body)).await;
        response
            .headers_mut()
            .insert("x-matched", matched.parse().unwrap());
        response
            .headers_mut()
            .insert("x-agent", agent.parse().unwrap());
        response
    }

    /// The design depends on `Router::layer` middleware seeing the matched template and the
    /// decoded `{agent}` parameter; this proves it with a real router.
    #[tokio::test]
    async fn layer_middleware_sees_matched_path_and_params() {
        let app = Router::new()
            .route("/api/v1/agents/{agent}/ls/{*path}", get(|| async { "ok" }))
            .layer(middleware::from_fn(probe));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = reqwest::get(format!(
            "http://{address}/api/v1/agents/my%20agent/ls/tmp/x"
        ))
        .await
        .unwrap();
        // The template, not the concrete path, is what the access table is keyed on.
        assert_eq!(
            response.headers()["x-matched"],
            "/api/v1/agents/{agent}/ls/{*path}"
        );
        // The agent parameter arrives percent-decoded, matching the ids in token scopes.
        assert_eq!(response.headers()["x-agent"], "my agent");
        server.abort();
    }
}
