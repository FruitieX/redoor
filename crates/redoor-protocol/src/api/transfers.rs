//! Copy, move, upload, and transfer-progress bodies.

use crate::ids::{AgentId, TransferId, UnixTimestampSeconds};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One side of a copy or move; carries the agent so transfers can cross agent boundaries.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CopyEndpoint {
    pub agent: AgentId,
    pub path: String,
}

/// Controls what copy does when the destination path already exists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum CopyExistingMode {
    /// Fail the copy and leave the destination untouched.
    #[default]
    Error,
    /// Replace the destination path entirely with the source contents.
    Override,
    /// Merge source contents into the destination tree; conflicting files are replaced.
    Merge,
}

impl CopyExistingMode {
    /// Default used by direct raw uploads so existing HTTP clients keep overwriting files.
    pub const fn override_mode() -> Self {
        Self::Override
    }
}

/// Starts one asynchronous copy between agent filesystem endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CopyFileRequest {
    pub source: CopyEndpoint,
    pub dest: CopyEndpoint,
    /// What to do when `dest.path` already exists. Omitted requests keep the historical error behavior.
    #[serde(default)]
    pub on_existing: CopyExistingMode,
}

/// Returns the public id used to follow asynchronous copy progress.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CopyFileResponse {
    pub copy_request_id: TransferId,
}

/// Confirms the extracted directory was published, rather than merely accepting its HTTP body.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ArchiveUploadResponse {
    pub path: String,
    #[ts(type = "number")]
    pub bytes_written: u64,
}

/// Correlates cancellation with a caller-owned upload without scanning another client's transfer paths.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CancelUploadRequestResponse {
    pub request_token: String,
    /// None means cancellation was accepted while destination setup was still pending.
    pub transfer: Option<CancelTransferResponse>,
}

/// Starts one logical move between agent filesystem endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MoveFileRequest {
    pub source: CopyEndpoint,
    pub dest: CopyEndpoint,
    /// What to do when `dest.path` already exists.
    #[serde(default)]
    pub on_existing: CopyExistingMode,
}

/// Returns the public id used to follow asynchronous move progress.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MoveFileResponse {
    pub move_request_id: TransferId,
}

/// Snapshot of all tracked transfers so the UI can render one combined progress list.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TransferProgressListResponse {
    pub transfers: Vec<TransferProgressEntry>,
}

/// Byte progress for one transfer, exported so TypeScript consumers share the shape.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TransferProgressUpdate {
    pub agent_id: AgentId,
    pub request_id: TransferId,
    #[ts(type = "number")]
    pub transferred_bytes: u64,
    #[ts(type = "number")]
    pub total_bytes: u64,
}

/// Public view of one upload, download, edit, copy or move, including its terminal state,
/// so finished transfers stay visible after the HTTP request that started them ends.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TransferProgressEntry {
    pub request_id: TransferId,
    pub agent_id: AgentId,
    pub path: String,
    pub source: Option<CopyEndpoint>,
    pub dest: Option<CopyEndpoint>,
    pub direction: TransferDirection,
    #[ts(type = "number")]
    pub total_bytes: u64,
    #[ts(type = "number")]
    pub transferred_bytes: u64,
    pub started_at: UnixTimestampSeconds,
    pub ended_at: Option<UnixTimestampSeconds>,
    pub state: TransferProgressState,
    /// Tells clients whether the active operation is still before its non-cancelable commit boundary.
    pub cancelable: bool,
    pub error: Option<String>,
    /// Stamped only after a same-agent filesystem rename so the UI can skip copy speeds.
    pub atomic: bool,
}

/// Kind of transfer, so clients can label entries and pick the right progress semantics.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[ts(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Upload,
    Edit,
    Download,
    Copy,
    Move,
}

/// Lifecycle of a transfer; `Canceling` is separate so clients know cancellation was
/// accepted but the agent has not yet confirmed cleanup.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[ts(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TransferProgressState {
    Active,
    Canceling,
    Canceled,
    Errored,
    Completed,
}

/// Describes the idempotent result of asking the router to cancel one public transfer.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[ts(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum CancelTransferStatus {
    Accepted,
    AlreadyCanceling,
    AlreadyCanceled,
}

/// Returns the public transfer and its cancellation lifecycle result.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CancelTransferResponse {
    pub transfer_id: TransferId,
    pub status: CancelTransferStatus,
}
