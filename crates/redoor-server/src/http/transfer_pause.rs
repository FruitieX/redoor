//! Pause waits for destination durability before returning control to the browser.

use super::{error::ApiError, state::ServerState};
use crate::actors::router::RouterMsg;
use axum::{
    Json,
    extract::{Path, State},
};
use redoor_protocol::{api::transfers::PauseTransferResponse, ids::TransferId};

/// The router removes the transfer only after its destination acknowledges durable storage.
pub(crate) async fn pause(
    State(state): State<ServerState>,
    Path(id): Path<u64>,
) -> Result<Json<PauseTransferResponse>, ApiError> {
    let transfer_id = TransferId::new(id);
    state
        .router_ref
        .request(|reply| RouterMsg::PauseTransfer { transfer_id, reply })
        .await?
        .map_err(ApiError::conflict)?;
    Ok(Json(PauseTransferResponse { transfer_id }))
}
