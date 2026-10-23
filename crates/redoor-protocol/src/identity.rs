//! Compile-time binary identity reported by both servers and agents.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Cargo profile the binary was compiled with (`debug` vs `release`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[ts(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ServerBuildMode {
    /// Unoptimized developer build (`cargo build` / `cargo test`).
    Debug,
    /// Optimized production build (`cargo build --release`).
    Release,
    /// Profile string from Cargo was not `debug` or `release`.
    Unknown,
}

/// Non-secret compile-time identity shared by server and agent binaries.
///
/// Kept identical on both sides so the UI can compare a connected agent against
/// the server and flag mismatched or dirty builds without probing the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BinaryIdentity {
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
