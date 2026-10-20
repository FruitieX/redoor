//! Per-account UI state persisted by the server.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Replacement JSON document written through to the login account's state file.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateUserStateRequest {
    /// Uninterpreted JSON that replaces the previous document.
    #[ts(type = "unknown")]
    pub state: serde_json::Value,
}

/// JSON document restored from the login account's state file so the UI can hydrate preferences.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UserStateResponse {
    /// Uninterpreted JSON previously persisted for this account.
    #[ts(type = "unknown")]
    pub state: serde_json::Value,
}
