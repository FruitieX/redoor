//! Server identity, restart, and authentication-mode bodies.

use crate::identity::ServerBuildMode;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How browser login credentials are sourced for this server process.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[ts(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ServerAuthMode {
    /// Username and password come from the server TOML config.
    Toml,
    /// Username/password are verified against the process owner's Linux PAM account.
    Pam,
}

/// Server identity and authenticated agent bootstrap settings shown on the UI home page.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ServerInfoResponse {
    /// Filesystem and service namespace selected by `REDOOR_APP_NAME`.
    pub app_name: String,
    /// Shared registration secret included so the authenticated UI can render a working agent config.
    pub agent_token: String,
    /// Absolute path of the TOML config file this process loaded.
    pub config_path: String,
    /// Absolute path of the running server binary.
    pub exe_path: String,
    /// Whether login uses TOML credentials or system PAM.
    pub auth_mode: ServerAuthMode,
    /// Primary non-loopback IP selected from the server's local routing table.
    pub external_ip: Option<String>,
    /// Operating system of the running server executable.
    pub os: String,
    /// CPU architecture of the running server executable.
    pub arch: String,
    /// `CARGO_PKG_VERSION` baked into this binary.
    pub version: String,
    /// Full git commit SHA (or `unknown` when git metadata was unavailable at build).
    pub git_rev: String,
    /// True when the working tree had uncommitted changes at build time.
    pub git_dirty: bool,
    /// True when HEAD was not tagged `v{version}` at build time.
    pub version_dirty: bool,
    /// Whether this binary was compiled as debug or release.
    pub build_mode: ServerBuildMode,
    /// UTC compile timestamp (`YYYY-MM-DDTHH:MM:SSZ`) or `unknown`.
    pub build_date: String,
}

/// Confirms a server or agent accepted a restart request before its connection closes.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RestartResponse {
    pub restarting: bool,
}
