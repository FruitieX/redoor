//! Git context, status, and diff bodies.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Distinguishes filesystem shapes without exposing platform metadata details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum GitEntryType {
    File,
    Directory,
    Symlink,
    Missing,
    Other,
}

/// Describes whether a browser file participates in repository tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum GitTrackingState {
    Tracked,
    Untracked,
    Ignored,
    Deleted,
}

/// Prevents repository-only context from being represented for paths outside a worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "status", rename_all = "snake_case")]
#[ts(tag = "status", rename_all = "snake_case")]
pub enum GitContextResponse {
    /// Reports Git-tab unavailability without duplicating general filesystem metadata.
    OutsideWorktree,
    /// Provides classification and repository paths only when repository discovery succeeded.
    InsideWorktree {
        entry_type: GitEntryType,
        tracking_state: Option<GitTrackingState>,
        repository_root: String,
        repository_relative_path: String,
    },
}

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

/// Identifies entries that cannot be represented by ordinary staged/unstaged states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum GitConflictState {
    Conflicted,
}

/// Identifies the Git object kind without leaking gix mode values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum GitStatusEntryKind {
    File,
    Symlink,
    Submodule,
    Other,
}

/// Describes one changed path from the merged HEAD/index/worktree view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStatusEntry {
    pub path: String,
    pub repository_relative_path: String,
    pub original_path: Option<String>,
    pub index_state: GitChangeState,
    pub worktree_state: GitChangeState,
    pub conflict_state: Option<GitConflictState>,
    pub entry_kind: GitStatusEntryKind,
}

/// Returns bounded, deterministic status for one literal directory prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStatusResponse {
    pub path: String,
    pub repository_root: String,
    pub branch_name: Option<String>,
    pub detached_head_id: Option<String>,
    pub upstream: Option<String>,
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    pub entries: Vec<GitStatusEntry>,
    pub truncated: bool,
    pub omitted_non_utf8_entries: usize,
}

/// Selects the source on the right side of a comparison with HEAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum GitDiffMode {
    Full,
    Staged,
}

/// Selects ordered files and the source compared with HEAD without exposing paths in the URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitDiffRequest {
    pub files: Vec<String>,
    pub mode: GitDiffMode,
}

impl GitDiffRequest {
    /// Bounds batch work and response metadata even when no file produces a text patch.
    pub const MAX_FILES: usize = 128;

    /// Keeps first-occurrence order while preventing repeated paths from amplifying work.
    pub fn normalize_files(&mut self) -> Result<(), &'static str> {
        let mut seen = std::collections::HashSet::new();
        let mut files = Vec::new();
        for path in std::mem::take(&mut self.files) {
            if seen.insert(path.clone()) {
                if files.len() == Self::MAX_FILES {
                    return Err("Git diff requests support at most 128 distinct files");
                }
                files.push(path);
            }
        }
        self.files = files;
        Ok(())
    }
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

impl GitDiffResult {
    /// Produces a payload-free label so command logs never contain file content.
    pub(crate) fn summary(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::NoChanges => "no_changes",
            Self::Untracked => "untracked",
            Self::Ignored => "ignored",
            Self::Binary => "binary",
            Self::TooLarge => "too_large",
            Self::UnsupportedEntry => "unsupported_entry",
        }
    }
}

/// Carries one file's bounded comparison in the same order as the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitFileDiff {
    pub path: String,
    pub result: GitDiffResult,
}

/// Returns ordered per-file Git comparisons without merging their patch payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitDiffResponse {
    pub diffs: Vec<GitFileDiff>,
}
