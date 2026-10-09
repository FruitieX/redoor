//! REST endpoints for delete, mkdir and rename on agent filesystems.

use axum::{
    Json,
    extract::{Path, Query, State as AxumState},
    http::StatusCode,
};
use redoor_protocol::{
    api::files::{
        CreateDirectoryResponse, CreationOwnershipOptions, RawDeleteResponse, RenamePathRequest,
        RenamePathResponse,
    },
    ids::AgentId,
};
use serde::Deserialize;
use std::path::Path as FilePath;

use super::{
    agent_client::{CreateDirectory, DeletePath, RenamePath},
    agent_helpers::{AgentFilePath, absolute_path_from_url, require_absolute_path},
    error::ApiError,
    state::ServerState,
    trash::require_move_to_trash_support,
};

/// Selects permanent deletion by default so existing callers remain destructive.
#[derive(Deserialize)]
pub(crate) struct RawDeleteQuery {
    #[serde(default)]
    trash: bool,
}

/// Route: `DELETE /api/v1/agents/{agent}/raw/{*path}`
pub(crate) async fn raw_agent_delete_handler(
    Path(AgentFilePath { agent, path }): Path<AgentFilePath>,
    Query(query): Query<RawDeleteQuery>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<RawDeleteResponse>, ApiError> {
    let agent_id = AgentId::from(agent);
    let path = absolute_path_from_url(path.unwrap_or_default());
    if query.trash {
        require_move_to_trash_support(&state, &agent_id).await?;
    }
    let call = DeletePath {
        path: path.clone(),
        to_trash: query.trash,
    };
    state.agents.call(&agent_id, call).await?;
    Ok(Json(RawDeleteResponse { path }))
}

/// Route: `POST /api/v1/agents/{agent}/mkdir/{*path}`
pub(crate) async fn create_directory_handler(
    Path(AgentFilePath { agent, path }): Path<AgentFilePath>,
    Query(ownership): Query<CreationOwnershipOptions>,
    AxumState(state): AxumState<ServerState>,
) -> Result<(StatusCode, Json<CreateDirectoryResponse>), ApiError> {
    let path = absolute_path_from_url(path.unwrap_or_default());
    ownership.validate().map_err(ApiError::bad_request)?;
    let call = CreateDirectory {
        path: path.clone(),
        ownership,
    };
    state.agents.call(&AgentId::from(agent), call).await?;
    Ok((StatusCode::CREATED, Json(CreateDirectoryResponse { path })))
}

/// Route: `POST /api/v1/agents/{agent}/rename`
pub(crate) async fn rename_path_handler(
    Path(agent): Path<String>,
    AxumState(state): AxumState<ServerState>,
    Json(request): Json<RenamePathRequest>,
) -> Result<Json<RenamePathResponse>, ApiError> {
    let dir = require_absolute_path(request.dir)?;
    if !is_filename(&request.old) || !is_filename(&request.new) {
        return Err(ApiError::bad_request(
            "Rename names must each be a single filename",
        ));
    }
    let source_path = FilePath::new(&dir)
        .join(&request.old)
        .to_string_lossy()
        .into_owned();
    let dest_path = FilePath::new(&dir)
        .join(&request.new)
        .to_string_lossy()
        .into_owned();
    let call = RenamePath {
        dir,
        old: request.old,
        new: request.new,
    };
    state.agents.call(&AgentId::from(agent), call).await?;
    Ok(Json(RenamePathResponse {
        source_path,
        dest_path,
    }))
}

/// Rejects path syntax so the REST boundary only accepts entry names within `dir`.
fn is_filename(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && name != "."
        && name != ".."
        && FilePath::new(name)
            .file_name()
            .is_some_and(|part| part == name)
}
