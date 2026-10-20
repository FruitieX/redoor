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
    /// Auto reuses verified partials; restart deliberately begins from zero.
    #[serde(default)]
    pub resume: ResumeMode,
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
    /// Auto reuses verified partials; restart deliberately begins from zero.
    #[serde(default)]
    pub resume: ResumeMode,
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
    /// Only signed regular-file staging supports pause.
    #[serde(default)]
    pub pausable: bool,
    /// Prefix hashing remains an active transfer with a distinct visible phase.
    #[serde(default)]
    pub verifying: bool,
    /// Speeds count only bytes transferred in this attempt.
    #[serde(default)]
    #[ts(type = "number")]
    pub resumed_bytes: u64,
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

/// Explicit restart preserves the user's ability to reject an existing partial.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ResumeMode {
    #[default]
    Auto,
    Restart,
}

/// Preflight inspects candidates without reading the potentially large prefix.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CopyPreflightRequest {
    pub source: CopyEndpoint,
    pub dest: CopyEndpoint,
}

/// Cheap candidate information shown before the user starts expensive verification.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ResumeCandidate {
    #[ts(type = "number")]
    pub partial_bytes: u64,
    #[ts(type = "number")]
    pub total_bytes: u64,
    #[ts(type = "number")]
    pub updated_at: u64,
}

/// A null candidate also covers stale and unverifiable manifests.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CopyPreflightResponse {
    pub resume_candidate: Option<ResumeCandidate>,
}

/// The client selects only an opaque agent-local key, never trusted source endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PartialActionRequest {
    pub dest_agent_id: AgentId,
    pub key: String,
}

/// Public inventory omits signatures and private filesystem identities.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ResumableTransfer {
    pub dest_agent_id: AgentId,
    pub key: String,
    pub source: CopyEndpoint,
    pub dest: CopyEndpoint,
    pub operation: TransferDirection,
    #[ts(type = "number")]
    pub partial_bytes: u64,
    #[ts(type = "number")]
    pub total_bytes: u64,
    #[ts(type = "number")]
    pub updated_at: u64,
}

/// Inventory is rebuilt from connected agents after every restart.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PartialsResponse {
    pub partials: Vec<ResumableTransfer>,
}

/// Resume always creates a new public transfer id.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ResumePartialResponse {
    pub transfer_id: TransferId,
}

/// Discard confirms agent-side cleanup completed.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DiscardPartialResponse {
    pub discarded: bool,
}

/// Pause is acknowledged only after durable destination storage.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PauseTransferResponse {
    pub transfer_id: TransferId,
}
