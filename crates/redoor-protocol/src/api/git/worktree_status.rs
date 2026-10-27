//! Worktree status summary: how much uncommitted work one checkout holds.
//!
//! Separate from the worktree list because counting changes walks the whole checkout, while
//! the list only reads each checkout's HEAD. The browser shows the list at once and fills in
//! each row's summary as its own walk finishes.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Most changed paths counted per worktree. The counts only tell a dirty checkout from a
/// clean one at a glance, so a huge untracked directory need not be counted to the end.
pub const MAX_GIT_WORKTREE_CHANGES: usize = 1_000;

/// How many uncommitted paths a checkout has, split the way people decide what to do next:
/// tracked edits to commit or stash, and new files that `git stash` would leave behind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitWorktreeStatusResponse {
    /// Tracked paths that differ from HEAD in the index or on disk, including conflicts.
    pub changed: u32,
    /// Files Git does not track and does not ignore.
    pub untracked: u32,
    /// True when the checkout had more changed paths than were counted, so the counts are
    /// lower bounds.
    pub truncated: bool,
}
