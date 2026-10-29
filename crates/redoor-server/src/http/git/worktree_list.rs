//! REST endpoint that lists the main and linked worktrees of a repository.

use axum::{
    Json,
    extract::{Path, State as AxumState},
};
use redoor_protocol::api::git::worktree_list::GitWorktreeListResponse;

use super::route_path;
use crate::http::{
    access::ResolvedDevice, agent_client::GitWorktreeList, agent_helpers::AgentFilePath,
    error::ApiError, state::ServerState,
};

/// Route: `GET /api/v1/agents/{agent}/git/worktrees[/{*path}]`.
pub(crate) async fn git_worktrees_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    Path(params): Path<AgentFilePath>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<GitWorktreeListResponse>, ApiError> {
    let call = GitWorktreeList {
        path: route_path(params.path)?,
    };
    Ok(Json(state.agents.call(&agent_id, call).await?))
}
