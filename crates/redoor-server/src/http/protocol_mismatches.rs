//! REST view of agents refused because they speak another protocol version.

use axum::{Json, extract::State as AxumState};
use redoor_protocol::wire::handshake::{PROTOCOL, ProtocolMismatchListResponse};

use super::state::ServerState;

/// Route: `GET /api/v1/protocol-mismatches`
///
/// Lets the UI warn about outdated standalone agents, which never register and would
/// otherwise only show up in the server log.
pub(crate) async fn list_protocol_mismatches_handler(
    AxumState(state): AxumState<ServerState>,
) -> Json<ProtocolMismatchListResponse> {
    Json(ProtocolMismatchListResponse {
        server_protocol: PROTOCOL.to_string(),
        attempts: state.protocol_mismatches.list(),
    })
}
