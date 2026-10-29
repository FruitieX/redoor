//! REST endpoint that counts the uncommitted changes of one checkout, apart from the worktree
//! list so the list answers without walking every checkout first.

use axum::{
    Json,
    extract::{Path, State as AxumState},
};
use redoor_protocol::api::git::worktree_status::GitWorktreeStatusResponse;

use super::route_path;
use crate::http::{
    access::ResolvedDevice, agent_client::GitWorktreeStatus, agent_helpers::AgentFilePath,
    error::ApiError, state::ServerState,
};

/// Route: `GET /api/v1/agents/{agent}/git/worktree-status[/{*path}]`.
pub(crate) async fn git_worktree_status_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    Path(params): Path<AgentFilePath>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<GitWorktreeStatusResponse>, ApiError> {
    let call = GitWorktreeStatus {
        path: route_path(params.path)?,
    };
    Ok(Json(state.agents.call(&agent_id, call).await?))
}
