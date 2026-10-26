//! Git REST and wire bodies, one submodule per feature area so new Git features get their own
//! module instead of growing a single file. Vocabulary shared by several areas lives here.

pub mod commit;
pub mod revision;
pub mod worktree;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Normalizes index and worktree status into a stable public vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum GitChangeState {
    Unmodified,
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
}

/// Represents every bounded single-file diff outcome explicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type", rename_all = "snake_case")]
pub enum GitDiffResult {
    Text { unified_diff: String },
    NoChanges,
    Untracked,
    Ignored,
    Binary,
    TooLarge,
    UnsupportedEntry,
}
