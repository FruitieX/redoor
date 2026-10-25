//! REST endpoints for git context, status and diffs in agent repositories.

use crate::http::access::ResolvedDevice;
use axum::{
    Json,
    extract::{Path, State as AxumState},
};
use redoor_protocol::api::git::{
    GitContextResponse, GitDiffRequest, GitDiffResponse, GitStatusResponse,
};

use super::{
    agent_client::{GitContext, GitDiff, GitStatus},
    agent_helpers::{AgentFilePath, absolute_path_from_url, require_absolute_path},
    error::ApiError,
    state::ServerState,
};

/// Route: `GET /api/v1/agents/{agent}/git/context[/{*path}]`.
pub(crate) async fn git_context_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    Path(params): Path<AgentFilePath>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<GitContextResponse>, ApiError> {
    let call = GitContext {
        path: route_path(params.path)?,
    };
    Ok(Json(state.agents.call(&agent_id, call).await?))
}

/// Route: `GET /api/v1/agents/{agent}/git/status[/{*path}]`.
pub(crate) async fn git_status_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    Path(params): Path<AgentFilePath>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<GitStatusResponse>, ApiError> {
    let call = GitStatus {
        path: route_path(params.path)?,
    };
    Ok(Json(state.agents.call(&agent_id, call).await?))
}

/// Route: `POST /api/v1/agents/{agent}/git/diff`.
pub(crate) async fn git_diff_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    AxumState(state): AxumState<ServerState>,
    Json(mut request): Json<GitDiffRequest>,
) -> Result<Json<GitDiffResponse>, ApiError> {
    request
        .normalize_files()
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let call = GitDiff {
        files: request.files,
        mode: request.mode,
    };
    Ok(Json(state.agents.call(&agent_id, call).await?))
}

/// Restores the implicit root and applies the shared absolute-path policy.
fn route_path(path: Option<String>) -> Result<String, ApiError> {
    let path = absolute_path_from_url(path.unwrap_or_default());
    require_absolute_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keeps both exact and wildcard routes on the same absolute filesystem convention.
    #[test]
    fn route_paths_restore_the_implicit_root() {
        // The exact route addresses filesystem root rather than an agent cwd.
        assert_eq!(route_path(None).unwrap(), "/");
        // Wildcard segments preserve their hierarchy behind a single route slash.
        assert_eq!(
            route_path(Some("tmp/repo".to_string())).unwrap(),
            "/tmp/repo"
        );
    }
}
