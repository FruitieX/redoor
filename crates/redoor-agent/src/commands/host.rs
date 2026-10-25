//! Host and process commands: echo, process info, agent details, accounts, logging and
//! process replacement.

use crate::platform::identity::HostIdentity;
use redoor_core::identity::{current_binary_identity, current_exe_path, external_ip};
use redoor_protocol::wire::request::CommandError;
use redoor_protocol::{
    api::agents::{AgentAccountsResponse, AgentDetailsResponse, EchoRequest},
    ids::{AgentId, UnixTimestampSeconds},
    wire::{
        command::{CommandErrorKind, EchoResult},
        log::Level,
    },
};

/// Echoes request content after the optional test-only randomized delay.
pub(super) async fn echo(request: EchoRequest) -> Result<EchoResult, CommandError> {
    if request.random_sleep {
        let sleep_ms = fastrand::u64(10..500);
        tokio::time::sleep(tokio::time::Duration::from_millis(sleep_ms)).await;
    }
    Ok(EchoResult {
        message: request.message,
    })
}

/// Collects process identity and host details used by the connected-agent view.
pub(super) async fn get_agent_details(
    loaded_config_path: &str,
) -> Result<AgentDetailsResponse, CommandError> {
    use sysinfo::System;

    let pid = std::process::id();
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    // Operators need the on-disk binary path when diagnosing upgrades and restarts.
    let (exe_path, external_ip) = tokio::join!(current_exe_path(), external_ip());
    // Empty when the agent was launched without a TOML file (CLI/env only).
    let config_path = loaded_config_path.to_string();

    // Load and uptime are cheap OS reads; a full `System` refresh would scan every process
    // on an async worker for nothing.
    let load_avg = System::load_average();
    let system_uptime = System::uptime();
    let host = HostIdentity::current();
    let mount_points = match crate::platform::mounts::capacities().await {
        Ok(mount_points) => mount_points,
        Err(message) => return Err(CommandError::new(CommandErrorKind::Internal, message)),
    };

    Ok(AgentDetailsResponse {
        id: AgentId::unchecked(""),
        name: String::new(),
        pid,
        cwd,
        config_path,
        exe_path,
        load_average_one: load_avg.one,
        load_average_five: load_avg.five,
        load_average_fifteen: load_avg.fifteen,
        system_uptime,
        os: host.os,
        arch: host.arch,
        hostname: host.hostname,
        external_ip,
        username: host.username,
        connected_at: UnixTimestampSeconds::new(0),
        // Agent process reports its own baked identity; router may also rewrite from registration.
        binary: current_binary_identity(),
        mount_points,
        uid: host.uid,
        is_root: host.is_root,
    })
}

/// Loads the user/group catalog only when a root details view needs owner/group selects.
pub(super) async fn list_accounts() -> Result<AgentAccountsResponse, CommandError> {
    match crate::platform::accounts::list().await {
        Ok(response) => Ok(response),
        Err(message) => Err(CommandError::new(CommandErrorKind::Internal, message)),
    }
}

/// Opens a path in the agent's graphical desktop session.
pub(super) async fn open_path(path: String) -> Result<(), CommandError> {
    match redoor_core::desktop::open_with_desktop(&path).await {
        Ok(()) => Ok(()),
        // Agent-facing wording: operators care that this host cannot open paths, not the launcher probe.
        Err(redoor_core::desktop::OpenError::NoDesktop) => Err(CommandError::new(
            CommandErrorKind::Internal,
            "Agent does not have access to a graphical desktop".to_string(),
        )),
        Err(error) => Err(CommandError::new(
            CommandErrorKind::Internal,
            error.to_string(),
        )),
    }
}

/// Changes logging admission for subsequent events in this agent process.
pub(super) fn set_logging_level(level: Level) -> Result<Level, CommandError> {
    redoor_core::logging::set_level(level);
    Ok(level)
}

/// Validates a replacement binary path and returns it; the re-exec itself runs after the
/// response is sent.
pub(super) fn self_exec(path: String) -> Result<std::path::PathBuf, CommandError> {
    if std::path::Path::new(&path).is_absolute() {
        Ok(std::path::PathBuf::from(path))
    } else {
        Err(CommandError::new(
            CommandErrorKind::InvalidInput,
            "Self-exec path must be absolute",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies self-exec requires an absolute replacement path.
    #[test]
    fn self_exec_rejects_relative_paths() {
        // A relative replacement path could resolve differently after an agent restart.
        assert!(matches!(
            self_exec("relative-agent".to_string()),
            Err(CommandError {
                kind: CommandErrorKind::InvalidInput,
                ..
            })
        ));
    }
}
