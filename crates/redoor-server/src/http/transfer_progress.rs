use crate::actors::router::RouterMsg;
use axum::{Json, extract::State as AxumState};
use redoor_protocol::api::transfers::TransferProgressListResponse;

use super::{error::ApiError, state::ServerState};

/// Route: `GET /api/v1/transfers/progress` returns all retained transfer states.
pub(crate) async fn list_transfer_progress_handler(
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<TransferProgressListResponse>, ApiError> {
    Ok(Json(
        state
            .router_ref
            .request(|reply| RouterMsg::GetTransferProgress { reply })
            .await?,
    ))
}
