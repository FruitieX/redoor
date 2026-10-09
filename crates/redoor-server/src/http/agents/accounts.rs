use axum::{
    Json,
    extract::{Path, State as AxumState},
};
use redoor_protocol::{api::agents::AgentAccountsResponse, ids::AgentId};

use crate::http::{agent_client::ListAccounts, error::ApiError, state::ServerState};

/// Route: `GET /api/v1/agents/{agent}/accounts` loads the host user/group catalog on demand.
pub(crate) async fn accounts_handler(
    Path(agent): Path<String>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<AgentAccountsResponse>, ApiError> {
    let accounts = state
        .agents
        .call(&AgentId::from(agent), ListAccounts)
        .await?;
    Ok(Json(accounts))
}
