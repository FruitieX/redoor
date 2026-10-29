//! Runs the repository's `install.sh`, the one implementation that downloads redoor releases.
//!
//! `redoor upgrade`, the server's update endpoint and the server's release cache for agent
//! upgrades and SSH provisioning all fetch releases through this script, so asset names,
//! release URLs and extraction exist in exactly one place, which is also the documented
//! `curl | bash` installer.

use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::process::Command;

/// The repository's `install.sh`, run with bash.
const INSTALL_SCRIPT: &str = include_str!("../../../../install.sh");
/// Directory `install.sh` writes the binary into.
const INSTALL_DIR_ENV: &str = "REDOOR_INSTALL_DIR";
/// Release mirror `install.sh` downloads from instead of GitHub.
const RELEASE_BASE_URL_ENV: &str = "REDOOR_RELEASE_BASE_URL";
/// Operating system of the binary `install.sh` fetches instead of the host's.
const TARGET_OS_ENV: &str = "REDOOR_TARGET_OS";
/// CPU architecture of the binary `install.sh` fetches instead of the host's.
const TARGET_ARCH_ENV: &str = "REDOOR_TARGET_ARCH";
/// Output kept from a captured run; the script prints a handful of lines.
const OUTPUT_LIMIT: usize = 16 * 1024;
/// Longest accepted release tag.
const TAG_MAX_LENGTH: usize = 64;

/// Platform of a release asset, named like the assets (`linux`, `aarch64`, ...).
#[derive(Debug, Clone)]
pub struct ReleaseTarget {
    /// `linux`, `macos` or `android`.
    pub os: String,
    /// `x86_64` or `aarch64`.
    pub arch: String,
}

/// Why installing a release failed.
#[derive(Debug, thiserror::Error)]
pub enum ReleaseInstallError {
    /// bash could not be started or waited for.
    #[error("Failed to run bash, which installing a release requires: {0}")]
    Spawn(#[source] std::io::Error),
    /// The script ran and failed; its output says why.
    #[error("Installing the release failed ({status}):\n{output}")]
    Failed {
        /// Exit status of the script.
        status: ExitStatus,
        /// End of the script's combined output.
        output: String,
    },
}

/// One run of `install.sh`.
#[derive(Debug, Clone)]
pub struct ReleaseInstall {
    /// Release tag such as `v1.3.1`; the latest release when `None`.
    pub tag: Option<String>,
    /// Directory that receives the `redoor` binary.
    pub install_dir: PathBuf,
    /// Platform to fetch; the host's when `None`.
    pub target: Option<ReleaseTarget>,
    /// Where the script keeps its temporary download, so a caller can clean up after a kill.
    pub temp_dir: Option<PathBuf>,
    /// Release mirror replacing GitHub; the process environment decides when `None`.
    pub base_url: Option<String>,
}

impl ReleaseInstall {
    /// Accepts tags that are safe both as an argv word and as a release URL segment.
    ///
    /// A leading `-` would also be parsed as an option by the CLI.
    pub fn is_valid_tag(tag: &str) -> bool {
        !tag.is_empty()
            && tag.len() <= TAG_MAX_LENGTH
            && !tag.starts_with('-')
            && tag
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    }

    /// The bash command running the script; stdio is inherited unless the caller changes it.
    pub fn command(&self) -> Command {
        let mut command = Command::new("bash");
        // `-c` takes the script text; the next word becomes `$0` and the rest its arguments.
        command.arg("-c").arg(INSTALL_SCRIPT).arg("redoor-install");
        if let Some(tag) = &self.tag {
            command.arg(tag);
        }
        command.env(INSTALL_DIR_ENV, &self.install_dir);
        if let Some(target) = &self.target {
            command
                .env(TARGET_OS_ENV, &target.os)
                .env(TARGET_ARCH_ENV, &target.arch);
        }
        if let Some(temp_dir) = &self.temp_dir {
            command.env("TMPDIR", temp_dir);
        }
        if let Some(base_url) = &self.base_url {
            command.env(RELEASE_BASE_URL_ENV, base_url);
        }
        command
    }

    /// Runs the script with captured output and returns that output.
    ///
    /// The script runs in its own process group, killed when this future is dropped, so a
    /// cancelled install also stops the `curl` and `tar` it started instead of leaving them
    /// writing into a directory the caller is removing.
    pub async fn run(&self) -> Result<String, ReleaseInstallError> {
        let mut command = self.command();
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let child = command.spawn().map_err(ReleaseInstallError::Spawn)?;
        let mut group = ProcessGroupGuard {
            leader: child.id().and_then(|pid| i32::try_from(pid).ok()),
        };
        let output = child
            .wait_with_output()
            .await
            .map_err(ReleaseInstallError::Spawn)?;
        // The leader was reaped, so its group ID may be reused and must not be signalled.
        group.leader = None;
        let text = Self::bounded_output(&output.stdout, &output.stderr);
        if !output.status.success() {
            return Err(ReleaseInstallError::Failed {
                status: output.status,
                output: text,
            });
        }
        Ok(text)
    }

    /// Joins stdout and stderr, keeping the end, where the script reports why it failed.
    fn bounded_output(stdout: &[u8], stderr: &[u8]) -> String {
        let mut text = String::from_utf8_lossy(stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(stderr));
        let text = text.trim();
        if text.len() <= OUTPUT_LIMIT {
            return text.to_string();
        }
        let mut start = text.len() - OUTPUT_LIMIT;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        format!("...{}", &text[start..])
    }
}

/// Kills a still-running script's whole process group when the install is abandoned.
struct ProcessGroupGuard {
    /// Group leader PID, cleared once it was reaped.
    leader: Option<i32>,
}

impl Drop for ProcessGroupGuard {
    /// SIGKILL because a cancelled download has nothing worth shutting down cleanly.
    fn drop(&mut self) {
        if let Some(leader) = self.leader {
            let _ = killpg(Pid::from_raw(leader), Signal::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tags end up in argv and in the release URL, so only plain release names pass.
    #[test]
    fn accepts_only_plain_release_tags() {
        // Real release tags are accepted.
        assert!(ReleaseInstall::is_valid_tag("v1.3.1"));
        assert!(ReleaseInstall::is_valid_tag("v2.0.0-rc.1"));
        // A leading dash would be parsed as a CLI option.
        assert!(!ReleaseInstall::is_valid_tag("--no-restart"));
        // Path and shell metacharacters could escape the release URL.
        assert!(!ReleaseInstall::is_valid_tag("../v1"));
        assert!(!ReleaseInstall::is_valid_tag("v1 v2"));
        // An empty tag would download from the releases root.
        assert!(!ReleaseInstall::is_valid_tag(""));
    }

    /// Long output keeps its end, where install.sh reports why it failed.
    #[test]
    fn keeps_the_end_of_long_output() {
        let stdout = "x".repeat(OUTPUT_LIMIT * 2);
        let text = ReleaseInstall::bounded_output(stdout.as_bytes(), b"install: error: boom");
        // The failure reason at the end survives the truncation.
        assert!(text.ends_with("install: error: boom"));
        // The kept text is bounded so responses and errors stay small.
        assert!(text.len() <= OUTPUT_LIMIT + 3);
    }
}
