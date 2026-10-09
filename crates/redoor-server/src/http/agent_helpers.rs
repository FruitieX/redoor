use serde::Deserialize;

use super::error::ApiError;

/// Extracts an agent and its optional file path from exact-root and wildcard routes.
#[derive(Deserialize)]
pub(crate) struct AgentFilePath {
    pub(crate) agent: String,
    pub(crate) path: Option<String>,
}

/// Restores the implicit filesystem root omitted from REST wildcard URL segments.
pub(crate) fn absolute_path_from_url(path: String) -> String {
    format!("/{path}")
}

/// Rejects cwd-dependent filesystem addressing without canonicalizing valid destinations.
pub(crate) fn require_absolute_path(path: String) -> Result<String, ApiError> {
    if std::path::Path::new(&path).is_absolute() {
        Ok(path)
    } else {
        Err(ApiError::bad_request("Filesystem path must be absolute"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies URL splats become absolute without requiring a doubled route separator.
    #[test]
    fn restores_implicit_url_root() {
        // The empty splat addresses the filesystem root.
        assert_eq!(absolute_path_from_url(String::new()), "/");
        // Nested URL segments retain their filesystem hierarchy.
        assert_eq!(
            absolute_path_from_url("home/user".to_string()),
            "/home/user"
        );
    }

    /// Verifies lexical validation accepts absolute destinations without requiring existence.
    #[test]
    fn accepts_absolute_paths() {
        for path in ["/", "/tmp/existing-style", "/not-created-yet/file"] {
            // Nonexistent destinations must remain usable by upload, mkdir, and copy APIs.
            assert_eq!(require_absolute_path(path.to_string()).unwrap(), path);
        }
    }

    /// Verifies malformed paths fail at the transport boundary before agent lookup.
    #[test]
    fn rejects_relative_paths() {
        for path in ["relative", "./relative", "../relative", ""] {
            let error = require_absolute_path(path.to_string()).unwrap_err();
            // A stable 400 response makes relative addressing invalid for every endpoint.
            assert_eq!(error.status_code(), axum::http::StatusCode::BAD_REQUEST);
        }
    }
}
