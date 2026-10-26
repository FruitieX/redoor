//! REST endpoints that browse committed trees and file contents at a revision.

use axum::{
    Json,
    extract::{Path, Query, State as AxumState},
};
use redoor_protocol::api::git::revision::{
    GitRevisionContentResponse, GitRevisionQuery, GitRevisionTreeResponse,
};

use super::{checked_revision, route_path};
use crate::http::{
    access::ResolvedDevice,
    agent_client::{GitRevisionContent, GitRevisionTree},
    agent_helpers::AgentFilePath,
    error::ApiError,
    state::ServerState,
};

/// Route: `GET /api/v1/agents/{agent}/git/tree[/{*path}]?rev=`.
pub(crate) async fn git_revision_tree_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    Path(params): Path<AgentFilePath>,
    Query(query): Query<GitRevisionQuery>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<GitRevisionTreeResponse>, ApiError> {
    let call = GitRevisionTree {
        path: route_path(params.path)?,
        rev: checked_revision(query.rev)?,
    };
    Ok(Json(state.agents.call(&agent_id, call).await?))
}

/// Route: `GET /api/v1/agents/{agent}/git/blob[/{*path}]?rev=`.
pub(crate) async fn git_revision_content_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    Path(params): Path<AgentFilePath>,
    Query(query): Query<GitRevisionQuery>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<GitRevisionContentResponse>, ApiError> {
    let call = GitRevisionContent {
        path: route_path(params.path)?,
        rev: checked_revision(query.rev)?,
    };
    Ok(Json(state.agents.call(&agent_id, call).await?))
}
