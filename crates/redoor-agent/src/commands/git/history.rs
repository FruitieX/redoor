//! The commits that changed a path, newest first, one page at a time, and the commits on
//! either side of one of them.

use std::{collections::HashMap, ops::ControlFlow, sync::atomic::AtomicBool};

use redoor_protocol::{
    api::git::{
        GitPage,
        commit::GitCommitSummary,
        history::{GitCommitNeighborsResponse, GitLogResponse},
    },
    wire::{command::CommandErrorKind, request::CommandError},
};

use super::{
    GitFailure, RepositoryPath, check_interrupt, discover_required_repository, internal_failure,
    revision::{RevisionPath, commit_summary, tree_entry},
    run_blocking, utf8_relative,
};

/// Object cache for the walk. Path filtering reads every visited commit and the trees along
/// the path, often twice (as a commit and as a parent), so a small cache saves most of the
/// inflating without growing much on memory-constrained agents.
const WALK_OBJECT_CACHE_BYTES: usize = 4 * 1024 * 1024;

/// What a path is in one commit: its mode and object, or nothing when the commit lacks it.
/// Comparing these finds the commits that changed the path, including mode-only changes.
type PathState = Option<(gix::objs::tree::EntryMode, gix::ObjectId)>;

/// Lists one page of the commits reachable from `rev` (or HEAD) that changed `path`.
pub(crate) async fn log(
    path: String,
    rev: Option<String>,
    page: GitPage,
) -> Result<GitLogResponse, CommandError> {
    run_blocking(move |interrupt| log_blocking(&path, rev.as_deref(), page, &interrupt)).await
}

/// Resolves the starting commit, then walks history until the page is full.
fn log_blocking(
    path: &str,
    rev: Option<&str>,
    page: GitPage,
    interrupt: &AtomicBool,
) -> Result<GitLogResponse, GitFailure> {
    page.validate()
        .map_err(|message| GitFailure::new(CommandErrorKind::InvalidInput, message))?;
    let (mut repository, relative, start) = match rev {
        Some(rev) => {
            let target = RevisionPath::open(path, rev)?;
            (target.repository, target.relative, Some(target.commit_id))
        }
        None => {
            let repository = discover_required_repository(path)?;
            let relative = utf8_relative(&repository.relative)?;
            // An unborn branch has no history yet, which is an empty log rather than an error.
            let head = repository
                .repo
                .head()
                .map_err(|error| internal_failure("read repository HEAD", error))?
                .id()
                .map(gix::Id::detach);
            (repository, relative, head)
        }
    };
    repository
        .repo
        .object_cache_size_if_unset(WALK_OBJECT_CACHE_BYTES);
    let (commits, has_more) = match start {
        Some(start) => HistoryWalk::new(&repository, &relative).page(start, page, interrupt)?,
        None => (Vec::new(), false),
    };
    Ok(GitLogResponse {
        repository_root: repository.root.display().to_string(),
        repository_relative_path: relative,
        commits,
        has_more,
    })
}

/// Finds the commits before and after the one `rev` names in the history of `path`.
pub(crate) async fn neighbors(
    path: String,
    rev: String,
) -> Result<GitCommitNeighborsResponse, CommandError> {
    run_blocking(move |interrupt| neighbors_blocking(&path, &rev, &interrupt)).await
}

/// Resolves the commit, then walks its ancestors for the older neighbour and HEAD's history
/// for the newer one.
fn neighbors_blocking(
    path: &str,
    rev: &str,
    interrupt: &AtomicBool,
) -> Result<GitCommitNeighborsResponse, GitFailure> {
    let mut target = RevisionPath::open(path, rev)?;
    target
        .repository
        .repo
        .object_cache_size_if_unset(WALK_OBJECT_CACHE_BYTES);
    let mut walk = HistoryWalk::new(&target.repository, &target.relative);
    let older = walk.older(target.commit_id, interrupt)?;
    let newer = walk.newer(target.commit_id, interrupt)?;
    let summary = |id: Option<gix::ObjectId>| {
        id.map(|id| {
            target
                .repository
                .repo
                .find_commit(id)
                .map_err(|error| internal_failure("read Git commit", error))
                .and_then(|commit| commit_summary(&commit))
        })
        .transpose()
    };
    Ok(GitCommitNeighborsResponse {
        older: summary(older)?,
        newer: summary(newer)?,
    })
}

/// Walks a repository's history and decides which commits changed one path.
///
/// A commit counts when the path differs from every parent, the rule `git log --full-history`
/// applies before merge simplification: an ordinary commit that touched the path is listed,
/// and a merge is listed only when it resolved the path into something neither side had.
/// Unlike plain `git log` the side branches of merges are not pruned, so commits whose
/// change a merge discarded still appear; that keeps every page a plain filter of one walk.
struct HistoryWalk<'a> {
    repository: &'a RepositoryPath,
    relative: &'a str,
    /// Path states computed for parents before the walk reaches them. An entry is removed
    /// when its commit is visited, so the map holds roughly the walk's frontier, not history.
    pending: HashMap<gix::ObjectId, PathState>,
}

impl<'a> HistoryWalk<'a> {
    /// Prepares a walk limited to `relative`; the empty path lists every commit.
    fn new(repository: &'a RepositoryPath, relative: &'a str) -> Self {
        Self {
            repository,
            relative,
            pending: HashMap::new(),
        }
    }

    /// Returns the page of matching commits and whether more follow it.
    ///
    /// Pages are offsets into one deterministic walk, so each request walks again from the
    /// start like `git log --skip`. That costs time on deep pages but keeps the agent
    /// stateless between requests, and the walk stops as soon as the request is dropped.
    fn page(
        &mut self,
        start: gix::ObjectId,
        page: GitPage,
        interrupt: &AtomicBool,
    ) -> Result<(Vec<GitCommitSummary>, bool), GitFailure> {
        let repo = &self.repository.repo;
        let mut skipped = 0;
        let mut commits = Vec::new();
        let full = self.walk(start, interrupt, |info, changed| {
            if !changed {
                return Ok(ControlFlow::Continue(()));
            }
            if skipped < page.skip {
                skipped += 1;
                return Ok(ControlFlow::Continue(()));
            }
            if commits.len() == page.limit as usize {
                return Ok(ControlFlow::Break(()));
            }
            let commit = repo
                .find_commit(info.id)
                .map_err(|error| internal_failure("read Git commit", error))?;
            commits.push(commit_summary(&commit)?);
            Ok(ControlFlow::Continue(()))
        })?;
        Ok((commits, full.is_some()))
    }

    /// The newest ancestor of `id` that changed the path: the change a reader of `git log`
    /// sees right below `id`, as long as no side branch merged later interleaves with it.
    fn older(
        &mut self,
        id: gix::ObjectId,
        interrupt: &AtomicBool,
    ) -> Result<Option<gix::ObjectId>, GitFailure> {
        self.walk(id, interrupt, |info, changed| {
            Ok(if changed && info.id != id {
                ControlFlow::Break(info.id)
            } else {
                ControlFlow::Continue(())
            })
        })
    }

    /// The oldest descendant of `id` on the way to HEAD that changed the path, the inverse
    /// of [`Self::older`].
    ///
    /// Git stores only parents, so the commits after `id` are found by walking from HEAD
    /// down to it and remembering the matching ones. Commits merged in from a side branch
    /// can be newer without descending from `id`, so the remembered commits are checked
    /// oldest first and the first one that contains `id` wins; in a linear history that is
    /// the very first one checked.
    fn newer(
        &mut self,
        id: gix::ObjectId,
        interrupt: &AtomicBool,
    ) -> Result<Option<gix::ObjectId>, GitFailure> {
        let repo = &self.repository.repo;
        // An unborn branch has no HEAD commit, so nothing is newer.
        let Some(head) = repo
            .head()
            .map_err(|error| internal_failure("read repository HEAD", error))?
            .id()
            .map(gix::Id::detach)
        else {
            return Ok(None);
        };
        if head == id {
            return Ok(None);
        }
        let time = commit_time(repo, id)?;
        // Ids only, so even the history above an old commit costs little memory.
        let mut later = Vec::new();
        let found = self.walk(head, interrupt, |info, changed| {
            if info.id == id {
                return Ok(ControlFlow::Break(true));
            }
            // The walk is ordered by commit time, so once it is older than `id` it has
            // passed the point where `id` would have been visited: HEAD does not contain it.
            if info.commit_time.is_some_and(|visited| visited < time) {
                return Ok(ControlFlow::Break(false));
            }
            if changed {
                later.push(info.id);
            }
            Ok(ControlFlow::Continue(()))
        })?;
        if found != Some(true) {
            return Ok(None);
        }
        for candidate in later.into_iter().rev() {
            if self.contains(candidate, id, time, interrupt)? {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    /// True when `ancestor`, committed at `ancestor_time`, is in the history of `start`.
    /// The walk gives up once it is older than the ancestor, which bounds it to the commits
    /// in between.
    fn contains(
        &self,
        start: gix::ObjectId,
        ancestor: gix::ObjectId,
        ancestor_time: gix::date::SecondsSinceUnixEpoch,
        interrupt: &AtomicBool,
    ) -> Result<bool, GitFailure> {
        for info in self.time_ordered_walk(start)? {
            check_interrupt(interrupt, "Git ancestry walk")?;
            let info = info.map_err(|error| internal_failure("walk Git history", error))?;
            if info.id == ancestor {
                return Ok(true);
            }
            if info.commit_time.is_some_and(|time| time < ancestor_time) {
                return Ok(false);
            }
        }
        Ok(false)
    }

    /// Walks every commit reachable from `start`, newest first, and hands each to `visit`
    /// with whether it changed the path, until `visit` breaks with a value.
    fn walk<B>(
        &mut self,
        start: gix::ObjectId,
        interrupt: &AtomicBool,
        mut visit: impl FnMut(
            &gix::revision::walk::Info<'_>,
            bool,
        ) -> Result<ControlFlow<B>, GitFailure>,
    ) -> Result<Option<B>, GitFailure> {
        for info in self.time_ordered_walk(start)? {
            check_interrupt(interrupt, "Git history walk")?;
            let info = info.map_err(|error| internal_failure("walk Git history", error))?;
            let changed = self.changed_path(info.id, &info.parent_ids)?;
            if let ControlFlow::Break(value) = visit(&info, changed)? {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    /// Starts a walk newest first by commit time, the order people expect from `git log`.
    fn time_ordered_walk(
        &self,
        start: gix::ObjectId,
    ) -> Result<gix::revision::Walk<'a>, GitFailure> {
        self.repository
            .repo
            .rev_walk([start])
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
            ))
            .all()
            .map_err(|error| internal_failure("start Git history walk", error))
    }

    /// True when `id` changed the path compared with all of its parents, or added it as a
    /// root commit.
    fn changed_path(
        &mut self,
        id: gix::ObjectId,
        parent_ids: &[gix::ObjectId],
    ) -> Result<bool, GitFailure> {
        if self.relative.is_empty() {
            return Ok(true);
        }
        let own = match self.pending.remove(&id) {
            Some(state) => state,
            None => self.path_state(id)?,
        };
        // A shallow clone's oldest commits name parents it never fetched. Like Git, such a
        // commit is compared as if it were a root, which lists it when it has the path.
        let repo = &self.repository.repo;
        let parent_ids: Vec<_> = parent_ids
            .iter()
            .filter(|parent| repo.has_object(parent))
            .collect();
        if parent_ids.is_empty() {
            return Ok(own.is_some());
        }
        let mut changed = true;
        for parent in parent_ids {
            let state = match self.pending.get(parent) {
                Some(state) => *state,
                None => {
                    let state = self.path_state(*parent)?;
                    self.pending.insert(*parent, state);
                    state
                }
            };
            changed &= state != own;
        }
        Ok(changed)
    }

    /// Reads what the path is in one commit's tree.
    fn path_state(&self, id: gix::ObjectId) -> Result<PathState, GitFailure> {
        let repo = &self.repository.repo;
        let tree = repo
            .find_commit(id)
            .and_then(|commit| commit.tree())
            .map_err(|error| internal_failure("read Git commit tree", error))?;
        Ok(tree_entry(&tree, self.relative)?.map(|entry| (entry.mode, entry.oid)))
    }
}

/// Reads when a commit was committed, the time the history walks are ordered by.
fn commit_time(
    repo: &gix::Repository,
    id: gix::ObjectId,
) -> Result<gix::date::SecondsSinceUnixEpoch, GitFailure> {
    repo.find_commit(id)
        .and_then(|commit| commit.time())
        .map(|time| time.seconds)
        .map_err(|error| internal_failure("read Git commit time", error))
}
