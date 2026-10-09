use crate::actors;
use axum::{Json, extract::State as AxumState};
use redoor_core::log;
use redoor_protocol::{
    api::transfers::{MoveFileRequest, MoveFileResponse},
    wire::log::Level,
};

use super::{
    agent_client::MoveMetadata, agent_helpers::require_absolute_path, error::ApiError,
    state::ServerState,
};

/// Route: `POST /api/v1/move` starts one atomic-or-copy/delete logical move.
pub(crate) async fn move_file_handler(
    AxumState(state): AxumState<ServerState>,
    Json(payload): Json<MoveFileRequest>,
) -> Result<Json<MoveFileResponse>, ApiError> {
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
            MoveMetadata {
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
            "Move supports regular files and directories only",
        ));
    };
    let move_request_id = state
        .router_ref
        .request(|reply| {
            actors::router::RouterMsg::StartCopyRest(actors::router::StartCopyRequest {
                source_agent_id: payload.source.agent.clone(),
                source_path: source_path.clone(),
                dest_agent_id: payload.dest.agent.clone(),
                dest_path: dest_path.clone(),
                total_bytes,
                content_kind,
                on_existing: payload.on_existing,
                operation: actors::router::CopyOperation::Move,
                source_identity: Some(source_metadata.identity),
                terminal: None,
                reply,
            })
        })
        .await??;
    log!(
        Level::Debug,
        "Smart move started: move_request_id={}, source_agent={}, source_path={}, dest_agent={}, dest_path={}, total_bytes={}",
        move_request_id,
        payload.source.agent,
        source_path,
        payload.dest.agent,
        dest_path,
        total_bytes
    );
    Ok(Json(MoveFileResponse { move_request_id }))
}
