//! Platform trash inventory and restore bodies.

use crate::ids::UnixTimestampSeconds;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Groups currently discoverable trash entries by their provider-selected storage location.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TrashListResponse {
    pub locations: Vec<TrashLocation>,
}

/// Reports how many payload entries were permanently removed from all trash locations.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EmptyTrashResponse {
    #[ts(type = "number")]
    pub deleted_items: u64,
}

/// Identifies one trash storage root without exposing it as a filesystem command argument.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TrashLocation {
    pub id: String,
    pub path: String,
    pub items: Vec<TrashItem>,
}

/// Describes one restorable payload while tolerating unavailable original-path metadata.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TrashItem {
    pub id: String,
    pub name: String,
    pub original_path: Option<String>,
    pub deleted_at: UnixTimestampSeconds,
}

/// Selects one item through opaque identifiers and supplies its restore destination.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RestoreTrashItemRequest {
    pub location_id: String,
    pub item_id: String,
    pub destination_path: String,
}

/// Returns the selected destination after a successful atomic restore.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RestoreTrashItemResponse {
    pub path: String,
}
