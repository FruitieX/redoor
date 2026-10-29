//! REST endpoints that walk the history of a path: the commits which changed it, and the
//! commits on either side of one of them.

use axum::{
    Json,
    extract::{Path, Query, State as AxumState},
};
use redoor_protocol::api::git::{
    GitPageQuery,
    history::{GitCommitNeighborsResponse, GitLogQuery, GitLogResponse},
    revision::GitRevisionQuery,
};

use super::{checked_page, checked_revision, route_path};
use crate::http::{
    access::ResolvedDevice,
    agent_client::{GitCommitNeighbors, GitLog},
    agent_helpers::AgentFilePath,
    error::ApiError,
    state::ServerState,
};

/// Route: `GET /api/v1/agents/{agent}/git/log[/{*path}]?rev=&skip=&limit=`.
pub(crate) async fn git_log_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    Path(params): Path<AgentFilePath>,
    Query(query): Query<GitLogQuery>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<GitLogResponse>, ApiError> {
    let call = GitLog {
        path: route_path(params.path)?,
        rev: query.rev.map(checked_revision).transpose()?,
        page: checked_page(GitPageQuery {
            skip: query.skip,
            limit: query.limit,
        })?,
    };
    Ok(Json(state.agents.call(&agent_id, call).await?))
}

/// Route: `GET /api/v1/agents/{agent}/git/commit-neighbors[/{*path}]?rev=`.
///
/// Separate from the commit diff because the newer neighbour depends on HEAD, which moves,
/// while a commit's diff never changes and is cached for good.
pub(crate) async fn git_commit_neighbors_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    Path(params): Path<AgentFilePath>,
    Query(query): Query<GitRevisionQuery>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<GitCommitNeighborsResponse>, ApiError> {
    let call = GitCommitNeighbors {
        path: route_path(params.path)?,
        rev: checked_revision(query.rev)?,
    };
    Ok(Json(state.agents.call(&agent_id, call).await?))
}
