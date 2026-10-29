//! Installs Redoor as a per-user macOS LaunchAgent.
//!
//! This module intentionally has no root or LaunchDaemon support. Services run
//! only in the invoking user's GUI launchd domain and keep all state under home.
//! It is compiled on Linux too so its logic stays type-checked and unit-tested there.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use nix::sys::signal::kill;
use nix::unistd::Pid;
use redoor_core::paths::AppPaths;
use redoor_core::subprocess::{SubprocessError, run_checked};
use tokio::process::Command;
use tokio::time::Instant;

use super::{ServiceBackend, ServiceRole, ServiceRuntime, ServiceState, not_installed};

/// How long launchd may take to finish removing a booted-out job.
///
/// launchd escalates from SIGTERM to SIGKILL after the job's `ExitTimeOut`, 20 seconds by
/// default, so this leaves room above that before giving up.
const UNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// How often launchd is asked whether a job finished unloading or a bootstrap may be retried.
const LAUNCHD_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// launchctl's exit status when bootstrap collides with a job that is loaded or still being
/// removed; launchctl prints it as `Bootstrap failed: 5: Input/output error`.
const BOOTSTRAP_CONFLICT_STATUS: i32 = 5;

/// launchd control for one role's LaunchAgent in the current user's GUI domain.
pub(super) struct LaunchdBackend {
    /// Role the agent runs; selects the subcommand and label.
    role: ServiceRole,
    /// `<app-name>-<role>`, or `<app-name>-relay-<id>` for relays.
    label: String,
    /// Shows launchctl output instead of capturing it.
    verbose: bool,
    /// Namespace and home used for the plist location and pinned config.
    paths: AppPaths,
}

impl LaunchdBackend {
    /// Resolves the label; rejects root because only user LaunchAgents are supported.
    pub(super) fn new(role: ServiceRole, paths: &AppPaths, verbose: bool) -> Result<Self> {
        if nix::unistd::Uid::effective().is_root() {
            bail!(
                "redoor launchd does not support root; run it as the user who will own the LaunchAgent"
            );
        }
        Ok(Self {
            label: service_name(&role, paths.app_name()),
            role,
            verbose,
            paths: paths.clone(),
        })
    }

    /// Fully qualified launchctl target for this label.
    fn target(&self) -> String {
        format!("{}/{}", user_domain(), self.label)
    }

    /// Conventional plist path for this label.
    fn plist_path(&self) -> Result<PathBuf> {
        Ok(self
            .launch_agents_directory()?
            .join(format!("{}.plist", self.label)))
    }

    /// `~/Library/LaunchAgents`.
    fn launch_agents_directory(&self) -> Result<PathBuf> {
        Ok(self
            .paths
            .home("the macOS LaunchAgents directory")?
            .join("Library/LaunchAgents"))
    }

    /// Whether the plist exists on disk.
    async fn plist_exists(&self) -> Result<bool> {
        let path = self.plist_path()?;
        tokio::fs::try_exists(&path)
            .await
            .with_context(|| format!("Failed to inspect LaunchAgent '{}'", path.display()))
    }

    /// Runs one launchctl administration command.
    async fn launchctl(&self, arguments: &[&str]) -> Result<()> {
        Ok(run_checked("launchctl", arguments, self.verbose).await?)
    }

    /// Checks whether launchd currently has the service loaded.
    async fn is_loaded(&self) -> Result<bool> {
        let mut command = Command::new("launchctl");
        command.args(["print", &self.target()]);
        if !self.verbose {
            command.stdout(std::process::Stdio::null());
            command.stderr(std::process::Stdio::null());
        }
        let status = command
            .status()
            .await
            .context("Failed to ask launchd whether the service is loaded")?;
        Ok(status.success())
    }

    /// Queries the loaded job's process state; `None` when it is not loaded.
    async fn loaded_state(&self) -> Result<Option<LaunchdProcessState>> {
        let output = Command::new("launchctl")
            .args(["print", &self.target()])
            .output()
            .await
            .context("Failed to ask launchd for the service state")?;
        if !output.status.success() {
            return Ok(None);
        }
        Ok(Some(parse_process_state(&String::from_utf8_lossy(
            &output.stdout,
        ))))
    }

    /// Queries launchd's persistent disabled override when the domain supports it.
    async fn is_enabled(&self) -> Result<Option<bool>> {
        let output = Command::new("launchctl")
            .args(["print-disabled", &user_domain()])
            .output()
            .await
            .context("Failed to ask launchd for disabled overrides")?;
        if !output.status.success() {
            return Ok(None);
        }
        Ok(parse_enabled_override(
            &self.label,
            &String::from_utf8_lossy(&output.stdout),
        ))
    }

    /// Loads the plist into the user's GUI domain, which also starts it (`RunAtLoad`).
    ///
    /// A job booted out by an earlier command can still be shutting down, which launchd
    /// rejects with a conflict, so that conflict is retried until the old job is gone. The
    /// result is verified because launchctl reporting success is not proof the job stayed.
    async fn bootstrap(&self) -> Result<()> {
        let plist = self.plist_path()?;
        let plist = plist.to_string_lossy();
        let domain = user_domain();
        let deadline = Instant::now() + UNLOAD_TIMEOUT;
        loop {
            let Err(error) =
                run_checked("launchctl", &["bootstrap", &domain, &plist], self.verbose).await
            else {
                break;
            };
            if !self.bootstrap_may_succeed_later(&error, deadline).await? {
                return Err(error.into());
            }
            tokio::time::sleep(LAUNCHD_POLL_INTERVAL).await;
        }
        if self.loaded_state().await?.is_none() {
            bail!(
                "launchctl bootstrap reported success but launchd did not keep '{}' loaded; inspect it with `launchctl print {}`",
                self.label,
                self.target()
            );
        }
        Ok(())
    }

    /// Whether a failed bootstrap is the transient conflict with a job still being removed.
    ///
    /// A loaded job or a disabled label produce the same status permanently, so those fail
    /// immediately instead of stalling until the deadline.
    async fn bootstrap_may_succeed_later(
        &self,
        error: &SubprocessError,
        deadline: Instant,
    ) -> Result<bool> {
        Ok(is_bootstrap_conflict(error)
            && Instant::now() < deadline
            && self.loaded_state().await?.is_none()
            && self.is_enabled().await? != Some(false))
    }

    /// Unloads the job and waits until launchd has finished stopping its process.
    async fn bootout(&self) -> Result<()> {
        let pid = self.loaded_pid().await?;
        self.launchctl(&["bootout", &self.target()]).await?;
        self.wait_until_unloaded(pid).await
    }

    /// PID of the job's running process, so its exit can be awaited after a bootout.
    async fn loaded_pid(&self) -> Result<Option<i32>> {
        Ok(self
            .loaded_state()
            .await?
            .and_then(|state| state.pid)
            .and_then(|pid| pid.parse().ok()))
    }

    /// Waits until launchd no longer lists the job and its old process has exited.
    ///
    /// `launchctl bootout` can return while launchd is still terminating the process. A
    /// bootstrap in that window either fails with an I/O error or is accepted and then
    /// discarded when the pending removal completes, leaving the service silently unloaded.
    /// The listing alone is not enough because it can disappear before the process exits.
    async fn wait_until_unloaded(&self, pid: Option<i32>) -> Result<()> {
        let waited = tokio::time::timeout(UNLOAD_TIMEOUT, async {
            let mut interval = tokio::time::interval(LAUNCHD_POLL_INTERVAL);
            loop {
                interval.tick().await;
                // EPERM would mean the PID now belongs to another user, so only success
                // means the old process is still around.
                let running = pid.is_some_and(|pid| kill(Pid::from_raw(pid), None).is_ok());
                if !running && self.loaded_state().await?.is_none() {
                    return Ok(());
                }
            }
        })
        .await;
        match waited {
            Ok(result) => result,
            Err(_) => bail!(
                "launchd did not finish unloading '{}' within {}s",
                self.label,
                UNLOAD_TIMEOUT.as_secs()
            ),
        }
    }

    /// Gives an ad-hoc-signed executable a fresh identity so macOS prompts again.
    pub(super) async fn refresh_local_network_permission(&self) -> Result<()> {
        self.ensure_installed().await?;
        let plist_path = self.plist_path()?;
        let binary_output = run_logged_output(
            "plutil",
            &[
                "-extract".to_string(),
                "ProgramArguments.0".to_string(),
                "raw".to_string(),
                "-o".to_string(),
                "-".to_string(),
                plist_path.to_string_lossy().into_owned(),
            ],
        )
        .await?;
        let binary = PathBuf::from(String::from_utf8(binary_output.stdout)?.trim());
        if binary.as_os_str().is_empty() {
            bail!(
                "LaunchAgent '{}' does not contain an executable path",
                plist_path.display()
            );
        }

        let binary_argument = binary.to_string_lossy().into_owned();
        let signature =
            run_logged_output("codesign", &["-dvv".to_string(), binary_argument]).await?;
        if signature_has_team_identifier(&String::from_utf8_lossy(&signature.stderr)) {
            bail!(
                "Refusing to replace the Developer ID signature on '{}'",
                binary.display()
            );
        }

        let temporary = local_network_signature_temp_path(&binary)?;
        let replaced = self.replace_signed_binary(&binary, &temporary).await;
        if replaced.is_err() {
            let _ = tokio::fs::remove_file(&temporary).await;
        }
        replaced?;
        run_logged_command(
            "launchctl",
            &[
                "bootstrap".to_string(),
                user_domain(),
                plist_path.to_string_lossy().into_owned(),
            ],
        )
        .await?;
        println!("Select Allow when macOS asks whether Redoor may find devices on local networks.");
        Ok(())
    }

    /// Signs a copy, stops the service and atomically swaps the copy into place.
    ///
    /// The replacement is verified before the running service is stopped, so a signing
    /// failure leaves the installed service untouched.
    async fn replace_signed_binary(&self, binary: &Path, temporary: &Path) -> Result<()> {
        prepare_local_network_signature(binary, temporary).await?;
        let target = self.target();
        let pid = self.loaded_pid().await?;
        let loaded = run_logged_status("launchctl", &["print".to_string(), target.clone()])
            .await?
            .success();
        if loaded {
            run_logged_command("launchctl", &["bootout".to_string(), target]).await?;
            // The caller bootstraps right after the swap, which races an unfinished removal.
            self.wait_until_unloaded(pid).await?;
        }
        tokio::fs::rename(temporary, binary).await.with_context(|| {
            format!(
                "Failed to atomically replace '{}' with the refreshed executable",
                binary.display()
            )
        })
    }
}

impl ServiceBackend for LaunchdBackend {
    /// Used in `redoor <role> launchd ...` guidance.
    fn manager(&self) -> &'static str {
        "launchd"
    }

    /// Matches the established `LaunchAgent ...` status prefix.
    fn describe(&self) -> String {
        format!("LaunchAgent '{}'", self.label)
    }

    /// Enabled LaunchAgents start when the user logs in.
    fn autostart_trigger(&self) -> &'static str {
        "login"
    }

    /// Renders the plist with this label and the pinned config.
    fn render_definition(&self, binary: &Path, config_path: &Path) -> String {
        render_plist(
            &self.role,
            &self.label,
            binary,
            config_path,
            self.paths.app_name(),
        )
    }

    /// Unloads an older definition first so launchd never keeps a stale plist in memory.
    async fn install(&self, config_path: &Path, start: bool) -> Result<PathBuf> {
        let binary = super::current_binary().await?;
        let content = self.render_definition(&binary, config_path);
        let directory = self.launch_agents_directory()?;
        tokio::fs::create_dir_all(&directory)
            .await
            .with_context(|| {
                format!(
                    "Failed to create LaunchAgents directory '{}'",
                    directory.display()
                )
            })?;
        let plist_path = self.plist_path()?;
        if self.is_loaded().await? {
            self.bootout()
                .await
                .context("Failed to unload the previous LaunchAgent definition")?;
        }
        super::atomic_write(&plist_path, content.as_bytes(), "LaunchAgent").await?;
        // A disabled selection survives plist discovery, so explicitly mark this label
        // enabled while leaving it unloaded until the operator starts it or logs in.
        self.enable()
            .await
            .context("Failed to enable the LaunchAgent for user login")?;
        if start {
            self.bootstrap()
                .await
                .context("Failed to start the newly installed LaunchAgent")?;
        }
        Ok(plist_path)
    }

    /// Reports success when either the plist or a loaded job existed.
    async fn uninstall(&self) -> Result<bool> {
        let loaded = self.is_loaded().await?;
        if loaded {
            self.bootout()
                .await
                .context("Failed to stop and unload the LaunchAgent")?;
        }
        let installed = self.plist_exists().await?;
        if installed {
            let path = self.plist_path()?;
            tokio::fs::remove_file(&path)
                .await
                .with_context(|| format!("Failed to remove LaunchAgent '{}'", path.display()))?;
        }
        // launchctl has no override-deletion command. Marking the absent label enabled
        // ensures a later reinstall cannot inherit a previous disabled selection.
        self.enable()
            .await
            .context("Failed to enable the LaunchAgent label for a future installation")?;
        Ok(installed || loaded)
    }

    /// The plist on disk is the source of truth for LaunchAgents.
    async fn ensure_installed(&self) -> Result<()> {
        if self.plist_exists().await? {
            return Ok(());
        }
        Err(not_installed(
            &format!("macOS LaunchAgent '{}'", self.label),
            &self.role,
            "launchd",
        ))
    }

    /// Kickstarts a loaded job, bootstraps an unloaded one, and leaves a running one alone.
    async fn start(&self) -> Result<()> {
        match self.loaded_state().await? {
            Some(state) if state.running => Ok(()),
            Some(_) => self.launchctl(&["kickstart", &self.target()]).await,
            None => self.bootstrap().await,
        }
    }

    /// Unloading is how launchd stops a job; the plist stays for the next login.
    async fn stop(&self) -> Result<()> {
        if self.is_loaded().await? {
            self.bootout().await?;
        }
        Ok(())
    }

    /// Bootout + bootstrap re-reads the plist.
    async fn restart(&self) -> Result<()> {
        if self.is_loaded().await? {
            self.bootout()
                .await
                .context("Failed to unload the running LaunchAgent before restart")?;
        }
        self.bootstrap().await
    }

    /// Clears the disabled override for this label.
    async fn enable(&self) -> Result<()> {
        self.launchctl(&["enable", &self.target()]).await
    }

    /// Sets the disabled override, optionally unloading the job now.
    async fn disable(&self, now: bool) -> Result<()> {
        self.launchctl(&["disable", &self.target()])
            .await
            .context("Failed to disable the LaunchAgent at login")?;
        if now && self.is_loaded().await? {
            self.bootout().await?;
        }
        Ok(())
    }

    /// Combines the plist, `launchctl print` and the disabled overrides into the shared state.
    async fn status(&self) -> Result<ServiceState> {
        let installed = self.plist_exists().await?;
        let runtime = match self.loaded_state().await? {
            None => ServiceRuntime::Unloaded,
            Some(state) if state.running => ServiceRuntime::Running { pid: state.pid },
            Some(state) => ServiceRuntime::Stopped {
                detail: state.state,
            },
        };
        let enabled = if installed {
            self.is_enabled().await?
        } else {
            None
        };
        Ok(ServiceState {
            installed,
            enabled,
            runtime,
        })
    }
}

/// Derives a safe launchd label from the already validated installation identity.
fn service_name(role: &ServiceRole, app_name: &str) -> String {
    format!("{app_name}-{}", role.service_suffix())
}

/// Returns the current user's launchd GUI domain.
fn user_domain() -> String {
    format!("gui/{}", nix::unistd::Uid::effective().as_raw())
}

/// Whether launchctl rejected a bootstrap because the label is loaded or still being removed.
fn is_bootstrap_conflict(error: &SubprocessError) -> bool {
    matches!(
        error,
        SubprocessError::Failed { status, .. } if status.code() == Some(BOOTSTRAP_CONFLICT_STATUS)
    )
}

/// The process details launchd reports for a loaded service.
#[derive(Debug, Eq, PartialEq)]
struct LaunchdProcessState {
    /// Distinguishes an active process from a loaded job waiting to run.
    running: bool,
    /// Reports the active process ID when launchd includes one.
    pid: Option<String>,
    /// Retains launchd's state word for concise stopped-state diagnostics.
    state: Option<String>,
}

/// Parses stable launchctl state properties without depending on output layout.
fn parse_process_state(print_output: &str) -> LaunchdProcessState {
    let property = |name: &str| {
        print_output
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix(&format!("{name} = ")))
    };
    let state = property("state").map(str::to_owned);
    LaunchdProcessState {
        running: state.as_deref() == Some("running"),
        pid: property("pid").map(str::to_owned),
        state,
    }
}

/// Interprets current and older launchd enablement values for one exact label.
fn parse_enabled_override(service: &str, output: &str) -> Option<bool> {
    for line in output.lines() {
        let Some((label, value)) = line.split_once("=>") else {
            continue;
        };
        let Some(label) = label
            .trim()
            .strip_prefix('"')
            .and_then(|label| label.strip_suffix('"'))
        else {
            continue;
        };
        if label != service {
            continue;
        }
        return match value.trim() {
            "enabled" | "false" => Some(true),
            "disabled" | "true" => Some(false),
            _ => None,
        };
    }
    // Labels without a disabled selection are enabled by launchd's default policy.
    Some(true)
}

/// Distinguishes publisher-signed releases from ad-hoc signatures safely replaceable here.
fn signature_has_team_identifier(details: &str) -> bool {
    details.lines().any(|line| {
        line.strip_prefix("TeamIdentifier=")
            .is_some_and(|team| !team.is_empty() && team != "not set")
    })
}

/// Creates and verifies the re-signed copy before the running service is stopped.
async fn prepare_local_network_signature(binary: &Path, temporary: &Path) -> Result<()> {
    tokio::fs::copy(binary, temporary).await.with_context(|| {
        format!(
            "Failed to copy '{}' to temporary executable '{}'",
            binary.display(),
            temporary.display()
        )
    })?;
    // A fresh identifier is what makes macOS treat the binary as a new app and ask again.
    let identifier = format!("local.redoor.network.{:016x}", fastrand::u64(..));
    let temporary_argument = temporary.to_string_lossy().into_owned();
    run_logged_command(
        "codesign",
        &[
            "--force".to_string(),
            "--sign".to_string(),
            "-".to_string(),
            "--identifier".to_string(),
            identifier,
            temporary_argument.clone(),
        ],
    )
    .await?;
    run_logged_command(
        "codesign",
        &[
            "--verify".to_string(),
            "--verbose=2".to_string(),
            temporary_argument,
        ],
    )
    .await?;
    tokio::fs::OpenOptions::new()
        .read(true)
        .open(temporary)
        .await
        .with_context(|| format!("Failed to open signed executable '{}'", temporary.display()))?
        .sync_all()
        .await
        .with_context(|| format!("Failed to sync signed executable '{}'", temporary.display()))
}

/// Places the replacement beside the destination so the final rename is atomic.
fn local_network_signature_temp_path(binary: &Path) -> Result<PathBuf> {
    let file_name = binary
        .file_name()
        .context("LaunchAgent executable path has no file name")?
        .to_string_lossy();
    Ok(binary.with_file_name(format!(
        ".{file_name}.local-network.{}.{}",
        std::process::id(),
        fastrand::u64(..)
    )))
}

/// Renders a LaunchAgent plist with separately escaped program arguments.
fn render_plist(
    role: &ServiceRole,
    service: &str,
    binary: &Path,
    config_path: &Path,
    app_name: &str,
) -> String {
    // Each argument is its own element, so relay IDs need XML escaping but no shell quoting.
    let role_arguments: String = role
        .program_arguments()
        .into_iter()
        .map(|argument| {
            format!(
                "
        <string>{}</string>",
                escape_xml(argument)
            )
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>{role_arguments}
        <string>--config</string>
        <string>{}</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>REDOOR_APP_NAME</key>
        <string>{}</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
        escape_xml(service),
        escape_xml(binary.to_string_lossy().as_ref()),
        escape_xml(config_path.to_string_lossy().as_ref()),
        escape_xml(app_name)
    )
}

/// Escapes dynamic text embedded in XML element content.
fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Runs a visible command used by the operator-requested permission repair.
async fn run_logged_command(program: &str, arguments: &[String]) -> Result<()> {
    println!("Executing: {}", format_command(program, arguments));
    let status = Command::new(program)
        .args(arguments)
        .status()
        .await
        .with_context(|| format!("Failed to run {program}"))?;
    if status.success() {
        return Ok(());
    }
    bail!(
        "{} failed with {status}",
        format_command(program, arguments)
    )
}

/// Captures command output while still showing exactly what Redoor executes.
async fn run_logged_output(program: &str, arguments: &[String]) -> Result<std::process::Output> {
    println!("Executing: {}", format_command(program, arguments));
    let output = Command::new(program)
        .args(arguments)
        .output()
        .await
        .with_context(|| format!("Failed to run {program}"))?;
    if output.status.success() {
        return Ok(output);
    }
    bail!(
        "{} failed with {}: {}",
        format_command(program, arguments),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

/// Returns status for a logged probe whose non-zero result can be expected state.
async fn run_logged_status(
    program: &str,
    arguments: &[String],
) -> Result<std::process::ExitStatus> {
    println!("Executing: {}", format_command(program, arguments));
    Command::new(program)
        .args(arguments)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .with_context(|| format!("Failed to run {program}"))
}

/// Formats logged commands unambiguously without relying on shell interpolation.
fn format_command(program: &str, arguments: &[String]) -> String {
    std::iter::once(program.to_string())
        .chain(arguments.iter().map(|argument| format!("{argument:?}")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies labels derive solely from the validated application identity.
    #[test]
    fn service_names_use_application_identity() {
        assert_eq!(
            service_name(&ServiceRole::Agent, "redoor-dev"),
            "redoor-dev-agent",
            "the agent label should use the validated application namespace"
        );
        assert_eq!(
            service_name(&ServiceRole::Relay("production".into()), "redoor-dev"),
            "redoor-dev-relay-production",
            "each relay needs its own label so several can be installed side by side"
        );
    }

    /// Verifies generated plists preserve arguments, namespaces, and restart behavior.
    #[test]
    fn agent_plist_pins_config_and_environment() {
        let plist = render_plist(
            &ServiceRole::Agent,
            "redoor-agent",
            Path::new("/Users/test & user/bin/redoor"),
            Path::new("/Users/test & user/.config/redoor/config.toml"),
            "redoor-dev",
        );
        assert!(
            plist.contains("<string>/Users/test &amp; user/bin/redoor</string>"),
            "the executable must be XML escaped as a distinct argument: {plist}"
        );
        assert!(
            plist.contains("<string>agent</string>"),
            "the LaunchAgent must run the selected process role: {plist}"
        );
        assert!(
            plist.contains("<string>/Users/test &amp; user/.config/redoor/config.toml</string>"),
            "the LaunchAgent must pin the prepared config path: {plist}"
        );
        assert!(
            plist.contains("<key>REDOOR_APP_NAME</key>")
                && plist.contains("<string>redoor-dev</string>"),
            "the LaunchAgent must preserve the selected application namespace: {plist}"
        );
        assert!(
            plist.contains("<key>SuccessfulExit</key>") && plist.contains("<false/>"),
            "launchd should restart only processes that exit unsuccessfully: {plist}"
        );
        assert!(
            !plist.contains("--token") && !plist.contains("--name"),
            "standalone-agent settings must come from the shared TOML: {plist}"
        );
    }

    /// Verifies relay plists run the named relay in the foreground under launchd supervision.
    #[test]
    fn relay_plist_starts_named_relay() {
        let plist = render_plist(
            &ServiceRole::Relay("production".into()),
            "redoor-relay-production",
            Path::new("/Users/test/bin/redoor"),
            Path::new("/Users/test/.config/redoor/config.toml"),
            "redoor",
        );
        assert!(
            plist.contains(
                "<string>/Users/test/bin/redoor</string>
        <string>agent</string>
        <string>relay</string>
        <string>start</string>
        <string>production</string>
        <string>--config</string>"
            ),
            "the LaunchAgent must start the selected relay as distinct arguments: {plist}"
        );
        assert!(
            !plist.contains("--daemon"),
            "launchd must supervise the relay process itself: {plist}"
        );
    }

    /// Verifies every XML-sensitive character is escaped in generated values.
    #[test]
    fn xml_values_are_escaped() {
        assert_eq!(
            escape_xml("<&>\"'"),
            "&lt;&amp;&gt;&quot;&apos;",
            "dynamic plist values must not be able to alter the XML structure"
        );
    }

    /// Keeps parsed launchctl output explicit about the difference between loaded and running.
    #[test]
    fn parses_launchd_process_state() {
        assert_eq!(
            parse_process_state(
                "state = running
pid = 1234"
            ),
            LaunchdProcessState {
                running: true,
                pid: Some("1234".to_string()),
                state: Some("running".to_string()),
            },
            "a running service should include its process ID"
        );
        assert!(
            !parse_process_state("state = waiting").running,
            "a registered service without a process must not be described as running"
        );
    }

    /// Only launchctl's conflict status may be retried; other failures must surface at once.
    #[test]
    fn recognizes_bootstrap_conflicts() {
        use std::os::unix::process::ExitStatusExt;

        let failed = |code: i32| SubprocessError::Failed {
            command: "launchctl bootstrap gui/501 /tmp/redoor-agent.plist".to_string(),
            status: std::process::ExitStatus::from_raw(code << 8),
            stderr: None,
        };
        assert!(
            is_bootstrap_conflict(&failed(BOOTSTRAP_CONFLICT_STATUS)),
            "the status launchctl reports while a booted-out job is still removed must be retried"
        );
        assert!(
            !is_bootstrap_conflict(&failed(1)),
            "unrelated launchctl failures must not be retried until the deadline"
        );
        assert!(
            !is_bootstrap_conflict(&SubprocessError::Spawn {
                program: "launchctl".to_string(),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            }),
            "a missing launchctl can never succeed later"
        );
    }

    /// Verifies real and older launchd values are matched by exact label.
    #[test]
    fn interprets_launchd_enablement_override() {
        let output = r#"disabled services = {
        "com.apple.example" => disabled
        "redoor-server-preview" => disabled
        "redoor-server"    =>    enabled
        "redoor-agent" => disabled
}"#;
        assert_eq!(
            parse_enabled_override("redoor-server", output),
            Some(true),
            "the enabled value emitted by current launchctl should be recognized"
        );
        assert_eq!(
            parse_enabled_override("redoor-agent", output),
            Some(false),
            "the disabled value emitted by current launchctl should be recognized"
        );
        assert_eq!(
            parse_enabled_override("redoor", output),
            Some(true),
            "a partial label match must not inherit another service's state"
        );
        let older_output = r#"disabled services = {
    "redoor-server" => true
    "redoor-agent" => false
}"#;
        assert_eq!(
            parse_enabled_override("redoor-server", older_output),
            Some(false),
            "the older true representation means the disabled selection is set"
        );
        assert_eq!(
            parse_enabled_override("redoor-agent", older_output),
            Some(true),
            "the older false representation means the disabled selection is clear"
        );
    }

    /// Ensures only a real signing team prevents the ad-hoc repair operation.
    #[test]
    fn detects_publisher_signed_executables() {
        assert!(
            signature_has_team_identifier(
                "Identifier=fi.example.redoor
TeamIdentifier=ABCDE12345"
            ),
            "a Developer ID team must protect the publisher signature from replacement"
        );
        assert!(
            !signature_has_team_identifier(
                "Identifier=redoor-123
TeamIdentifier=not set"
            ),
            "an ad-hoc signature should remain eligible for the permission repair"
        );
    }

    /// Keeps command logs explicit about argument boundaries and paths containing spaces.
    #[test]
    fn formats_executed_commands() {
        assert_eq!(
            format_command(
                "codesign",
                &[
                    "--verify".to_string(),
                    "/Users/test user/bin/redoor".to_string()
                ]
            ),
            "codesign \"--verify\" \"/Users/test user/bin/redoor\"",
            "logged commands should preserve every argument as a distinct quoted value"
        );
    }
}
