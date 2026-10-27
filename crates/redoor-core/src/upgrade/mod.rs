//! Upgrading redoor in place: installing a release and restarting what runs the old binary.
//!
//! Shared by `redoor upgrade` and the server's update endpoint so both install and restart
//! exactly the same way; see "Upgrading in place" in `ARCHITECTURE.md`.

pub mod install;
pub mod restart;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Overrides the directory an upgrade installs into.
const INSTALL_DIR_ENV: &str = "REDOOR_INSTALL_DIR";
/// File name `install.sh` writes into the install directory.
const INSTALLED_NAME: &str = "redoor";

/// Where an upgrade of the running binary installs, and the path its processes run.
pub struct SelfInstall {
    /// Canonical path of this process's binary.
    pub current: PathBuf,
    /// Directory `install.sh` installs into.
    pub install_dir: PathBuf,
    /// Canonical path of the binary the install replaces, comparable with process paths.
    pub installed: PathBuf,
}

impl SelfInstall {
    /// Upgrades the binary that is running unless `REDOOR_INSTALL_DIR` picks another directory.
    ///
    /// `install.sh` alone would choose `~/.local/bin` for users and `/usr/local/bin` for root,
    /// which would leave a second, stale copy whenever the running one lives elsewhere.
    pub async fn resolve() -> Result<Self> {
        let current = crate::executable::current()?;
        // Canonical like the process paths it is compared with; macOS may report a symlink.
        let current = tokio::fs::canonicalize(&current).await.unwrap_or(current);
        let install_dir = match std::env::var_os(INSTALL_DIR_ENV).filter(|dir| !dir.is_empty()) {
            Some(directory) => PathBuf::from(directory),
            None => current
                .parent()
                .map(Path::to_path_buf)
                .context("The running redoor binary has no parent directory")?,
        };
        let directory = tokio::fs::canonicalize(&install_dir)
            .await
            .unwrap_or_else(|_| install_dir.clone());
        Ok(Self {
            installed: directory.join(INSTALLED_NAME),
            current,
            install_dir,
        })
    }
}
