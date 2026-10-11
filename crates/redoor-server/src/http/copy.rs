//! REST endpoint that starts an asynchronous copy between agents.

use axum::{Extension, Json, extract::State as AxumState};
use redoor_protocol::api::{
    tokens::Permission,
    transfers::{CopyFileRequest, CopyFileResponse},
};

use super::{
    access::Principal,
    error::ApiError,
    services::transfer::{CopySpec, StartedCopy},
    state::ServerState,
};
use crate::actors::router::CopyOperation;

/// Route: `POST /api/v1/copy`; needs read on the source and write on the destination.
pub(crate) async fn copy_file_handler(
    AxumState(state): AxumState<ServerState>,
    Extension(principal): Extension<Principal>,
    Json(payload): Json<CopyFileRequest>,
) -> Result<Json<CopyFileResponse>, ApiError> {
    principal.require(&[Permission::FilesRead], &payload.source.agent)?;
    principal.require(&[Permission::FilesWrite], &payload.dest.agent)?;
    let StartedCopy { transfer_id, .. } = state
        .transfers
        .start_copy(CopySpec {
            op: CopyOperation::Copy,
            source: payload.source,
            dest: payload.dest,
            on_existing: payload.on_existing,
            resume: payload.resume,
            observe_terminal: false,
        })
        .await?;
    Ok(Json(CopyFileResponse {
        copy_request_id: transfer_id,
    }))
}
