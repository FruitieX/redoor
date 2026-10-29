//! Browsing committed trees: directory listings and file contents at a revision.

use std::sync::atomic::{AtomicBool, Ordering};

use gix::bstr::ByteSlice;

use redoor_protocol::{
    api::git::revision::{
        GitRevisionContentResponse, GitRevisionEntry, GitRevisionTreeResponse, GitTreeEntry,
        GitTreeEntryKind,
    },
    wire::{command::CommandErrorKind, request::CommandError},
};

use super::{
    GitFailure,
    diff::{DiffSource, MAX_DIFF_INPUT_BYTES, read_object_blob},
    internal_failure,
    revision::{RevisionPath, commit_summary, tree_entry},
    run_blocking,
};

/// Bounds listing memory and the size of its WebSocket response, like worktree status.
const MAX_TREE_ENTRIES: usize = 5_000;
/// Symlink blobs are paths; anything longer is not worth showing in full.
const MAX_SYMLINK_TARGET_BYTES: u64 = 4 * 1024;

/// Lists a directory or describes a file as committed at a revision.
pub(crate) async fn tree(
    path: String,
    rev: String,
) -> Result<GitRevisionTreeResponse, CommandError> {
    run_blocking(move |interrupt| tree_blocking(&path, rev, &interrupt)).await
}

/// Reads one committed text file for the read-only editor.
pub(crate) async fn content(
    path: String,
    rev: String,
) -> Result<GitRevisionContentResponse, CommandError> {
    run_blocking(move |_| content_blocking(&path, &rev)).await
}

/// Resolves the path at the revision and describes it by entry kind.
fn tree_blocking(
    path: &str,
    rev: String,
    interrupt: &AtomicBool,
) -> Result<GitRevisionTreeResponse, GitFailure> {
    let target = RevisionPath::open(path, &rev)?;
    let repo = &target.repository.repo;
    let commit = target.commit()?;
    let root = commit
        .tree()
        .map_err(|error| internal_failure("read Git commit tree", error))?;
    let entry = match tree_entry(&root, &target.relative)? {
        // The repository root is the commit's tree itself.
        None if target.relative.is_empty() => list_directory(&root, interrupt)?,
        None => return Err(missing_at_revision(&target.relative, &rev)),
        Some(entry) if entry.mode.is_tree() => {
            let tree = repo
                .find_tree(entry.oid)
                .map_err(|error| internal_failure("read Git tree", error))?;
            list_directory(&tree, interrupt)?
        }
        Some(entry) if entry.mode.is_link() => GitRevisionEntry::Symlink {
            target: symlink_target(repo, entry.oid)?,
        },
        Some(entry) if entry.mode.is_commit() => GitRevisionEntry::Submodule {
            commit_id: entry.oid.to_string(),
        },
        Some(entry) => GitRevisionEntry::File {
            size: object_size(repo, entry.oid)?,
            executable: entry.mode.is_executable(),
            editable: matches!(
                read_object_blob(repo, entry.oid)?,
                DiffSource::Content(data) if std::str::from_utf8(&data).is_ok()
            ),
            blob_id: entry.oid.to_string(),
        },
    };
    Ok(GitRevisionTreeResponse {
        rev,
        commit: commit_summary(&commit)?,
        repository_root: target.repository.root.display().to_string(),
        path: target.worktree_path(&target.relative).display().to_string(),
        repository_relative_path: target.relative.clone(),
        entry,
    })
}

/// Returns the committed text, refusing anything the editor could not show safely.
fn content_blocking(path: &str, rev: &str) -> Result<GitRevisionContentResponse, GitFailure> {
    let target = RevisionPath::open(path, rev)?;
    let repo = &target.repository.repo;
    let commit = target.commit()?;
    let root = commit
        .tree()
        .map_err(|error| internal_failure("read Git commit tree", error))?;
    let entry = tree_entry(&root, &target.relative)?
        .ok_or_else(|| missing_at_revision(&target.relative, rev))?;
    if !entry.mode.is_blob() {
        return Err(GitFailure::new(
            CommandErrorKind::IsDirectory,
            "Only files have content at a Git revision",
        ));
    }
    let data = match read_object_blob(repo, entry.oid)? {
        DiffSource::Content(data) => data,
        DiffSource::TooLarge => {
            return Err(GitFailure::new(
                CommandErrorKind::InvalidInput,
                format!("File is larger than {MAX_DIFF_INPUT_BYTES} bytes and cannot be viewed"),
            ));
        }
        DiffSource::Missing | DiffSource::Unsupported => {
            return Err(missing_at_revision(&target.relative, rev));
        }
    };
    let content = String::from_utf8(data).map_err(|_| {
        GitFailure::new(
            CommandErrorKind::InvalidInput,
            "File is not UTF-8 text and cannot be viewed",
        )
    })?;
    Ok(GitRevisionContentResponse {
        path: target.worktree_path(&target.relative).display().to_string(),
        commit_id: commit.id.to_string(),
        content,
    })
}

/// Lists one tree's children with blob sizes, skipping names the browser cannot link.
fn list_directory(
    tree: &gix::Tree<'_>,
    interrupt: &AtomicBool,
) -> Result<GitRevisionEntry, GitFailure> {
    let mut entries = Vec::new();
    let mut omitted_non_utf8_entries = 0;
    let mut truncated = false;
    for entry in tree.iter() {
        if interrupt.load(Ordering::Acquire) {
            return Err(GitFailure::new(
                CommandErrorKind::Canceled,
                "Git tree listing was canceled",
            ));
        }
        let entry = entry.map_err(|error| internal_failure("decode Git tree", error))?;
        let Ok(name) = entry.filename().to_str() else {
            omitted_non_utf8_entries += 1;
            continue;
        };
        if entries.len() == MAX_TREE_ENTRIES {
            truncated = true;
            break;
        }
        let mode = entry.mode();
        let kind = if mode.is_tree() {
            GitTreeEntryKind::Directory
        } else if mode.is_commit() {
            GitTreeEntryKind::Submodule
        } else if mode.is_link() {
            GitTreeEntryKind::Symlink
        } else if mode.is_executable() {
            GitTreeEntryKind::Executable
        } else {
            GitTreeEntryKind::File
        };
        let size = if mode.is_blob_or_symlink() {
            Some(object_size(tree.repo, entry.object_id())?)
        } else {
            None
        };
        entries.push(GitTreeEntry {
            name: name.to_string(),
            kind,
            size,
        });
    }
    Ok(GitRevisionEntry::Directory {
        entries,
        truncated,
        omitted_non_utf8_entries,
    })
}

/// Reads an object's size from its header, without inflating the object.
pub(super) fn object_size(repo: &gix::Repository, id: gix::ObjectId) -> Result<u64, GitFailure> {
    repo.find_header(id)
        .map(|header| header.size())
        .map_err(|error| internal_failure("read Git object header", error))
}

/// Reads a symlink's committed target, which Git stores as the blob's content.
fn symlink_target(repo: &gix::Repository, id: gix::ObjectId) -> Result<String, GitFailure> {
    if object_size(repo, id)? > MAX_SYMLINK_TARGET_BYTES {
        return Ok(String::from("(target too long to show)"));
    }
    let object = repo
        .find_object(id)
        .map_err(|error| internal_failure("read Git symlink", error))?;
    Ok(object.data.to_str_lossy().into_owned())
}

/// Reports a path that the revision does not contain as missing, like the worktree browser.
pub(super) fn missing_at_revision(relative: &str, rev: &str) -> GitFailure {
    GitFailure::new(
        CommandErrorKind::NotFound,
        format!("{relative} does not exist at Git revision {rev:?}"),
    )
}
