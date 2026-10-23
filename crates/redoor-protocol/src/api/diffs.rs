//! Server-generated diffs between two agent files.

use crate::ids::AgentId;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Identifies one agent-side file participating in a server-generated diff.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DiffEndpoint {
    pub agent: AgentId,
    pub path: String,
}

/// Selects two ordered files without requiring them to share an agent or path.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DiffFilesRequest {
    pub left: DiffEndpoint,
    pub right: DiffEndpoint,
}

/// Returns text in the conventional unified diff representation used by patch tools.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DiffFilesResponse {
    pub unified_diff: String,
}
