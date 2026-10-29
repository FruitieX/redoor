//! The main and linked worktrees of a repository (`git worktree list`), with what each has
//! checked out, so the Git view can show where work is happening without visiting every
//! checkout. Only HEAD and the index file are read here, so the list answers quickly; the
//! uncommitted changes of each checkout are counted by `worktree_status` on request.

use std::{
    cmp::Ordering,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::{SystemTime, UNIX_EPOCH},
};

use gix::bstr::ByteSlice;

use redoor_protocol::{
    api::git::worktree_list::{GitWorktree, GitWorktreeListResponse, MAX_GIT_WORKTREES},
    ids::UnixTimestampSeconds,
    wire::request::CommandError,
};

use super::{
    GitFailure, check_interrupt, discover_required_repository, internal_failure,
    revision::commit_summary, run_blocking,
};

/// Lists every checkout of the repository containing `path`.
pub(crate) async fn list(path: String) -> Result<GitWorktreeListResponse, CommandError> {
    run_blocking(move |interrupt| list_blocking(&path, interrupt)).await
}

/// Describes the main worktree and up to [`MAX_GIT_WORKTREES`] linked ones, then orders them.
///
/// Starts from the main repository rather than the discovered one, so the list is the same
/// whichever checkout the request came from.
fn list_blocking(
    path: &str,
    interrupt: Arc<AtomicBool>,
) -> Result<GitWorktreeListResponse, GitFailure> {
    let repository = discover_required_repository(path)?;
    let main = repository
        .repo
        .main_repo()
        .map_err(|error| internal_failure("open the main Git repository", error))?;
    let linked = repository
        .repo
        .worktrees()
        .map_err(|error| internal_failure("list Git worktrees", error))?;
    let truncated = linked.len() > MAX_GIT_WORKTREES;

    let lister = WorktreeLister {
        current_root: &repository.root,
        interrupt,
    };
    let mut worktrees = Vec::new();
    // A bare main repository has no checkout of its own, so only its linked worktrees show.
    if let Some(base) = main.workdir().map(Path::to_path_buf) {
        worktrees.push(lister.describe(&main, base, true, false)?);
    }
    for proxy in linked.into_iter().take(MAX_GIT_WORKTREES) {
        check_interrupt(&lister.interrupt, "Git worktree listing")?;
        // A registration whose `gitdir` file cannot be read names no checkout to show.
        let Ok(base) = proxy.base() else {
            continue;
        };
        let locked = proxy.is_locked();
        let repo = proxy
            .into_repo_with_possibly_inaccessible_worktree()
            .map_err(|error| internal_failure("open Git worktree", error))?;
        worktrees.push(lister.describe(&repo, base, false, locked)?);
    }
    worktrees.sort_by(listing_order);

    Ok(GitWorktreeListResponse {
        repository_root: repository.root.display().to_string(),
        worktrees,
        truncated,
    })
}

/// What describing each worktree needs from the listing, gathered so every checkout is
/// compared with the same requested root and stops on the same interrupt.
struct WorktreeLister<'a> {
    /// The canonical root of the checkout the request came from.
    current_root: &'a Path,
    interrupt: Arc<AtomicBool>,
}

impl WorktreeLister<'_> {
    /// Reads one checkout's HEAD and when its index was last written.
    fn describe(
        &self,
        repo: &gix::Repository,
        base: PathBuf,
        is_main: bool,
        locked: bool,
    ) -> Result<GitWorktree, GitFailure> {
        // Canonical paths make the current checkout comparable with the discovered root and
        // give the browser the same path it would reach by navigating there.
        let root = std::fs::canonicalize(&base)
            .ok()
            .filter(|root| root.is_dir());
        let missing = root.is_none();

        let head = repo
            .head()
            .map_err(|error| internal_failure("read Git worktree HEAD", error))?;
        let branch_name = head.referent_name().and_then(|name| {
            name.shorten()
                .to_str()
                .ok()
                .map(std::string::ToString::to_string)
        });
        let (head_commit, committed_at) = match head.id() {
            Some(id) => {
                let commit = repo
                    .find_commit(id.detach())
                    .map_err(|error| internal_failure("read Git worktree HEAD commit", error))?;
                // Committer time, so a rebased or freshly amended branch counts as recent.
                let committed_at = commit
                    .time()
                    .map_err(|error| internal_failure("read Git commit time", error))?
                    .seconds;
                (Some(commit_summary(&commit)?), Some(committed_at))
            }
            None => (None, None),
        };

        // Git rewrites the index on add, commit, checkout and most status runs, so its time
        // tells when someone last worked in the checkout without walking its files.
        let indexed_at = std::fs::metadata(repo.index_path())
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(unix_seconds);

        Ok(GitWorktree {
            is_current: root.as_deref() == Some(self.current_root),
            path: root.unwrap_or(base).display().to_string(),
            is_main,
            branch_name,
            head_commit,
            last_activity_at: committed_at.max(indexed_at).map(UnixTimestampSeconds::new),
            locked,
            missing,
        })
    }
}

/// Converts a file time to whole seconds since the epoch, dropping times before it.
fn unix_seconds(time: SystemTime) -> Option<i64> {
    let age = time.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(age.as_secs()).ok()
}

/// The main worktree first because every other checkout branches off it, then the most
/// recently active, with missing checkouts last since nobody can be working in them. Ties
/// fall back to the path, which is unique, so the order is stable.
fn listing_order(left: &GitWorktree, right: &GitWorktree) -> Ordering {
    right
        .is_main
        .cmp(&left.is_main)
        .then(left.missing.cmp(&right.missing))
        .then(right.last_activity_at.cmp(&left.last_activity_at))
        .then_with(|| left.path.cmp(&right.path))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a worktree whose only interesting facts are the ones the order reads.
    fn worktree(path: &str, is_main: bool, missing: bool, activity: Option<i64>) -> GitWorktree {
        GitWorktree {
            path: path.to_string(),
            is_main,
            is_current: false,
            branch_name: None,
            head_commit: None,
            last_activity_at: activity.map(UnixTimestampSeconds::new),
            locked: false,
            missing,
        }
    }

    /// Confirms the main worktree leads, the most recent follow, and missing ones trail.
    #[test]
    fn worktrees_sort_main_first_then_most_recent() {
        let mut worktrees = [
            worktree("/old", false, false, Some(1)),
            worktree("/gone", false, true, Some(9)),
            worktree("/main", true, false, Some(0)),
            worktree("/new", false, false, Some(5)),
            worktree("/unknown", false, false, None),
        ];
        worktrees.sort_by(listing_order);
        let paths: Vec<_> = worktrees.iter().map(|w| w.path.as_str()).collect();
        // The oldest main still leads, unknown activity sorts after known activity, and a
        // missing checkout trails even when its last commit was the newest.
        assert_eq!(paths, ["/main", "/new", "/old", "/unknown", "/gone"]);
    }
}
