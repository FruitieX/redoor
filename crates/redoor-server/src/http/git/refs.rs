//! REST endpoints that list a repository's branches and tags.

use axum::{
    Json,
    extract::{Path, Query, State as AxumState},
};
use redoor_protocol::{
    api::git::{
        GitPageQuery,
        refs::{GitRefListKind, GitRefListResponse, GitRefSortQuery},
    },
    ids::AgentId,
};

use super::{checked_page, route_path};
use crate::http::{
    access::ResolvedDevice, agent_client::GitRefList, agent_helpers::AgentFilePath,
    error::ApiError, state::ServerState,
};

/// Route: `GET /api/v1/agents/{agent}/git/branches[/{*path}]?skip=&limit=&sort=recent|name`.
pub(crate) async fn git_branches_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    Path(params): Path<AgentFilePath>,
    Query(query): Query<GitPageQuery>,
    Query(sort): Query<GitRefSortQuery>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<GitRefListResponse>, ApiError> {
    list_refs(
        &state,
        &agent_id,
        params,
        query,
        sort,
        GitRefListKind::Branches,
    )
    .await
}

/// Route: `GET /api/v1/agents/{agent}/git/tags[/{*path}]?skip=&limit=&sort=recent|name`.
pub(crate) async fn git_tags_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    Path(params): Path<AgentFilePath>,
    Query(query): Query<GitPageQuery>,
    Query(sort): Query<GitRefSortQuery>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<GitRefListResponse>, ApiError> {
    list_refs(&state, &agent_id, params, query, sort, GitRefListKind::Tags).await
}

/// Both listings differ only in the kind of ref, so they share the validation and the call.
async fn list_refs(
    state: &ServerState,
    agent_id: &AgentId,
    params: AgentFilePath,
    query: GitPageQuery,
    sort: GitRefSortQuery,
    kind: GitRefListKind,
) -> Result<Json<GitRefListResponse>, ApiError> {
    let call = GitRefList {
        path: route_path(params.path)?,
        kind,
        sort: sort.sort.unwrap_or_default(),
        page: checked_page(query)?,
    };
    Ok(Json(state.agents.call(agent_id, call).await?))
}
