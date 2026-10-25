//! REST endpoint that changes ownership of an agent path.

use crate::http::access::ResolvedDevice;
use axum::{
    Json,
    extract::{Path, Query, State as AxumState},
};
use redoor_protocol::api::files::{ChownPathRequest, ChownPathResponse};

use crate::http::{
    agent_client::ChownPath,
    agent_helpers::{AgentFilePath, absolute_path_from_url},
    error::ApiError,
    state::ServerState,
};

/// Route: `POST /api/v1/agents/{agent}/chown/{*path}` changes followed-inode ownership.
pub(crate) async fn chown_path_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    Path(AgentFilePath { path }): Path<AgentFilePath>,
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
    Ok(Json(state.agents.call(&agent_id, call).await?))
}
