//! REST endpoint listing an agent host's users and groups for ownership pickers.

use crate::http::access::ResolvedDevice;
use axum::{Json, extract::State as AxumState};
use redoor_protocol::api::agents::AgentAccountsResponse;

use crate::http::{agent_client::ListAccounts, error::ApiError, state::ServerState};

/// Route: `GET /api/v1/agents/{agent}/accounts` loads the host user/group catalog on demand.
pub(crate) async fn accounts_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<AgentAccountsResponse>, ApiError> {
    let accounts = state.agents.call(&agent_id, ListAccounts {}).await?;
    Ok(Json(accounts))
}
