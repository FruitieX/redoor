//! REST endpoint that lists the commits which changed a path.

use axum::{
    Json,
    extract::{Path, Query, State as AxumState},
};
use redoor_protocol::api::git::{
    GitPageQuery,
    history::{GitLogQuery, GitLogResponse},
};

use super::{checked_page, checked_revision, route_path};
use crate::http::{
    access::ResolvedDevice, agent_client::GitLog, agent_helpers::AgentFilePath, error::ApiError,
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
