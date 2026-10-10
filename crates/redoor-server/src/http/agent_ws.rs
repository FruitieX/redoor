//! The agent websocket endpoint: authentication and protocol negotiation at upgrade time.
//!
//! Both checks happen before the upgrade is accepted, so a client with a wrong token or an
//! incompatible protocol never gets a socket and the session actor only ever sees
//! authenticated agents that speak [`PROTOCOL`].

use crate::actors;
use axum::{
    Json,
    extract::{ConnectInfo, State as AxumState, ws::WebSocketUpgrade},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use redoor_core::log;
use redoor_protocol::ids::SocketId;
use redoor_protocol::wire::handshake::{
    AGENT_VERSION_HEADER, PROTOCOL, ProtocolMismatchResponse, offered_protocols,
};
use redoor_protocol::wire::log::Level;
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use subtle::ConstantTimeEq;

use super::state::ServerState;

/// Constant-time string equality so invalid tokens do not leak length via timing.
fn constant_time_eq(left: &str, right: &str) -> bool {
    let left_digest = Sha256::digest(left.as_bytes());
    let right_digest = Sha256::digest(right.as_bytes());
    bool::from(left_digest.ct_eq(&right_digest))
}

/// Extracts the `Authorization: Bearer` credential of an upgrade request.
pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::to_string)
}

/// The subprotocols an upgrade request offers.
fn offered(headers: &HeaderMap) -> Vec<String> {
    headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .map(offered_protocols)
        .unwrap_or_default()
}

/// Whether an upgrade request offers the protocol this server speaks.
pub(crate) fn offers_protocol(headers: &HeaderMap) -> bool {
    offered(headers).iter().any(|protocol| protocol == PROTOCOL)
}

/// The agent's self-reported binary version, for diagnosing rejections.
fn agent_version(headers: &HeaderMap) -> Option<String> {
    headers
        .get(AGENT_VERSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// Refuses an upgrade that does not offer [`PROTOCOL`] with `426` and remembers the attempt.
fn protocol_mismatch(state: &ServerState, remote: SocketAddr, headers: &HeaderMap) -> Response {
    let version = agent_version(headers);
    let offered = offered(headers);
    // An error, not a warning: the agent cannot connect until an operator upgrades it.
    redoor_core::log_failure!(
        Level::Error,
        "Rejecting agent with an incompatible protocol: remote={}, agent_version={}, offered={:?}, server_protocol={}",
        remote,
        version.as_deref().unwrap_or("unknown"),
        offered,
        PROTOCOL
    );
    state
        .protocol_mismatches
        .record(remote.to_string(), version, offered);
    let server_version = redoor_core::identity::current_binary_identity().version;
    (
        StatusCode::UPGRADE_REQUIRED,
        Json(ProtocolMismatchResponse {
            server_protocol: PROTOCOL.to_string(),
            message: format!(
                "This server speaks {PROTOCOL} (version {server_version}); upgrade the agent to the server's version"
            ),
            server_version,
        }),
    )
        .into_response()
}

/// Route: `GET /api/v1/agent/ws`
///
/// `401` for a missing or wrong token (checked first, so unauthenticated clients cannot fill
/// the mismatch list), `426` for a missing or unknown protocol, otherwise the upgrade echoing
/// [`PROTOCOL`].
pub(crate) async fn agent_websocket_handler(
    ws: WebSocketUpgrade,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    AxumState(state): AxumState<ServerState>,
) -> Response {
    let authenticated = bearer_token(&headers)
        .is_some_and(|token| constant_time_eq(&token, state.auth.agent_token()));
    if !authenticated {
        log!(
            Level::Warning,
            "Rejecting agent with a missing or invalid token: remote={}, agent_version={}",
            remote,
            agent_version(&headers).as_deref().unwrap_or("unknown")
        );
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !offers_protocol(&headers) {
        return protocol_mismatch(&state, remote, &headers);
    }
    let router_ref = state.router_ref;
    let watchdog_registry = state.watchdog_registry;
    let timeouts = state.runtime.ws_timeouts;
    ws.protocols([PROTOCOL])
        .on_upgrade(move |socket| {
            actors::session::handle_websocket(
                socket,
                SocketId::new(),
                router_ref,
                watchdog_registry,
                timeouts,
            )
        })
        .into_response()
}

/// Route: `GET /ws`, the endpoint of agents that predate protocol negotiation.
///
/// Such agents authenticate inside the socket, which this server no longer supports, so every
/// attempt is refused with `426` and listed as a mismatch; otherwise outdated agents would
/// only appear as an endless stream of failed requests in their own logs.
pub(crate) async fn legacy_agent_websocket_handler(
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    AxumState(state): AxumState<ServerState>,
) -> Response {
    protocol_mismatch(&state, remote, &headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    /// Verifies only a well-formed bearer credential is extracted.
    #[test]
    fn bearer_token_requires_the_bearer_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer t"));
        // The agent sends exactly this form.
        assert_eq!(bearer_token(&headers).as_deref(), Some("t"));
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic t"));
        // Other schemes must not be mistaken for an agent token.
        assert_eq!(bearer_token(&headers), None);
    }

    /// Verifies negotiation accepts the server's protocol among several offers.
    #[test]
    fn protocol_offer_is_found_among_several() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("redoor.v1, redoor.v2"),
        );
        // An agent may offer older protocols too; the server picks its own.
        assert!(offers_protocol(&headers));
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("redoor.v3"),
        );
        // A different version alone must be refused.
        assert!(!offers_protocol(&headers));
    }
}
