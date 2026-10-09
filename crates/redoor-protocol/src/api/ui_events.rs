//! Server-sent UI invalidation events.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[ts(rename_all = "snake_case")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEvent {
    /// Agent inventory changed and agent-dependent route data may need reloading.
    AgentsChanged,
    /// Filesystem state changed and the active route may need reloading.
    RoutesChanged,
    /// Transfer progress changed without requiring unrelated route loaders to rerun.
    TransfersChanged,
}
