//! REST request and response bodies grouped by domain.

pub mod agents;
pub mod auth;
pub mod diffs;
pub mod exec;
pub mod files;
pub mod git;
pub mod logging;
pub mod mcp;
pub mod search;
pub mod server;
pub mod transfers;
pub mod trash;
pub mod ui_events;
pub mod user_state;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Uniform JSON error body so clients can show failures without parsing per-route shapes.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ErrorResponse {
    pub error: String,
}
