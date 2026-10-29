//! Commit views: what one commit is and what it changed.

use super::{GitChangeState, GitDiffResult};
use crate::ids::UnixTimestampSeconds;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The facts a revision banner (and later history rows) show about one commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitCommitSummary {
    pub id: String,
    /// The first line of the message, which is what Git tools conventionally show.
    pub summary: String,
    pub author_name: String,
    pub author_email: String,
    /// Author time, because that is when the change was made rather than when it was rebased.
    pub authored_at: UnixTimestampSeconds,
    pub parent_ids: Vec<String>,
}

/// One file a commit changed below the requested path, with its bounded diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitCommitFileChange {
    /// The absolute worktree path, so the browser can link to the file at the commit.
    pub path: String,
    pub repository_relative_path: String,
    pub change: GitChangeState,
    pub result: GitDiffResult,
}

/// Shows what one commit changed below a path, compared with its first parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitCommitDiffResponse {
    pub commit: GitCommitSummary,
    /// The message after the summary line, empty when there is none. It lives here rather
    /// than in the summary because history lists repeat summaries and never show bodies.
    pub body: String,
    /// The compared parent; absent for a root commit, whose files all count as added.
    pub parent_id: Option<String>,
    pub repository_root: String,
    pub repository_relative_path: String,
    pub files: Vec<GitCommitFileChange>,
    /// True when the commit changed more files than one response carries.
    pub truncated: bool,
}
