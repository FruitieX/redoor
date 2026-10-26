//! Git REST endpoints, one submodule per feature area so new Git features (history,
//! branches, search) get their own module. Path handling shared by every area lives here.

pub(crate) mod commit;
pub(crate) mod revision;
pub(crate) mod worktree;

use super::{
    agent_helpers::{absolute_path_from_url, require_absolute_path},
    error::ApiError,
};
use redoor_protocol::api::git::revision::validate_revision;

/// Restores the implicit root and applies the shared absolute-path policy.
fn route_path(path: Option<String>) -> Result<String, ApiError> {
    let path = absolute_path_from_url(path.unwrap_or_default());
    require_absolute_path(path)
}

/// Rejects revision text at the REST boundary so a bad URL fails before reaching an agent.
fn checked_revision(rev: String) -> Result<String, ApiError> {
    validate_revision(&rev).map_err(ApiError::bad_request)?;
    Ok(rev)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keeps both exact and wildcard routes on the same absolute filesystem convention.
    #[test]
    fn route_paths_restore_the_implicit_root() {
        // The exact route addresses filesystem root rather than an agent cwd.
        assert_eq!(route_path(None).unwrap(), "/");
        // Wildcard segments preserve their hierarchy behind a single route slash.
        assert_eq!(
            route_path(Some("tmp/repo".to_string())).unwrap(),
            "/tmp/repo"
        );
    }
}
