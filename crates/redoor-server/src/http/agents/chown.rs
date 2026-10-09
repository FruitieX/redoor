use axum::{
    Json,
    extract::{Path, Query, State as AxumState},
};
use redoor_protocol::{
    api::files::{ChownPathRequest, ChownPathResponse},
    ids::AgentId,
};

use crate::http::{
    agent_client::ChownPath,
    agent_helpers::{AgentFilePath, absolute_path_from_url},
    error::ApiError,
    state::ServerState,
};

/// Route: `POST /api/v1/agents/{agent}/chown/{*path}` changes followed-inode ownership.
pub(crate) async fn chown_path_handler(
    Path(AgentFilePath { agent, path }): Path<AgentFilePath>,
    AxumState(state): AxumState<ServerState>,
    Query(request): Query<ChownPathRequest>,
) -> Result<Json<ChownPathResponse>, ApiError> {
    if request.owner.is_none() && request.group.is_none() {
        return Err(ApiError::bad_request("Owner or group is required"));
    }
    let call = ChownPath {
        path: absolute_path_from_url(path.unwrap_or_default()),
        owner: request.owner,
        group: request.group,
    };
    Ok(Json(state.agents.call(&AgentId::from(agent), call).await?))
}
