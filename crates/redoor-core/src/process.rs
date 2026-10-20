//! Process identity markers and in-place re-exec shared by every process role.

use std::{ffi::OsString, path::Path};

/// Replaces this process with the same executable and arguments so supervisors keep tracking it.
pub fn reexec_current_process() -> ! {
    let exe = std::env::current_exe().unwrap_or_else(|error| {
        eprintln!("restart: failed to resolve current executable: {error}");
        std::process::exit(1);
    });
    reexec_process(&exe)
}

/// Replaces this process from an explicit installed path after an atomic binary upgrade.
pub fn reexec_process(exe: &Path) -> ! {
    use std::os::unix::process::CommandExt;

    // Command supplies argv[0], so only the original arguments are forwarded.
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();

    let error = std::process::Command::new(exe).args(&args).exec();
    eprintln!("restart: exec failed for {}: {error}", exe.display());
    std::process::exit(1);
}

/// Internal marker used by the server because managed agents have their own watchdog lifecycle.
pub const MANAGED_AGENT_ENV: &str = "REDOOR_MANAGED_AGENT";

/// Marks a detached child so it does not die with the short-lived CLI that spawned it.
pub const DETACHED_ENV: &str = "REDOOR_DETACHED";

/// Long-lived process that owns a PID file and optional daemon mode.
///
/// Lives in core (not the binary) because the server's managed-agent launcher and the
/// process-control commands must agree on the PID and log file names of each role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessSlot {
    /// Standalone `redoor agent` process.
    Agent,
    /// `redoor server` process.
    Server,
}

impl ProcessSlot {
    /// Basename for PID and default log files (`agent`, `server`, `relay`).
    pub fn file_stem(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Server => "server",
        }
    }

    /// CLI path after `redoor` used in operator-facing stop guidance.
    pub fn command_path(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Server => "server",
        }
    }

    /// Managed local agents skip PID files so they do not fight the standalone agent lock.
    pub fn skips_pid_when_managed(self) -> bool {
        matches!(self, Self::Agent) && std::env::var_os(MANAGED_AGENT_ENV).is_some()
    }
}
