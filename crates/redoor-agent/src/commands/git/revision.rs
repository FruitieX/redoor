//! Resolving user-supplied revisions to commits, shared by every feature that reads history.

use std::path::{Path, PathBuf};

use gix::bstr::ByteSlice;

use redoor_protocol::{
    api::git::{commit::GitCommitSummary, revision::validate_revision},
    ids::UnixTimestampSeconds,
    wire::command::CommandErrorKind,
};

use super::{
    GitFailure, RepositoryPath, discover_repository_from, discover_required_repository,
    internal_failure, utf8_relative,
};

/// Commits and annotated tags are small; anything larger is not a revision worth inflating
/// on a memory-constrained agent.
const MAX_REVISION_OBJECT_BYTES: u64 = 4 * 1024 * 1024;
/// Tags can point at tags; a longer chain than this is treated as malformed.
const MAX_TAG_DEPTH: usize = 16;

/// A repository path resolved together with the commit a revision names, so history commands
/// read one consistent commit instead of resolving the revision repeatedly.
pub(super) struct RevisionPath {
    pub(super) repository: RepositoryPath,
    /// The repository-relative path in `/` form, empty for the repository root.
    pub(super) relative: String,
    pub(super) commit_id: gix::ObjectId,
}

impl RevisionPath {
    /// Validates both inputs and resolves the revision inside the path's repository.
    ///
    /// The worktree only locates the repository; the path may be missing from it, because
    /// history browsing must reach files and directories that were deleted since.
    pub(super) fn open(path: &str, rev: &str) -> Result<Self, GitFailure> {
        validate_revision(rev)
            .map_err(|message| GitFailure::new(CommandErrorKind::InvalidInput, message))?;
        let mut repository = discover_required_repository(path)?;
        let requested = repository.requested.clone();
        let commit_id = loop {
            if let Some(id) = resolve_commit(&repository.repo, rev)? {
                break id;
            }
            // A checked-out submodule or other nested repository shadows the repository that
            // contains it. Opening a submodule while browsing a superproject commit must show
            // the gitlink that commit recorded, so a revision the nested repository does not
            // know is resolved in the enclosing one instead.
            let enclosing = match repository.root.parent() {
                Some(parent) => discover_repository_from(parent, requested.clone())?,
                None => None,
            };
            repository = enclosing.ok_or_else(|| {
                GitFailure::new(
                    CommandErrorKind::InvalidInput,
                    format!("Unknown Git revision {rev:?}"),
                )
            })?;
        };
        let relative = utf8_relative(&repository.relative)?;
        Ok(Self {
            repository,
            relative,
            commit_id,
        })
    }

    /// Returns the resolved commit, borrowed from the repository it was resolved in.
    pub(super) fn commit(&self) -> Result<gix::Commit<'_>, GitFailure> {
        self.repository
            .repo
            .find_commit(self.commit_id)
            .map_err(|error| internal_failure("read Git commit", error))
    }

    /// Converts a repository-relative location back into the absolute worktree path the
    /// browser links to.
    pub(super) fn worktree_path(&self, relative: &str) -> PathBuf {
        if relative.is_empty() {
            self.repository.root.clone()
        } else {
            self.repository.root.join(Path::new(relative))
        }
    }
}

/// Resolves branches, tags, abbreviated ids and other revision expressions to a commit, or
/// `None` when the repository does not know the revision.
///
/// Tags are peeled, so `v1` shows the tagged commit. Each object's kind and size are read
/// from its header before it is inflated, so a revision naming a huge blob is rejected
/// without loading it into memory. Revisions naming anything but a commit are invalid input
/// rather than missing paths, so the browser does not offer to create them.
fn resolve_commit(repo: &gix::Repository, rev: &str) -> Result<Option<gix::ObjectId>, GitFailure> {
    let Ok(id) = repo.rev_parse_single(rev.as_bytes().as_bstr()) else {
        return Ok(None);
    };
    peel_to_commit(repo, id.detach())?.map(Some).ok_or_else(|| {
        GitFailure::new(
            CommandErrorKind::InvalidInput,
            format!("Git revision {rev:?} does not name a commit"),
        )
    })
}

/// Follows annotated tags from `id` to the commit they name, or `None` when the chain ends
/// at a tree or blob, is oversized or is too deep.
///
/// Each object's kind and size are read from its header before it is inflated, so a tag
/// naming a huge blob is rejected without loading it into memory. Shared by revisions typed
/// by the user and by ref listings, which point at arbitrary objects too.
pub(super) fn peel_to_commit(
    repo: &gix::Repository,
    mut id: gix::ObjectId,
) -> Result<Option<gix::ObjectId>, GitFailure> {
    for _ in 0..MAX_TAG_DEPTH {
        let header = repo
            .find_header(id)
            .map_err(|error| internal_failure("read Git revision object header", error))?;
        if header.size() > MAX_REVISION_OBJECT_BYTES {
            return Ok(None);
        }
        match header.kind() {
            gix::object::Kind::Commit => return Ok(Some(id)),
            gix::object::Kind::Tag => {
                id = repo
                    .find_tag(id)
                    .and_then(|tag| tag.target_id())
                    .map_err(|error| internal_failure("read Git tag", error))?
                    .detach();
            }
            gix::object::Kind::Tree | gix::object::Kind::Blob => return Ok(None),
        }
    }
    Ok(None)
}

/// Reads the summary fields the UI shows for a commit.
pub(super) fn commit_summary(commit: &gix::Commit<'_>) -> Result<GitCommitSummary, GitFailure> {
    let author = commit
        .author()
        .map_err(|error| internal_failure("read Git commit author", error))?;
    // A malformed author time should not hide the commit, so it falls back to the epoch.
    let authored_at = author.time().map(|time| time.seconds).unwrap_or_default();
    let summary = commit
        .message()
        .map_err(|error| internal_failure("read Git commit message", error))?
        .summary()
        .to_str_lossy()
        .into_owned();
    Ok(GitCommitSummary {
        id: commit.id.to_string(),
        summary,
        author_name: author.name.to_str_lossy().trim().to_string(),
        author_email: author.email.to_str_lossy().trim().to_string(),
        authored_at: UnixTimestampSeconds::new(authored_at),
        parent_ids: commit.parent_ids().map(|id| id.to_string()).collect(),
    })
}

/// Looks up one repository-relative path in a tree; the empty path has no entry because it
/// is the tree itself.
pub(super) fn tree_entry(
    tree: &gix::Tree<'_>,
    relative: &str,
) -> Result<Option<gix::objs::tree::Entry>, GitFailure> {
    if relative.is_empty() {
        return Ok(None);
    }
    Ok(tree
        .lookup_entry_by_path(Path::new(relative))
        .map_err(|error| internal_failure("look up Git tree entry", error))?
        .map(gix::object::tree::Entry::detach))
}
