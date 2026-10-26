//! History views: the commits that changed a path, newest first.

use super::commit::GitCommitSummary;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Selects where a log starts and which page of it to return. Without `rev` the log starts at
/// HEAD, which is what the worktree Git view shows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitLogQuery {
    pub rev: Option<String>,
    pub skip: Option<u32>,
    pub limit: Option<u32>,
}

/// One page of the commits that changed a path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitLogResponse {
    pub repository_root: String,
    /// The path the log is limited to, empty when it covers the whole repository.
    pub repository_relative_path: String,
    /// Newest first by commit time; empty when HEAD has no commits yet.
    pub commits: Vec<GitCommitSummary>,
    /// True when more matching commits follow this page.
    pub has_more: bool,
}

/// The commits on either side of one commit in its path's history, so a commit view can step
/// to the previous or next change of the browsed path without going back to the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitCommitNeighborsResponse {
    /// The newest of the commit's ancestors that changed the path; absent at the path's
    /// first commit.
    pub older: Option<GitCommitSummary>,
    /// The oldest commit between it and HEAD that changed the path. Only HEAD knows which way
    /// is forward, because Git records parents and never children, so this is absent at HEAD
    /// and for commits HEAD does not contain.
    pub newer: Option<GitCommitSummary>,
}
