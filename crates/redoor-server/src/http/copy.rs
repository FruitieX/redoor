use crate::actors;
use axum::{Json, extract::State as AxumState};
use redoor_protocol::api::transfers::{CopyFileRequest, CopyFileResponse};

use super::{
    agent_client::Metadata, agent_helpers::require_absolute_path, error::ApiError,
    state::ServerState,
};

/// Route: `POST /api/v1/copy`
pub(crate) async fn copy_file_handler(
    AxumState(state): AxumState<ServerState>,
    Json(payload): Json<CopyFileRequest>,
) -> Result<Json<CopyFileResponse>, ApiError> {
    let source_path = require_absolute_path(payload.source.path.clone())?;
    let dest_path = require_absolute_path(payload.dest.path.clone())?;
    if payload.source.agent == payload.dest.agent && source_path == dest_path {
        return Err(ApiError::bad_request(
            "Source and destination must be different",
        ));
    }
    let source_metadata = state
        .agents
        .call(
            &payload.source.agent,
            Metadata {
                path: source_path.clone(),
            },
        )
        .await?;
    let (total_bytes, content_kind) = if source_metadata.is_file {
        (
            source_metadata.file_size,
            actors::router::CopyContentKind::RawFile,
        )
    } else if source_metadata.is_dir {
        (0, actors::router::CopyContentKind::TarDirectory)
    } else {
        return Err(ApiError::bad_request(
            "Copy supports regular files and directories only",
        ));
    };
    let copy_request_id = state
        .router_ref
        .request(|reply| {
            actors::router::RouterMsg::StartCopyRest(actors::router::StartCopyRequest {
                source_agent_id: payload.source.agent.clone(),
                source_path,
                dest_agent_id: payload.dest.agent.clone(),
                dest_path,
                total_bytes,
                content_kind,
                on_existing: payload.on_existing,
                operation: actors::router::CopyOperation::Copy,
                source_identity: None,
                terminal: None,
                reply,
            })
        })
        .await??;
    Ok(Json(CopyFileResponse { copy_request_id }))
}
