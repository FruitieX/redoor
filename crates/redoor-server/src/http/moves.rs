//! REST endpoint that starts an asynchronous move between agent filesystem endpoints.

use axum::{Json, extract::State as AxumState};
use redoor_protocol::api::transfers::{MoveFileRequest, MoveFileResponse};

use super::{
    error::ApiError,
    services::transfer::{CopySpec, StartedCopy},
    state::ServerState,
};
use crate::actors::router::CopyOperation;

/// Route: `POST /api/v1/move` starts one atomic-or-copy/delete logical move.
pub(crate) async fn move_file_handler(
    AxumState(state): AxumState<ServerState>,
    Json(payload): Json<MoveFileRequest>,
) -> Result<Json<MoveFileResponse>, ApiError> {
    let StartedCopy { transfer_id, .. } = state
        .transfers
        .start_copy(CopySpec {
            op: CopyOperation::Move,
            source: payload.source,
            dest: payload.dest,
            on_existing: payload.on_existing,
            resume: payload.resume,
            observe_terminal: false,
        })
        .await?;
    Ok(Json(MoveFileResponse {
        move_request_id: transfer_id,
    }))
}
