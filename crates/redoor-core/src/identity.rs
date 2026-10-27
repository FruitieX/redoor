//! Identity of the running binary and host shared by agent and server diagnostics.

use redoor_protocol::identity::{BinaryIdentity, ServerBuildMode};

/// Name the `redoor` executable reports in `--version`. Every workspace crate has its own
/// `CARGO_PKG_NAME`, so the binary's clap metadata and SSH provisioning (which compares the
/// remote `--version` output) must share this constant instead of each reading its own
/// package name.
pub const BINARY_NAME: &str = "redoor";

/// Workspace version shared by every crate (`version.workspace = true`), so any crate can
/// state the version the binary reports.
pub const BINARY_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Exact `redoor --version` line (without newline). SSH provisioning compares a remote
/// binary's output against it to decide whether the remote install is current.
pub fn version_line() -> String {
    format!("{BINARY_NAME} {BINARY_VERSION}")
}

/// Reads compile-time identity baked by `build.rs` for the running binary.
pub fn current_binary_identity() -> BinaryIdentity {
    BinaryIdentity {
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_rev: env!("REDOOR_GIT_REV").to_string(),
        git_dirty: env!("REDOOR_GIT_DIRTY") == "1",
        version_dirty: env!("REDOOR_VERSION_DIRTY") == "1",
        build_mode: match env!("REDOOR_BUILD_PROFILE") {
            "release" => ServerBuildMode::Release,
            "debug" => ServerBuildMode::Debug,
            _ => ServerBuildMode::Unknown,
        },
        build_date: env!("REDOOR_BUILD_DATE").to_string(),
    }
}

/// Absolute filesystem path of the running binary for operator diagnostics.
///
/// Prefer the canonical path so `/proc/self/exe`-style links resolve to a real file, even
/// after `redoor upgrade` replaced it, so an agent still reports where to install the next one.
pub async fn current_exe_path() -> String {
    match crate::executable::current() {
        Ok(path) => match tokio::fs::canonicalize(&path).await {
            Ok(canonical) => canonical.display().to_string(),
            Err(_) => path.display().to_string(),
        },
        Err(_) => "unknown".to_string(),
    }
}

/// Resolves and caches the primary non-loopback IP selected by the local routing table.
pub async fn external_ip() -> Option<String> {
    static EXTERNAL_IP: tokio::sync::OnceCell<Option<String>> = tokio::sync::OnceCell::const_new();

    EXTERNAL_IP
        .get_or_init(|| async {
            let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await.ok()?;
            // UDP connect only asks the kernel to choose a route; it sends no network traffic.
            socket.connect("1.1.1.1:80").await.ok()?;
            let address = socket.local_addr().ok()?.ip();
            (!address.is_loopback() && !address.is_unspecified()).then(|| address.to_string())
        })
        .await
        .clone()
}
