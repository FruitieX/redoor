//! Local managed-agent child process launcher.

use super::{LocalAgentConfig, bootstrap::default_local_agent_name};
use crate::log;
use redoor_protocol::wire::log::Level;
use std::process::Stdio;
use tokio::process::Command;

/// Spawns `redoor agent` as a local child process and returns the running
/// [`tokio::process::Child`] so the watchdog supervisor can wait for it or
/// kill it when the WebSocket goes stale.
///
/// The child reuses the server's own binary (via `std::env::current_exe`),
/// which is always present because the server itself was launched from it.
/// This avoids requiring the operator to keep two binaries in sync or
/// configure a path. Stdio is inherited so agent logs appear in the same
/// terminal as the server logs unless `log` is set in the toml, in which case
/// stdout and stderr are piped for the caller to forward into that file.
///
/// Failures are I/O errors whose message already names the failing step and path, so the
/// watchdog can show them to the browser unchanged.
pub async fn spawn_local_agent(
    config: &LocalAgentConfig,
    app_name: &str,
    redoor_port: u16,
    agent_token: &str,
) -> std::io::Result<tokio::process::Child> {
    let name = config.name.clone().unwrap_or_else(default_local_agent_name);
    // The server owns managed ids; startup assigned one to every entry before spawning.
    let agent_id = config.id.as_ref().ok_or_else(|| {
        std::io::Error::other(format!(
            "local agent '{name}' has no device id in the config"
        ))
    })?;
    let ws_url = format!(
        "ws://localhost:{}{}",
        redoor_port,
        redoor_protocol::wire::handshake::AGENT_WS_PATH
    );

    let bin = std::env::current_exe()
        .map_err(|e| with_context(e, "Failed to determine redoor binary path"))?;

    let mut command = Command::new(&bin);
    command
        .env_remove(crate::process::DETACHED_ENV)
        .env(crate::process::MANAGED_AGENT_ENV, "1")
        .arg("--app-name")
        .arg(app_name)
        .arg("agent")
        .arg(&ws_url)
        .arg("--log-format")
        .arg("line")
        .arg("--name")
        .arg(&name)
        .arg("--agent-id")
        .arg(agent_id.as_str())
        .arg("--token")
        .arg(agent_token);

    if let Some(home) = &config.home {
        command.arg("--home").arg(home);
    }

    command.stdin(Stdio::inherit());

    if config.log.is_some() {
        // Pipe the child's output so the watchdog forwards it into the size-bounded log
        // file. A descriptor inherited straight into the file could not be kept under its
        // limit, and passing --log instead would duplicate every line through stdout.
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
    } else {
        command.stdout(Stdio::inherit());
        command.stderr(Stdio::inherit());
    }

    log!(
        Level::Info,
        "Starting local redoor agent: name={}, agent_id={}, ws_url={}, bin={}, log={:?}",
        name,
        agent_id,
        ws_url,
        bin.display(),
        config.log,
    );

    // Ensure the agent process is killed if the supervisor task is
    // dropped (e.g. on server shutdown), preventing the `redoor agent`
    // child from being orphaned. `kill_on_drop` sends SIGKILL.
    command.kill_on_drop(true);
    command.spawn().map_err(|e| {
        with_context(
            e,
            &format!("failed to spawn local agent binary '{}'", bin.display()),
        )
    })
}

/// Prefixes an I/O error with the failed step while keeping its kind for callers.
fn with_context(error: std::io::Error, context: &str) -> std::io::Error {
    std::io::Error::new(error.kind(), format!("{context}: {error}"))
}
