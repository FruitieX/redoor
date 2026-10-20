//! Runtime logging-threshold REST bodies.

use crate::wire::log::Level;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Carries a requested runtime threshold without overloading stream protocol data.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LoggingLevelRequest {
    pub level: Level,
}

/// Returns the authoritative threshold after a read or successful update.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LoggingLevelResponse {
    pub level: Level,
}
