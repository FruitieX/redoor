//! REST endpoints that show what a commit is and what it changed.

use axum::{
    Json,
    extract::{Path, Query, State as AxumState},
};
use redoor_protocol::api::git::{commit::GitCommitDiffResponse, revision::GitRevisionQuery};

use super::{checked_revision, route_path};
use crate::http::{
    access::ResolvedDevice, agent_client::GitCommitDiff, agent_helpers::AgentFilePath,
    error::ApiError, state::ServerState,
};

/// Route: `GET /api/v1/agents/{agent}/git/commit-diff[/{*path}]?rev=`.
pub(crate) async fn git_commit_diff_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    Path(params): Path<AgentFilePath>,
    Query(query): Query<GitRevisionQuery>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<GitCommitDiffResponse>, ApiError> {
    let call = GitCommitDiff {
        path: route_path(params.path)?,
        rev: checked_revision(query.rev)?,
    };
    Ok(Json(state.agents.call(&agent_id, call).await?))
}
