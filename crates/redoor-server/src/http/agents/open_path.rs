use axum::{
    Json,
    extract::{Path, State as AxumState},
};
use redoor_protocol::{api::agents::OpenPathResponse, ids::AgentId};

use crate::http::{
    agent_client::OpenPath,
    agent_helpers::{AgentFilePath, absolute_path_from_url},
    error::ApiError,
    state::ServerState,
};

/// Route: `POST /api/v1/agents/{agent}/open/{*path}` launches a path on the agent desktop.
pub(crate) async fn open_path_agent_handler(
    Path(AgentFilePath { agent, path }): Path<AgentFilePath>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<OpenPathResponse>, ApiError> {
    let path = absolute_path_from_url(path.unwrap_or_default());
    state
        .agents
        .call(&AgentId::from(agent), OpenPath { path: path.clone() })
        .await?;
    Ok(Json(OpenPathResponse { path }))
}
