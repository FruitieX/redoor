//! Runs short administrative commands (systemctl, launchctl, useradd, ...) to completion.

use std::process::{ExitStatus, Stdio};

use tokio::process::Command;

/// Why a finite command failed; the message always names the full command line.
#[derive(Debug, thiserror::Error)]
pub enum SubprocessError {
    /// The program could not be started (missing binary, permissions).
    #[error("Failed to run {program}: {source}")]
    Spawn {
        /// Program that was requested.
        program: String,
        /// Underlying spawn failure.
        source: std::io::Error,
    },
    /// The program ran and exited unsuccessfully.
    #[error("{command} failed with {status}{}", stderr_suffix(.stderr))]
    Failed {
        /// Program and arguments, space separated.
        command: String,
        /// Exit status reported by the OS.
        status: ExitStatus,
        /// Trimmed stderr when it was captured (non-verbose runs).
        stderr: Option<String>,
    },
}

/// Formats captured stderr after the status, or nothing when output was shown live.
fn stderr_suffix(stderr: &Option<String>) -> String {
    match stderr {
        Some(stderr) => format!(": {stderr}"),
        None => String::new(),
    }
}

/// Runs `program args` and fails unless it exits successfully.
///
/// Non-verbose runs capture output so a failure carries the tool's own stderr explanation
/// while success stays quiet; verbose runs let the operator watch the tool directly, so the
/// error then only reports the status.
pub async fn run_checked(
    program: &str,
    args: &[&str],
    verbose: bool,
) -> Result<(), SubprocessError> {
    let mut command = Command::new(program);
    command.args(args);
    let spawn_error = |source| SubprocessError::Spawn {
        program: program.to_string(),
        source,
    };
    let (status, stderr) = if verbose {
        let status = command.status().await.map_err(spawn_error)?;
        (status, None)
    } else {
        let output = command
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(spawn_error)?;
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        (output.status, Some(stderr))
    };
    if status.success() {
        return Ok(());
    }
    Err(SubprocessError::Failed {
        command: std::iter::once(program)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" "),
        status,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A failing command reports the full command line and its stderr.
    #[tokio::test]
    async fn failure_includes_command_and_stderr() {
        let error = run_checked("sh", &["-c", "echo nope >&2; exit 3"], false)
            .await
            .expect_err("exit 3 must fail");
        let message = error.to_string();
        // Operators need to see which command failed and why.
        assert!(
            message.starts_with("sh -c echo nope >&2; exit 3 failed with"),
            "{message}"
        );
        assert!(message.ends_with(": nope"), "{message}");
    }

    /// A missing program is reported as a start failure, not an exit status.
    #[tokio::test]
    async fn missing_program_is_a_spawn_error() {
        let error = run_checked("redoor-definitely-missing-program", &[], false)
            .await
            .expect_err("a missing binary cannot run");
        // The distinct variant lets callers tell "not installed" from "failed".
        assert!(matches!(error, SubprocessError::Spawn { .. }));
    }
}
