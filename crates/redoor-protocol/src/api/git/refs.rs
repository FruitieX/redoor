//! Ref views: the branches and tags of a repository, most recently committed first.

use super::commit::GitCommitSummary;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Chooses which refs a listing reads. Branches and tags are separate lists because people
/// look for them separately, and one huge tag namespace should not push branches off a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitRefListKind {
    /// Local and remote-tracking branches.
    Branches,
    Tags,
}

/// Distinguishes where a ref lives, so the browser can mark remote and tag rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum GitRefKind {
    Branch,
    RemoteBranch,
    Tag,
}

/// One branch or tag with the commit it points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitRef {
    /// The short name people type, such as `main`, `origin/main` or `v1.0`.
    pub name: String,
    /// The full ref name, which links use as the revision because a short name can be
    /// ambiguous between a tag and a branch.
    pub full_name: String,
    pub kind: GitRefKind,
    /// True for the branch HEAD has checked out.
    pub is_head: bool,
    /// The commit the ref points at, with annotated tags peeled.
    pub commit: GitCommitSummary,
}

/// One page of a repository's branches or tags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitRefListResponse {
    pub repository_root: String,
    /// Most recently committed first, so the refs people work on lead the list.
    pub refs: Vec<GitRef>,
    /// Every listable ref of this kind, so the browser can tell how many remain.
    pub total: usize,
    pub has_more: bool,
}
