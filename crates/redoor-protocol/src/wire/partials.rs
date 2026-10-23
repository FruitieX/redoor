//! Durable resume contracts; signed fields remain separate from destination-local metadata.

use crate::{
    api::transfers::{CopyExistingMode, ResumeMode},
    ids::AgentId,
    wire::command::MoveSourceIdentity,
};
use serde::{Deserialize, Serialize};

/// User-authorized endpoints and source identity covered by the server's signature.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PartialAuthorization {
    pub version: u32,
    pub source_agent_id: AgentId,
    pub source_path: String,
    pub source_identity: MoveSourceIdentity,
    pub dest_agent_id: AgentId,
    pub dest_path: String,
    pub operation: PartialOperation,
    pub on_existing: CopyExistingMode,
    pub created_at: u64,
}

/// A move must retain its source until verified destination publication completes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PartialOperation {
    Copy,
    Move,
}

/// Agents preserve this envelope without knowing the signing secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedPartial {
    pub authorization: PartialAuthorization,
    pub signature: String,
}

/// Local file identity prevents resume and cleanup from touching a substituted entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartialManifest {
    pub signed: SignedPartial,
    pub partial_path: String,
    pub partial_device: u64,
    pub partial_inode: u64,
    pub updated_at: u64,
}

/// Discovery returns the actual file length rather than a crash-prone saved byte count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartialRecord {
    pub key: String,
    pub manifest: PartialManifest,
    pub partial_bytes: u64,
}

/// Transfer commands carry authorization even when no previous partial exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartialTransfer {
    pub signed: SignedPartial,
    pub resume: ResumeMode,
    pub offset: u64,
    /// Reopening must refer to the same inode whose prefix was verified.
    #[serde(default)]
    pub expected_partial: Option<PartialIdentity>,
}

/// Cancellation distinguishes intentional deletion from durable pause and transport failure.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    #[default]
    Canceled,
    Paused,
    Disconnected,
}

impl CancelReason {
    /// Default cancel frames stay compatible with v2 readers that omit the disposition.
    pub fn is_canceled(&self) -> bool {
        *self == Self::Canceled
    }
}

/// Binds negotiated resume offsets to one destination inode across asynchronous commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartialIdentity {
    pub device: u64,
    pub inode: u64,
}
