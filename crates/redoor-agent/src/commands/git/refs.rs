//! A repository's branches or tags, most recently committed first, one page at a time.

use std::{cmp::Ordering, collections::BinaryHeap, sync::atomic::AtomicBool};

use gix::bstr::ByteSlice;

use redoor_protocol::{
    api::git::{
        GitPage,
        refs::{GitRef, GitRefKind, GitRefListKind, GitRefListResponse},
    },
    wire::{command::CommandErrorKind, request::CommandError},
};

use super::{
    GitFailure, check_interrupt, discover_required_repository, internal_failure,
    revision::{commit_summary, peel_to_commit},
    run_blocking,
};

/// A ref that may belong on the page, ordered by the time of its commit.
#[derive(PartialEq, Eq)]
struct Candidate {
    /// Committer time, so a rebased or freshly merged branch counts as recent.
    time: i64,
    full_name: String,
    kind: GitRefKind,
    commit_id: gix::ObjectId,
}

impl Ord for Candidate {
    /// Newer commits sort first; equal times fall back to the name so pages are stable.
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .time
            .cmp(&self.time)
            .then_with(|| self.full_name.cmp(&other.full_name))
    }
}

impl PartialOrd for Candidate {
    /// Delegates to the total order above.
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Lists one page of the branches or tags of the repository containing `path`.
pub(crate) async fn list(
    path: String,
    kind: GitRefListKind,
    page: GitPage,
) -> Result<GitRefListResponse, CommandError> {
    run_blocking(move |interrupt| list_blocking(&path, kind, page, &interrupt)).await
}

/// Reads every ref of the kind, keeps only the rows up to the end of the page, then
/// summarizes the page.
///
/// Sorting by commit time needs every ref's commit, so all refs are visited, but at most
/// `skip + limit` candidates are held at once. A repository with a hundred thousand tags
/// costs reading their commits, not keeping them.
fn list_blocking(
    path: &str,
    kind: GitRefListKind,
    page: GitPage,
    interrupt: &AtomicBool,
) -> Result<GitRefListResponse, GitFailure> {
    page.validate()
        .map_err(|message| GitFailure::new(CommandErrorKind::InvalidInput, message))?;
    let repository = discover_required_repository(path)?;
    let repo = &repository.repo;
    let head_name = repo
        .head_name()
        .map_err(|error| internal_failure("read repository HEAD", error))?;
    let references = repo
        .references()
        .map_err(|error| internal_failure("open Git references", error))?;
    let sources = match kind {
        GitRefListKind::Branches => vec![
            (
                references
                    .local_branches()
                    .map_err(|error| internal_failure("list Git branches", error))?,
                GitRefKind::Branch,
            ),
            (
                references
                    .remote_branches()
                    .map_err(|error| internal_failure("list Git remote branches", error))?,
                GitRefKind::RemoteBranch,
            ),
        ],
        GitRefListKind::Tags => vec![(
            references
                .tags()
                .map_err(|error| internal_failure("list Git tags", error))?,
            GitRefKind::Tag,
        )],
    };

    let keep = (page.skip + page.limit) as usize;
    // A max-heap under the newest-first order holds the oldest kept candidate on top, so it
    // is the one dropped when a newer ref arrives.
    let mut kept = BinaryHeap::with_capacity(keep + 1);
    let mut total = 0;
    for (iter, kind) in sources {
        for reference in iter {
            check_interrupt(interrupt, "Git ref listing")?;
            let reference =
                reference.map_err(|error| internal_failure("read Git reference", error))?;
            // Symbolic refs such as `origin/HEAD` only repeat a branch already listed.
            let Some(target) = reference.try_id() else {
                continue;
            };
            // A name that is not UTF-8 cannot be linked to as a revision.
            let Ok(full_name) = reference.name().as_bstr().to_str() else {
                continue;
            };
            // Tags may name trees or blobs, which have no history to browse.
            let Some(commit_id) = peel_to_commit(repo, target.detach())? else {
                continue;
            };
            let time = repo
                .find_commit(commit_id)
                .and_then(|commit| commit.time())
                .map_err(|error| internal_failure("read Git commit time", error))?
                .seconds;
            total += 1;
            kept.push(Candidate {
                time,
                full_name: full_name.to_string(),
                kind,
                commit_id,
            });
            if kept.len() > keep {
                kept.pop();
            }
        }
    }

    let refs = kept
        .into_sorted_vec()
        .into_iter()
        .skip(page.skip as usize)
        .map(|candidate| {
            let commit = repo
                .find_commit(candidate.commit_id)
                .map_err(|error| internal_failure("read Git commit", error))?;
            let is_head = head_name
                .as_ref()
                .is_some_and(|head| head.as_bstr() == candidate.full_name.as_bytes());
            Ok(GitRef {
                name: short_name(&candidate.full_name).to_string(),
                full_name: candidate.full_name,
                kind: candidate.kind,
                is_head,
                commit: commit_summary(&commit)?,
            })
        })
        .collect::<Result<Vec<_>, GitFailure>>()?;
    let has_more = (page.skip as usize).saturating_add(refs.len()) < total;
    Ok(GitRefListResponse {
        repository_root: repository.root.display().to_string(),
        refs,
        total,
        has_more,
    })
}

/// Strips the namespace prefix so rows show names the way `git branch -a` and `git tag` do.
fn short_name(full_name: &str) -> &str {
    ["refs/heads/", "refs/remotes/", "refs/tags/"]
        .iter()
        .find_map(|prefix| full_name.strip_prefix(prefix))
        .unwrap_or(full_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a candidate whose identity is irrelevant to ordering.
    fn candidate(time: i64, name: &str) -> Candidate {
        Candidate {
            time,
            full_name: name.to_string(),
            kind: GitRefKind::Branch,
            commit_id: gix::ObjectId::null(gix::hash::Kind::Sha1),
        }
    }

    /// Confirms the newest commits lead and equal times are ordered by name.
    #[test]
    fn candidates_sort_newest_first_then_by_name() {
        let mut sorted = vec![candidate(1, "a"), candidate(3, "c"), candidate(3, "b")];
        sorted.sort();
        let names: Vec<_> = sorted.iter().map(|c| c.full_name.as_str()).collect();
        // The newer pair comes first, broken by name, then the older ref.
        assert_eq!(names, ["b", "c", "a"]);
    }

    /// Confirms rows drop only the namespace a ref lives in.
    #[test]
    fn short_names_drop_the_ref_namespace() {
        // Remote branches keep their remote, like `git branch -r`.
        assert_eq!(short_name("refs/remotes/origin/main"), "origin/main");
        // Tags and local branches show their bare names.
        assert_eq!(short_name("refs/tags/v1"), "v1");
        assert_eq!(short_name("refs/heads/feature/x"), "feature/x");
    }
}
