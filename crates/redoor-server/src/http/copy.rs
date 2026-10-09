//! REST endpoint that starts an asynchronous copy between agents.

use axum::{Json, extract::State as AxumState};
use redoor_protocol::api::transfers::{CopyFileRequest, CopyFileResponse};

use super::{
    error::ApiError,
    services::transfer::{CopySpec, StartedCopy},
    state::ServerState,
};
use crate::actors::router::CopyOperation;

/// Route: `POST /api/v1/copy`
pub(crate) async fn copy_file_handler(
    AxumState(state): AxumState<ServerState>,
    Json(payload): Json<CopyFileRequest>,
) -> Result<Json<CopyFileResponse>, ApiError> {
    let StartedCopy { transfer_id, .. } = state
        .transfers
        .start_copy(CopySpec {
            op: CopyOperation::Copy,
            source: payload.source,
            dest: payload.dest,
            on_existing: payload.on_existing,
            observe_terminal: false,
        })
        .await?;
    Ok(Json(CopyFileResponse {
        copy_request_id: transfer_id,
    }))
}
