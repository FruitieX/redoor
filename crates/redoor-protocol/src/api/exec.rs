//! Streaming process execution REST bodies.

use crate::ids::RequestId;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// HTTP 202 accepts retained cancellation delivery; only an agent terminal event acknowledges termination.
#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CancelExecutionResponse {
    pub execution_id: RequestId,
}
