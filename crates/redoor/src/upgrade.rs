//! `redoor upgrade`: installs a release over the running binary with the shared installer,
//! then restarts the local redoor processes that still run the replaced binary.
//!
//! When this command runs inside one of those processes, for example in a web terminal of
//! an agent, the restarts are handed to a detached helper. The agent kills its terminals
//! and execs when its server connection drops, which restarting the server causes, and the
//! helper's own session keeps it out of the process groups that teardown signals.

use std::io::Write;
use std::process::{ExitCode, Stdio};

use anyhow::{Context, Result, bail};
use clap::Args;
use redoor_core::upgrade::SelfInstall;
use redoor_core::upgrade::install::ReleaseInstall;
use redoor_core::upgrade::restart::Restarter;
use redoor_protocol::api::server::{UpgradeProcessOutcome, UpgradeProcessReport};
use tokio::process::Command;

/// Options of `redoor upgrade`.
#[derive(Args)]
pub(crate) struct UpgradeArgs {
    /// Release tag to install, such as v1.3.1; the latest release when omitted.
    tag: Option<String>,
    /// Only replace the binary and leave running processes on the old version.
    #[arg(long, conflicts_with = "restart_pids")]
    no_restart: bool,
    /// Internal: restart these PIDs without installing, as the detached helper.
    #[arg(long, hide = true, value_delimiter = ',', conflicts_with = "tag")]
    restart_pids: Vec<u32>,
}

impl UpgradeArgs {
    /// Installs the release and restarts what still runs the old binary.
    pub(crate) async fn run(self) -> Result<ExitCode> {
        if !self.restart_pids.is_empty() {
            return Self::restart(Restarter::for_pids(&self.restart_pids).await?).await;
        }
        if let Some(tag) = &self.tag
            && !ReleaseInstall::is_valid_tag(tag)
        {
            bail!("Invalid release tag: {tag}");
        }
        let target = SelfInstall::resolve().await?;
        // Found before the install, which a failed download must not restart anything after.
        let restarter = if self.no_restart {
            None
        } else {
            Some(Restarter::discover(&target.installed).await?)
        };
        let status = ReleaseInstall {
            tag: self.tag,
            install_dir: target.install_dir,
            target: None,
            temp_dir: None,
            base_url: None,
        }
        .command()
        .status()
        .await
        .context("Failed to run bash, which installing a release requires")?;
        if !status.success() {
            bail!("Installing the release failed ({status})");
        }
        if target.installed != target.current {
            println!(
                "note: this command runs from '{}', but the release was installed to '{}'",
                target.current.display(),
                target.installed.display()
            );
        }
        let Some(restarter) = restarter else {
            println!("Running redoor processes keep the old version until they are restarted.");
            return Ok(ExitCode::SUCCESS);
        };
        if restarter.hosting_processes().next().is_some() {
            return Self::restart_detached(restarter).await;
        }
        Self::restart(restarter).await
    }

    /// Restarts the processes here, printing each outcome as it is known.
    async fn restart(restarter: Restarter) -> Result<ExitCode> {
        if restarter.pids().is_empty() {
            Self::report(format_args!(
                "No other running redoor process uses this binary."
            ));
            return Ok(ExitCode::SUCCESS);
        }
        let launchd = restarter.restarts_launchd_jobs();
        let reports = restarter.restart(true, Self::print).await;
        if launchd {
            Self::report(format_args!(
                "If a LaunchAgent can no longer reach the local network, run \
                 `redoor agent launchd refresh-local-network-permission` (see docs/launchd.md)."
            ));
        }
        let failed = reports
            .iter()
            .any(|report| matches!(report.outcome, UpgradeProcessOutcome::Failed { .. }));
        Ok(if failed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        })
    }

    /// Restarts the processes from a helper in its own session and waits for it.
    ///
    /// The helper is the upgraded binary told which PIDs to restart. If this process is killed
    /// while waiting, the helper carries on and its output goes wherever the terminal still
    /// lets it. The new session keeps the helper out of the process groups an agent kills when
    /// it tears down the terminal or exec that started this command.
    async fn restart_detached(restarter: Restarter) -> Result<ExitCode> {
        for (pid, command) in restarter.hosting_processes() {
            Self::report(format_args!(
                "This command runs inside PID {pid} ({command}); restarting it may end this \
                 session, and the restarts then continue in the background."
            ));
        }
        let pids = restarter
            .pids()
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let mut command = Command::new(redoor_core::executable::current()?);
        command
            .args(["upgrade", "--restart-pids", &pids])
            .stdin(Stdio::null());
        // SAFETY: this closure runs after fork and before exec, so it calls only `setsid`
        // and converts its errno without touching locks or shared runtime state.
        unsafe {
            command.pre_exec(|| {
                nix::unistd::setsid()
                    .map(|_| ())
                    .map_err(std::io::Error::from)
            });
        }
        let status = command
            .status()
            .await
            .context("Failed to start the detached restart helper")?;
        Ok(if status.success() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        })
    }

    /// Prints one restart outcome.
    fn print(report: &UpgradeProcessReport) {
        let pid = report.pid;
        let command = &report.command;
        match &report.outcome {
            UpgradeProcessOutcome::Restarted => {
                Self::report(format_args!("Restarted PID {pid}: {command}"))
            }
            UpgradeProcessOutcome::Restarting => {
                Self::report(format_args!("Restarting PID {pid}: {command}"))
            }
            UpgradeProcessOutcome::LeftRunning { hint } => Self::report(format_args!(
                "Left PID {pid} on the old version: {command}\n  {hint}"
            )),
            UpgradeProcessOutcome::Failed { error } => Self::report(format_args!(
                "Failed to restart PID {pid}: {command}\n  {error}"
            )),
        }
    }

    /// Writes one report line, ignoring a closed terminal.
    ///
    /// Restarting the agent that hosts this command closes its terminal, and the remaining
    /// restarts must not be aborted by the panic `println!` raises on a failed write.
    fn report(line: std::fmt::Arguments) {
        let _ = writeln!(std::io::stdout(), "{line}");
    }
}
