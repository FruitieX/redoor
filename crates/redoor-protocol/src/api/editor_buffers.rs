//! Per-account editor buffers of open files, persisted by the server so unsaved edits, undo
//! history and the caret survive page reloads and follow the user to other browsers.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Largest accepted buffer payload. Editable files are at most 2 MiB, and a buffer holds the
/// document plus its bounded undo history, so this leaves room without letting one request
/// exhaust server memory or disk.
pub const MAX_EDITOR_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// One stored buffer. `data` is opaque to the server: only the editor interprets it.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EditorBuffer {
    /// Server-assigned on every write and always growing, so a browser can tell a buffer
    /// written elsewhere from the one it wrote itself.
    #[ts(type = "number")]
    pub version: u64,
    /// Uninterpreted editor state written by the browser.
    #[ts(type = "unknown")]
    pub data: serde_json::Value,
}

/// Lookup result; `buffer` is null when nothing is stored, which is the normal case for a
/// file that was never edited, so it is not reported as an error.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EditorBufferResponse {
    pub key: String,
    pub buffer: Option<EditorBuffer>,
}

/// Replaces one buffer. The last write wins: buffers belong to one user, who works from one
/// computer at a time.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PutEditorBufferRequest {
    /// Uninterpreted editor state to store.
    #[ts(type = "unknown")]
    pub data: serde_json::Value,
}

/// The version a write created, which the writer remembers so it does not adopt its own write.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PutEditorBufferResponse {
    pub key: String,
    #[ts(type = "number")]
    pub version: u64,
}

/// Result of closing a file's buffer; `deleted` is false when nothing was stored.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DeleteEditorBufferResponse {
    pub key: String,
    pub deleted: bool,
}
