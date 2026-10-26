//! What one commit changed: its tree compared with its first parent's, below one path.

use std::sync::atomic::{AtomicBool, Ordering};

use gix::bstr::ByteSlice;

use redoor_protocol::{
    api::git::{
        GitChangeState, GitDiffResult,
        commit::{GitCommitDiffResponse, GitCommitFileChange},
        worktree::GitDiffRequest,
    },
    wire::{command::CommandErrorKind, request::CommandError},
};

use super::{
    GitFailure,
    diff::{DiffSource, MAX_BATCH_DIFF_BYTES, compare_sources, read_object_blob},
    internal_failure,
    revision::{RevisionPath, commit_summary, tree_entry},
    run_blocking,
};

/// One side of a changed file: its mode and object, or nothing when the side lacks the file.
type Side = Option<(gix::objs::tree::EntryMode, gix::ObjectId)>;

/// One changed file before its patch is formatted.
struct FileChange {
    relative: String,
    old: Side,
    new: Side,
}

/// Diffs the commit a revision names against its first parent, below one path.
pub(crate) async fn commit_diff(
    path: String,
    rev: String,
) -> Result<GitCommitDiffResponse, CommandError> {
    run_blocking(move |interrupt| commit_diff_blocking(&path, &rev, &interrupt)).await
}

/// Collects the changed files, then formats each within the shared batch budget.
fn commit_diff_blocking(
    path: &str,
    rev: &str,
    interrupt: &AtomicBool,
) -> Result<GitCommitDiffResponse, GitFailure> {
    let target = RevisionPath::open(path, rev)?;
    let repo = &target.repository.repo;
    let commit = target.commit()?;
    let new_root = commit
        .tree()
        .map_err(|error| internal_failure("read Git commit tree", error))?;
    // Merges are shown against their first parent, the branch they were merged into.
    let parent = commit
        .parent_ids()
        .next()
        .map(|id| {
            repo.find_commit(id)
                .map_err(|error| internal_failure("read Git parent commit", error))
        })
        .transpose()?;
    let old_root = parent
        .as_ref()
        .map(|parent| {
            parent
                .tree()
                .map_err(|error| internal_failure("read Git parent tree", error))
        })
        .transpose()?;

    let mut changes = collect_changes(repo, &target.relative, old_root.as_ref(), &new_root)?;
    changes.sort_by(|left, right| left.relative.cmp(&right.relative));
    let truncated = changes.len() > GitDiffRequest::MAX_FILES;
    changes.truncate(GitDiffRequest::MAX_FILES);

    let mut remaining = MAX_BATCH_DIFF_BYTES;
    let mut files = Vec::with_capacity(changes.len());
    for change in changes {
        if interrupt.load(Ordering::Acquire) {
            return Err(GitFailure::new(
                CommandErrorKind::Canceled,
                "Git commit diff was canceled",
            ));
        }
        let result = if change.relative.chars().any(char::is_control) {
            GitDiffResult::UnsupportedEntry
        } else {
            compare_sources(
                side_source(repo, change.old)?,
                side_source(repo, change.new)?,
                &change.relative,
                remaining,
            )?
        };
        if let GitDiffResult::Text { unified_diff } = &result {
            remaining -= unified_diff.len();
        }
        files.push(GitCommitFileChange {
            path: target.worktree_path(&change.relative).display().to_string(),
            change: change_state(change.old, change.new),
            repository_relative_path: change.relative,
            result,
        });
    }

    Ok(GitCommitDiffResponse {
        commit: commit_summary(&commit)?,
        parent_id: parent.map(|parent| parent.id.to_string()),
        repository_root: target.repository.root.display().to_string(),
        repository_relative_path: target.relative.clone(),
        files,
        truncated,
    })
}

/// Finds the files that differ below `relative`, diffing only the selected subtree so a
/// large commit elsewhere in the repository costs nothing.
fn collect_changes<'repo>(
    repo: &'repo gix::Repository,
    relative: &str,
    old_root: Option<&gix::Tree<'repo>>,
    new_root: &gix::Tree<'repo>,
) -> Result<Vec<FileChange>, GitFailure> {
    let old_entry = match old_root {
        Some(tree) => tree_entry(tree, relative)?,
        None => None,
    };
    let new_entry = tree_entry(new_root, relative)?;
    let is_tree = |entry: &Option<gix::objs::tree::Entry>| {
        entry.as_ref().is_some_and(|entry| entry.mode.is_tree())
    };

    if !relative.is_empty() && !is_tree(&old_entry) && !is_tree(&new_entry) {
        // A single file: compare its two committed versions directly.
        let old = old_entry.map(|entry| (entry.mode, entry.oid));
        let new = new_entry.map(|entry| (entry.mode, entry.oid));
        return Ok(if old == new {
            Vec::new()
        } else {
            vec![FileChange {
                relative: relative.to_string(),
                old,
                new,
            }]
        });
    }

    let old_tree = subtree(repo, relative, old_root, old_entry)?;
    let new_tree = subtree(repo, relative, Some(new_root), new_entry)?;
    let changes = repo
        .diff_tree_to_tree(
            old_tree.as_ref(),
            new_tree.as_ref(),
            gix::diff::Options::default().with_rewrites(None),
        )
        .map_err(|error| internal_failure("diff Git trees", error))?;

    let prefix = if relative.is_empty() {
        String::new()
    } else {
        format!("{relative}/")
    };
    let mut files = Vec::new();
    for change in changes {
        use gix::object::tree::diff::ChangeDetached;
        let (location, old, new) = match change {
            ChangeDetached::Addition {
                location,
                entry_mode,
                id,
                ..
            } => (location, None, Some((entry_mode, id))),
            ChangeDetached::Deletion {
                location,
                entry_mode,
                id,
                ..
            } => (location, Some((entry_mode, id)), None),
            ChangeDetached::Modification {
                location,
                previous_entry_mode,
                previous_id,
                entry_mode,
                id,
            } => (
                location,
                Some((previous_entry_mode, previous_id)),
                Some((entry_mode, id)),
            ),
            // Rewrite tracking is disabled above, so renames arrive as deletion plus addition.
            ChangeDetached::Rewrite { .. } => continue,
        };
        // Trees are reported alongside their files; only the files carry content.
        let old = old.filter(|(mode, _)| !mode.is_tree());
        let new = new.filter(|(mode, _)| !mode.is_tree());
        if old.is_none() && new.is_none() {
            continue;
        }
        let Ok(location) = location.to_str() else {
            continue;
        };
        files.push(FileChange {
            relative: format!("{prefix}{location}"),
            old,
            new,
        });
    }
    Ok(files)
}

/// Selects the tree to diff on one side: the root for the repository itself, the entry's tree
/// for a directory, or nothing when that side has no directory at the path.
fn subtree<'repo>(
    repo: &'repo gix::Repository,
    relative: &str,
    root: Option<&gix::Tree<'repo>>,
    entry: Option<gix::objs::tree::Entry>,
) -> Result<Option<gix::Tree<'repo>>, GitFailure> {
    match (root, entry) {
        (Some(root), _) if relative.is_empty() => Ok(Some(root.clone())),
        (_, Some(entry)) if entry.mode.is_tree() => repo
            .find_tree(entry.oid)
            .map(Some)
            .map_err(|error| internal_failure("read Git tree", error)),
        _ => Ok(None),
    }
}

/// Reads one side's blob within the per-file bound; symlinks and submodules have no text diff.
fn side_source(repo: &gix::Repository, side: Side) -> Result<DiffSource, GitFailure> {
    match side {
        None => Ok(DiffSource::Missing),
        Some((mode, id)) if mode.is_blob() => read_object_blob(repo, id),
        Some(_) => Ok(DiffSource::Unsupported),
    }
}

/// Classifies a file change in the same vocabulary as worktree status.
fn change_state(old: Side, new: Side) -> GitChangeState {
    match (old, new) {
        (None, _) => GitChangeState::Added,
        (_, None) => GitChangeState::Deleted,
        (Some((old_mode, _)), Some((new_mode, _))) if old_mode.kind() != new_mode.kind() => {
            if old_mode.is_blob() && new_mode.is_blob() {
                // Only the executable bit changed, which Git still calls a modification.
                GitChangeState::Modified
            } else {
                GitChangeState::TypeChanged
            }
        }
        _ => GitChangeState::Modified,
    }
}
