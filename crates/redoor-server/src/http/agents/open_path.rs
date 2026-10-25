//! REST endpoint that opens a path with the agent host's desktop handler.

use crate::http::access::ResolvedDevice;
use axum::{
    Json,
    extract::{Path, State as AxumState},
};
use redoor_protocol::api::agents::OpenPathResponse;

use crate::http::{
    agent_client::OpenPath,
    agent_helpers::{AgentFilePath, absolute_path_from_url},
    error::ApiError,
    state::ServerState,
};

/// Route: `POST /api/v1/agents/{agent}/open/{*path}` launches a path on the agent desktop.
pub(crate) async fn open_path_agent_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    Path(AgentFilePath { path }): Path<AgentFilePath>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<OpenPathResponse>, ApiError> {
    let path = absolute_path_from_url(path.unwrap_or_default());
    state
        .agents
        .call(&agent_id, OpenPath { path: path.clone() })
        .await?;
    Ok(Json(OpenPathResponse { path }))
}
