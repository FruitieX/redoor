//! Counts the uncommitted changes of one checkout for its row in the worktree list.
//!
//! Kept out of the list itself because a status walk visits every file of a checkout, so the
//! browser shows the list at once and asks for each checkout's counts separately.

use tokio::sync::Semaphore;

use redoor_protocol::{
    api::git::{
        GitChangeState,
        worktree_status::{GitWorktreeStatusResponse, MAX_GIT_WORKTREE_CHANGES},
    },
    wire::{command::CommandErrorKind, request::CommandError},
};

use super::{
    discover_required_repository, run_blocking,
    status::{StatusCollection, collect_status},
};

/// Status walks allowed to run at once. A browser asks for every row of the list together,
/// and each gix walk already spreads over all cores, so running more of them at once only
/// makes the disk seek between checkouts; the rest wait their turn here.
const WORKTREE_STATUS_SLOTS: usize = 2;

/// Queue shared by every connection, because the disk being walked is shared too.
static WORKTREE_STATUS_PERMITS: Semaphore = Semaphore::const_new(WORKTREE_STATUS_SLOTS);

/// Counts the uncommitted paths of the checkout containing `path`, once a slot is free.
///
/// Waiting for the slot happens before the blocking walk starts, so a dropped request leaves
/// the queue without ever touching the disk.
pub(crate) async fn status(path: String) -> Result<GitWorktreeStatusResponse, CommandError> {
    let _permit = WORKTREE_STATUS_PERMITS.acquire().await.map_err(|_| {
        CommandError::new(
            CommandErrorKind::Internal,
            "Git worktree status queue is unavailable",
        )
    })?;
    run_blocking(move |interrupt| {
        let repository = discover_required_repository(&path)?;
        let StatusCollection {
            changes,
            omitted_non_utf8_entries,
            overflowed,
        } = collect_status(
            &repository.repo,
            |_| true,
            false,
            MAX_GIT_WORKTREE_CHANGES,
            Vec::new(),
            interrupt,
        )?;

        let mut counts = GitWorktreeStatusResponse {
            // Paths that are not UTF-8 are still uncommitted work, so they keep a checkout
            // from looking clean even though they cannot be classified.
            changed: u32::try_from(omitted_non_utf8_entries).unwrap_or(u32::MAX),
            untracked: 0,
            truncated: overflowed,
        };
        for status in changes.values() {
            let untracked = status.index_state == GitChangeState::Unmodified
                && status.worktree_state == GitChangeState::Added
                && status.conflict_state.is_none();
            if untracked {
                counts.untracked += 1;
            } else {
                counts.changed += 1;
            }
        }
        Ok(counts)
    })
    .await
}
