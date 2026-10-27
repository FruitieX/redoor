//! Worktree list: the main and linked checkouts (`git worktree list`) of one repository.
//!
//! Separate from [`super::worktree`], which describes the files of one checkout, because this
//! list spans every checkout that shares the repository.

use super::commit::GitCommitSummary;
use crate::ids::UnixTimestampSeconds;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Most worktrees one listing returns, so a repository with runaway automation cannot grow the
/// reply or the time spent walking each checkout's status without bound.
pub const MAX_GIT_WORKTREES: usize = 200;

/// Most changed paths counted per worktree. The counts only tell a dirty checkout from a
/// clean one at a glance, so a huge untracked directory need not be counted to the end.
pub const MAX_GIT_WORKTREE_CHANGES: usize = 1_000;

/// How many uncommitted paths a checkout has, split the way people decide what to do next:
/// tracked edits to commit or stash, and new files that `git stash` would leave behind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitWorktreeChanges {
    /// Tracked paths that differ from HEAD in the index or on disk, including conflicts.
    pub changed: u32,
    /// Files Git does not track and does not ignore.
    pub untracked: u32,
    /// True when the checkout had more changed paths than were counted, so the counts are
    /// lower bounds.
    pub truncated: bool,
}

/// One checkout of the repository and what it has checked out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitWorktree {
    /// The checkout's absolute directory, which the browser opens.
    pub path: String,
    /// True for the checkout that owns the repository, which `git worktree remove` cannot
    /// remove and which the list always shows first.
    pub is_main: bool,
    /// True for the checkout the requested path is in, so the list shows where the user is.
    pub is_current: bool,
    /// The short name of the checked-out branch, absent when HEAD is detached.
    pub branch_name: Option<String>,
    /// The commit HEAD points at, absent while the branch has no commits yet.
    pub head_commit: Option<GitCommitSummary>,
    /// Uncommitted paths, absent when the checkout directory is missing and cannot be read.
    pub changes: Option<GitWorktreeChanges>,
    /// The newest of the HEAD commit's committer time and the modification times of the
    /// changed files, so a checkout with fresh uncommitted work counts as recently used even
    /// on an old branch. Absent when neither is known.
    pub last_activity_at: Option<UnixTimestampSeconds>,
    /// True when `git worktree lock` protects the checkout from pruning.
    pub locked: bool,
    /// True when the registered directory no longer exists, so `git worktree prune` would
    /// remove the registration.
    pub missing: bool,
}

/// Every checkout of the repository containing the requested path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitWorktreeListResponse {
    pub repository_root: String,
    /// The main worktree first, then the most recently active, with missing checkouts last.
    pub worktrees: Vec<GitWorktree>,
    /// True when the repository has more linked worktrees than one listing returns.
    pub truncated: bool,
}
