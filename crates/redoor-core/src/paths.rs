//! Locates the account home and every per-application directory derived from it.
//!
//! All path conventions live here so the server, agent, CLI and service installers agree
//! on one layout, and so code under test can point them at a temporary home instead of
//! mutating process-wide environment variables.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::process::ProcessSlot;

/// Returned when `HOME` is missing or empty; callers add what they needed it for.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("HOME is not set")]
pub struct HomeNotSet;

/// Reads the current account's home directory from `HOME`.
///
/// One lookup for every caller keeps the "unset or empty means absent" rule identical
/// everywhere instead of each module deciding whether an empty value counts.
pub fn home() -> Result<PathBuf, HomeNotSet> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .ok_or(HomeNotSet)
}

/// Per-application filesystem layout for one validated application name.
///
/// Captured once at startup from the process environment and privileges, then passed
/// down, so every subsystem resolves the same namespace without reading globals.
#[derive(Clone, Debug)]
pub struct AppPaths {
    /// Validated namespace used as one path component and in service names.
    app_name: String,
    /// Account home; absent when `HOME` is unset (only fatal for paths that need it).
    home: Option<PathBuf>,
    /// `XDG_CACHE_HOME` as given; validated lazily so unrelated commands do not fail.
    cache_root: Option<PathBuf>,
    /// Root uses system-wide config and log locations instead of the home directory.
    root: bool,
}

impl AppPaths {
    /// Snapshots `HOME`, `XDG_CACHE_HOME` and the effective UID for an already validated name.
    pub fn from_env(app_name: String) -> Self {
        Self {
            app_name,
            home: home().ok(),
            cache_root: std::env::var_os("XDG_CACHE_HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            root: effective_uid_is_root(),
        }
    }

    /// Builds a layout rooted in an explicit home so tests never touch the real account.
    pub fn for_home(app_name: impl Into<String>, home: impl Into<PathBuf>) -> Self {
        Self {
            app_name: app_name.into(),
            home: Some(home.into()),
            cache_root: None,
            root: false,
        }
    }

    /// Validated application namespace selected by `--app-name` / `REDOOR_APP_NAME`.
    pub fn app_name(&self) -> &str {
        &self.app_name
    }

    /// Account home, or an error naming what the caller was trying to locate.
    pub fn home(&self, purpose: &str) -> Result<&Path> {
        self.home
            .as_deref()
            .ok_or(HomeNotSet)
            .with_context(|| format!("cannot locate {purpose}"))
    }

    /// Persistent per-application data (`~/.local/share/<app-name>`): sessions, PID files, MCP state.
    pub fn data_dir(&self) -> Result<PathBuf> {
        Ok(self
            .home("the application data directory")?
            .join(".local/share")
            .join(&self.app_name))
    }

    /// Disposable per-application cache, honoring an absolute `XDG_CACHE_HOME`.
    ///
    /// Keeping downloads out of the data directory lets cache cleanup tools reclaim them
    /// without touching configuration or application state.
    pub fn cache_dir(&self) -> Result<PathBuf> {
        let cache_root = match &self.cache_root {
            Some(cache_root) => cache_root.clone(),
            None => self.home("the application cache directory")?.join(".cache"),
        };
        anyhow::ensure!(
            cache_root.is_absolute(),
            "XDG_CACHE_HOME must be an absolute path"
        );
        Ok(cache_root.join(&self.app_name))
    }

    /// Conventional config path for the current privileges.
    ///
    /// Root loads `/etc/<app-name>/config.toml` so system units and admin installs share
    /// one file; non-root uses the matching directory under `~/.config`.
    pub fn default_config_path(&self) -> Result<PathBuf> {
        if self.root {
            return Ok(PathBuf::from("/etc")
                .join(&self.app_name)
                .join("config.toml"));
        }
        Ok(self
            .home("the default config; pass --config with a config.toml path")?
            .join(".config")
            .join(&self.app_name)
            .join("config.toml"))
    }

    /// Directory for process log files under the current privileges.
    ///
    /// Root uses `/var/log/<app-name>` so system units share a writable location;
    /// non-root keeps logs in the per-application data directory.
    pub fn log_dir(&self) -> Result<PathBuf> {
        if self.root {
            return Ok(PathBuf::from("/var/log").join(&self.app_name));
        }
        self.data_dir()
    }

    /// Default persistent log for a process slot when no CLI, environment, or TOML path is set.
    pub fn process_log_path(&self, slot: ProcessSlot) -> Result<String> {
        Ok(self
            .log_dir()?
            .join(format!("{}.log", slot.file_stem()))
            .display()
            .to_string())
    }

    /// Default SSH-relay log; the stable relay ID keeps relays from mixing output.
    pub fn relay_log_path(&self, id: &str) -> Result<String> {
        Ok(self
            .log_dir()?
            .join("relays")
            .join(format!("{id}.log"))
            .display()
            .to_string())
    }
}

/// Whether the process effective UID is root (system-install / privileged path).
fn effective_uid_is_root() -> bool {
    #[cfg(unix)]
    {
        nix::unistd::Uid::effective().is_root()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keeps every role's files inside one namespace below the supplied home.
    #[test]
    fn layout_derives_from_home_and_app_name() {
        let paths = AppPaths::for_home("redoor-dev", "/home/test");
        // Persistent state and logs share the per-application data directory.
        assert_eq!(
            paths.data_dir().unwrap(),
            Path::new("/home/test/.local/share/redoor-dev")
        );
        assert_eq!(
            paths.process_log_path(ProcessSlot::Server).unwrap(),
            "/home/test/.local/share/redoor-dev/server.log"
        );
        // The cache defaults beside, not inside, the data directory.
        assert_eq!(
            paths.cache_dir().unwrap(),
            Path::new("/home/test/.cache/redoor-dev")
        );
        assert_eq!(
            paths.default_config_path().unwrap(),
            Path::new("/home/test/.config/redoor-dev/config.toml")
        );
    }

    /// A missing home must explain which location could not be derived.
    #[test]
    fn missing_home_names_the_purpose() {
        let paths = AppPaths {
            app_name: "redoor".into(),
            home: None,
            cache_root: None,
            root: false,
        };
        let error = format!("{:#}", paths.data_dir().unwrap_err());
        // Both the purpose and the root cause are needed to fix the environment.
        assert_eq!(
            error,
            "cannot locate the application data directory: HOME is not set"
        );
    }
}
