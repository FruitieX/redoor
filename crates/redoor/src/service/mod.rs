//! Installs and controls Redoor as an operating-system service.
//!
//! The CLI surface (`redoor agent|server systemd|launchd ...`), the install/config checks,
//! the help text and the status line are shared; each service manager only implements
//! [`ServiceBackend`]. systemd is used on Linux and launchd (per-user LaunchAgents) on macOS.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod launchd;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod systemd;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::{Path, PathBuf};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use anyhow::Context;
use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use redoor_core::paths::AppPaths;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use tokio::io::AsyncWriteExt;

/// Process role selected by the parent CLI command for service management.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ServiceRole {
    /// Manage a standalone Redoor agent process.
    Agent,
    /// Manage the Redoor HTTP and WebSocket server process.
    Server,
}

impl ServiceRole {
    /// Returns the stable CLI and generated-service suffix for this role.
    #[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
    pub(crate) fn cli_name(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Server => "server",
        }
    }
}

/// Operations shared by Redoor's supported operating-system service managers.
#[derive(Subcommand)]
pub(crate) enum ServiceCommand {
    /// Install and enable the service, remaining stopped unless requested.
    #[command(after_help = crate::examples::SERVICE_INSTALL)]
    Install(InstallArgs),
    /// Stop, disable, and remove the service while preserving its config.
    #[command(after_help = crate::examples::SERVICE_UNINSTALL)]
    Uninstall,
    /// Start the installed service.
    #[command(after_help = crate::examples::SERVICE_START)]
    Start,
    /// Stop the installed service while leaving it enabled for future startup.
    #[command(after_help = crate::examples::SERVICE_STOP)]
    Stop,
    /// Reload and restart the installed service.
    #[command(after_help = crate::examples::SERVICE_RESTART)]
    Restart,
    /// Show installation, enablement, and process state.
    #[command(after_help = crate::examples::SERVICE_STATUS)]
    Status,
    /// Enable the installed service without starting it.
    #[command(after_help = crate::examples::SERVICE_ENABLE)]
    Enable,
    /// Disable automatic startup, optionally stopping the service now.
    #[command(after_help = crate::examples::SERVICE_DISABLE)]
    Disable(DisableArgs),
}

/// Controls whether installation also starts the service.
#[derive(Args)]
pub(crate) struct InstallArgs {
    /// Start the service after installing and enabling it.
    #[arg(long)]
    pub(crate) start: bool,
}

/// Controls whether disabling also stops the current service process.
#[derive(Args)]
pub(crate) struct DisableArgs {
    /// Stop the service in addition to disabling automatic startup.
    #[arg(long)]
    pub(crate) now: bool,
}

/// Arguments for role-scoped `redoor agent|server systemd` service management.
#[derive(Args)]
#[command(author, version, about)]
pub(crate) struct SystemdArgs {
    /// Selects the systemd operation while keeping service targeting consistent.
    #[command(subcommand)]
    command: ServiceCommand,
}

/// Arguments for role-scoped `redoor agent|server launchd` management.
#[derive(Args)]
#[command(author, version, about)]
pub(crate) struct LaunchdArgs {
    /// Show output from internal launchctl commands.
    #[arg(long, global = true)]
    verbose: bool,
    /// Selects the launchd operation while keeping service targeting consistent.
    #[command(subcommand)]
    command: LaunchdCommand,
}

/// launchd supports the shared operations plus a macOS-only permission repair.
#[derive(Subcommand)]
enum LaunchdCommand {
    /// Operations shared with systemd.
    #[command(flatten)]
    Service(ServiceCommand),
    /// Replace a stale ad-hoc identity so macOS asks for Local Network access again.
    #[command(after_help = crate::examples::REFRESH_LOCAL_NETWORK_PERMISSION)]
    RefreshLocalNetworkPermission,
}

/// Runs one systemd operation against the unit scope implied by current privileges.
pub(crate) async fn run_systemd(
    args: SystemdArgs,
    role: ServiceRole,
    paths: &AppPaths,
) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let backend = systemd::SystemdBackend::new(role, paths);
        run_command(&backend, role, paths, args.command).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (args, role, paths);
        bail!("systemd commands are only supported on Linux")
    }
}

/// Runs one macOS LaunchAgent operation for the invoking non-root user.
pub(crate) async fn run_launchd(
    args: LaunchdArgs,
    role: ServiceRole,
    paths: &AppPaths,
) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let backend = launchd::LaunchdBackend::new(role, paths, args.verbose)?;
        match args.command {
            LaunchdCommand::Service(command) => run_command(&backend, role, paths, command).await,
            LaunchdCommand::RefreshLocalNetworkPermission => {
                backend.refresh_local_network_permission().await
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (args, role, paths);
        bail!("launchd commands are only supported on macOS")
    }
}

/// One operating-system service manager controlling one Redoor role.
///
/// Implementations hold the resolved service name and scope, so the shared dispatcher
/// only expresses the lifecycle order (check, act, report).
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) trait ServiceBackend {
    /// CLI name of the manager (`systemd`, `launchd`), used in operator guidance.
    fn manager(&self) -> &'static str;
    /// Human name of the installed artifact, e.g. `Systemd unit 'redoor-agent.service'`.
    fn describe(&self) -> String;
    /// When an enabled service starts automatically: `boot` or `login`.
    fn autostart_trigger(&self) -> &'static str;
    /// Renders the service definition file for the given executable and config.
    fn render_definition(&self, binary: &Path, config_path: &Path) -> String;
    /// Writes the definition, enables it and optionally starts it; returns where it was written.
    async fn install(&self, config_path: &Path, start: bool) -> Result<PathBuf>;
    /// Stops, disables and removes the definition; returns whether anything was installed.
    async fn uninstall(&self) -> Result<bool>;
    /// Fails with install guidance unless the manager knows the service.
    async fn ensure_installed(&self) -> Result<()>;
    /// Starts the installed service.
    async fn start(&self) -> Result<()>;
    /// Stops the service while keeping it enabled.
    async fn stop(&self) -> Result<()>;
    /// Reloads the definition and restarts the service.
    async fn restart(&self) -> Result<()>;
    /// Enables automatic startup.
    async fn enable(&self) -> Result<()>;
    /// Disables automatic startup, optionally stopping it now.
    async fn disable(&self, now: bool) -> Result<()>;
    /// Reads installation, enablement and runtime state without failing on a stopped service.
    async fn status(&self) -> Result<ServiceState>;
}

/// Executes one shared service operation through `backend`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn run_command<B: ServiceBackend>(
    backend: &B,
    role: ServiceRole,
    paths: &AppPaths,
    command: ServiceCommand,
) -> Result<()> {
    match command {
        ServiceCommand::Install(install) => {
            let config_path = paths.default_config_path()?;
            let config_created = prepare_config(role, &config_path).await?;
            let definition = backend.install(&config_path, install.start).await?;
            print_install_help(
                backend,
                role,
                &InstallReport {
                    definition,
                    config_path,
                    config_created,
                    started: install.start,
                },
            );
        }
        ServiceCommand::Uninstall => {
            let description = backend.describe();
            if backend.uninstall().await? {
                println!("Uninstalled {description}; configuration was preserved.");
            } else {
                println!("{description} is already uninstalled; configuration was preserved.");
            }
        }
        ServiceCommand::Status => {
            println!("{}", backend.status().await?.format(&backend.describe()))
        }
        ServiceCommand::Start => {
            backend.ensure_installed().await?;
            backend.start().await?;
        }
        ServiceCommand::Stop => {
            backend.ensure_installed().await?;
            backend.stop().await?;
        }
        ServiceCommand::Restart => {
            backend.ensure_installed().await?;
            backend.restart().await?;
        }
        ServiceCommand::Enable => {
            backend.ensure_installed().await?;
            backend.enable().await?;
        }
        ServiceCommand::Disable(disable) => {
            backend.ensure_installed().await?;
            backend.disable(disable.now).await?;
        }
    }
    Ok(())
}

/// Builds the actionable error shared by every "not installed" check.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn not_installed(description: &str, role: ServiceRole, manager: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "{description} is not installed. Install it with: redoor {} {manager} install",
        role.cli_name()
    )
}

/// What a successful install produced, for the follow-up help text.
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct InstallReport {
    /// Path of the written unit or plist.
    definition: PathBuf,
    /// Config the service was pinned to.
    config_path: PathBuf,
    /// Whether the install created that config (agents import it from stdin).
    config_created: bool,
    /// Whether the service was started right away.
    started: bool,
}

/// Prints how to control the service after a successful installation.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn print_install_help<B: ServiceBackend>(backend: &B, role: ServiceRole, report: &InstallReport) {
    let trigger = backend.autostart_trigger();
    println!(
        "Installed {} at {} and enabled it {} {trigger} ({}).",
        backend.describe(),
        report.definition.display(),
        if trigger == "boot" { "on" } else { "at" },
        if report.started {
            "started"
        } else {
            "not started"
        }
    );
    if report.config_created {
        println!(
            "Created config at {}.
Edit it before starting the service.",
            report.config_path.display()
        );
        if role == ServiceRole::Agent {
            println!("The [agent] section probably needs to be changed (token, server).");
        }
    } else {
        println!("Config: {}", report.config_path.display());
    }
    let role = role.cli_name();
    let manager = backend.manager();
    println!(
        "
Manage the service:
  redoor {role} {manager} start
  redoor {role} {manager} stop       # remains enabled for future {trigger}
  redoor {role} {manager} restart
  redoor {role} {manager} status
  redoor {role} {manager} disable --now
  redoor {role} logs --follow"
    );
}

/// What the service manager reports about the running process.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ServiceRuntime {
    /// The manager has no loaded job for this service.
    Unloaded,
    /// A process is running, with its PID when the manager reports one.
    Running {
        /// Main process ID.
        pid: Option<String>,
    },
    /// The job is loaded but no process runs; `detail` is the manager's state words.
    Stopped {
        /// e.g. `inactive/dead` (systemd) or `waiting` (launchd).
        detail: Option<String>,
    },
}

/// Installation, enablement and runtime state of one service, formatted the same way
/// for every manager.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServiceState {
    /// Whether the definition file exists.
    pub(crate) installed: bool,
    /// Automatic startup; `None` when the manager cannot tell.
    pub(crate) enabled: Option<bool>,
    /// Process state reported by the manager.
    pub(crate) runtime: ServiceRuntime,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl ServiceState {
    /// One concise status line, e.g. `... installed, enabled, loaded, running, PID 42`.
    ///
    /// An absent definition omits meaningless enablement, but a job the manager still has
    /// loaded stays visible so orphans can be found.
    pub(crate) fn format(&self, description: &str) -> String {
        let runtime = match &self.runtime {
            ServiceRuntime::Running { pid: Some(pid) } => format!("loaded, running, PID {pid}"),
            ServiceRuntime::Running { pid: None } => "loaded, running".to_string(),
            ServiceRuntime::Stopped {
                detail: Some(detail),
            } => format!("loaded, stopped ({detail})"),
            ServiceRuntime::Stopped { detail: None } => "loaded, stopped".to_string(),
            ServiceRuntime::Unloaded => "unloaded, stopped".to_string(),
        };
        if !self.installed {
            return match self.runtime {
                ServiceRuntime::Unloaded => format!("{description}: not installed"),
                _ => format!("{description}: not installed, {runtime}"),
            };
        }
        let enabled = match self.enabled {
            Some(true) => "enabled",
            Some(false) => "disabled",
            None => "enablement unknown",
        };
        format!("{description}: installed, {enabled}, {runtime}")
    }
}

/// Ensures service installation has a complete config usable without CLI overrides.
#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn prepare_config(mode: ServiceRole, config_path: &Path) -> Result<bool> {
    if tokio::fs::try_exists(config_path)
        .await
        .with_context(|| format!("Failed to inspect config '{}'", config_path.display()))?
    {
        validate_existing_config(mode, config_path).await?;
        return Ok(false);
    }

    if mode == ServiceRole::Agent {
        let imported_path =
            redoor_core::config::import_agent_config_from_stdin(config_path).await?;
        validate_existing_config(mode, &imported_path).await?;
        return Ok(false);
    }

    bail!(
        "config '{}' is missing; run `redoor server` once to create a starter config, then re-run install",
        config_path.display()
    )
}

/// Rejects configs that cannot run the selected role without extra CLI flags.
#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn validate_existing_config(mode: ServiceRole, config_path: &Path) -> Result<()> {
    let config = redoor_core::config::parse_config_file(config_path).await?;
    match mode {
        ServiceRole::Agent => {
            if !redoor_core::config::standalone_agent_is_fully_configured(&config) {
                bail!(
                    "config '{}' is missing required standalone agent settings; set top-level agent_token plus [agent] server so the service can start without CLI flags",
                    config_path.display()
                );
            }
        }
        ServiceRole::Server => {
            redoor_core::config::require_server_section(&config)?;
        }
    }
    Ok(())
}

/// Replaces a service definition in one rename so managers never read a partial file.
#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn atomic_write(path: &Path, content: &[u8], artifact: &str) -> Result<()> {
    let file_name = path
        .file_name()
        .with_context(|| format!("{artifact} path has no file name"))?
        .to_string_lossy();
    let temporary_path = path.with_file_name(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        fastrand::u64(..)
    ));
    let write_result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to create temporary {artifact} '{}'",
                    temporary_path.display()
                )
            })?;
        file.write_all(content).await.with_context(|| {
            format!(
                "Failed to write temporary {artifact} '{}'",
                temporary_path.display()
            )
        })?;
        file.sync_all().await.with_context(|| {
            format!(
                "Failed to sync temporary {artifact} '{}'",
                temporary_path.display()
            )
        })?;
        drop(file);
        tokio::fs::rename(&temporary_path, path)
            .await
            .with_context(|| format!("Failed to install {artifact} '{}'", path.display()))
    }
    .await;
    if write_result.is_err() {
        let _ = tokio::fs::remove_file(&temporary_path).await;
    }
    write_result
}

/// Resolves the running executable so the service runs exactly this binary.
#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn current_binary() -> Result<PathBuf> {
    tokio::fs::canonicalize(std::env::current_exe()?)
        .await
        .context("Failed to resolve the current redoor executable")
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    /// Every manager shares one status vocabulary, including orphaned loaded jobs.
    #[test]
    fn formats_service_state() {
        let running = ServiceState {
            installed: true,
            enabled: Some(true),
            runtime: ServiceRuntime::Running {
                pid: Some("4321".into()),
            },
        };
        // An active service reports its main process ID.
        assert_eq!(
            running.format("Systemd unit 'redoor-server.service'"),
            "Systemd unit 'redoor-server.service': installed, enabled, loaded, running, PID 4321"
        );
        let waiting = ServiceState {
            installed: true,
            enabled: Some(false),
            runtime: ServiceRuntime::Stopped {
                detail: Some("waiting".into()),
            },
        };
        // A loaded job without a process must not be described as running.
        assert_eq!(
            waiting.format("LaunchAgent 'redoor-server'"),
            "LaunchAgent 'redoor-server': installed, disabled, loaded, stopped (waiting)"
        );
        let absent = ServiceState {
            installed: false,
            enabled: None,
            runtime: ServiceRuntime::Unloaded,
        };
        // An absent definition omits meaningless enablement and runtime state.
        assert_eq!(
            absent.format("LaunchAgent 'redoor-server'"),
            "LaunchAgent 'redoor-server': not installed"
        );
        let orphan = ServiceState {
            installed: false,
            ..running
        };
        // A manager-cached orphan stays visible after its definition disappears.
        assert_eq!(
            orphan.format("Systemd unit 'redoor-server.service'"),
            "Systemd unit 'redoor-server.service': not installed, loaded, running, PID 4321"
        );
    }
}
