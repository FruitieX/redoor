//! Redoor binary entry point: dispatches the server, agent, and remote CLI roles.

mod process_control;
mod process_logs;
mod service;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use process_control::LaunchParent;
use redoor_core::paths::AppPaths;
use redoor_core::runtime::RuntimeContext;
use redoor_core::websocket::WebSocketTimeouts;
use redoor_core::{app_name, config, process::ProcessSlot};
use service::ServiceRole;

#[derive(Parser)]
#[command(author, version, about)]
#[command(subcommand_required = true, arg_required_else_help = true)]
struct Cli {
    /// Namespace for config, data, logs, SSH caches, and generated service names.
    #[arg(
        long,
        env = app_name::APP_NAME_ENV,
        default_value = app_name::DEFAULT_APP_NAME,
        value_parser = app_name::parse_app_name,
        global = true
    )]
    app_name: String,
    /// How often idle sockets write a ping so proxies stay alive and half-open links fail fast.
    #[arg(
        long,
        env = "REDOOR_WEBSOCKET_KEEPALIVE",
        default_value = "10s",
        value_parser = redoor_core::websocket::parse_duration_millis,
        global = true
    )]
    websocket_keepalive: std::time::Duration,
    /// Silence before an agent WebSocket is treated as stale and closed.
    #[arg(
        long,
        env = "REDOOR_WEBSOCKET_STALE_TIMEOUT",
        default_value = "30s",
        value_parser = redoor_core::websocket::parse_duration_millis,
        global = true
    )]
    websocket_stale_timeout: std::time::Duration,
    /// How often to compare last inbound traffic against the stale timeout.
    #[arg(
        long,
        env = "REDOOR_WEBSOCKET_STALE_CHECK_INTERVAL",
        default_value = "5s",
        value_parser = redoor_core::websocket::parse_duration_millis,
        global = true
    )]
    websocket_stale_check_interval: std::time::Duration,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the server or use its role-specific utilities.
    Server(ServerArgs),
    /// Run the agent or use its role-specific utilities.
    Agent(Box<AgentCommandArgs>),
    /// Authenticate and operate on devices through a remote server.
    Remote(redoor_cli::RemoteArgs),
}

/// Preserves flat server startup flags while exposing `server logs`.
#[derive(Args)]
struct ServerArgs {
    /// Selects a utility action instead of starting the server.
    #[command(subcommand)]
    command: Option<ServerCommand>,
    /// Existing server startup settings remain accepted directly after `server`.
    #[command(flatten)]
    run: redoor_server::http::state::CoordinatorArgs,
}

/// Server-specific utility commands.
#[derive(Subcommand)]
enum ServerCommand {
    /// Stop the server recorded in the application PID file.
    Stop,
    /// Report whether the server PID file lock is held.
    Status,
    /// Print the configured server file log.
    Logs(ServerLogsArgs),
    /// Install or manage the server as a systemd service.
    Systemd(service::SystemdArgs),
    /// Install or manage the server as a macOS LaunchAgent.
    Launchd(service::LaunchdArgs),
    /// Create, inspect, or revoke durable MCP machine credentials.
    McpToken(redoor_server::http::machine_tokens::MachineTokenArgs),
}

/// Options used to locate and limit server file logs.
#[derive(Args)]
struct ServerLogsArgs {
    /// Number of trailing lines to print.
    #[arg(short = 'n', default_value_t = 500)]
    lines: usize,
    /// Continue printing new log entries until interrupted.
    #[arg(short = 'f', long)]
    follow: bool,
    /// Override the shared TOML config path.
    #[arg(long)]
    config: Option<String>,
    /// Override `REDOOR_SERVER_LOG` and `[server].log`.
    #[arg(long, env = "REDOOR_SERVER_LOG")]
    log: Option<String>,
}

/// Preserves flat agent startup flags while exposing `agent logs`.
#[derive(Args)]
struct AgentCommandArgs {
    /// Selects a utility action instead of starting the agent.
    #[command(subcommand)]
    command: Option<AgentCommand>,
    /// Existing agent startup settings remain accepted directly after `agent`.
    #[command(flatten)]
    run: redoor_agent::args::AgentArgs,
}

/// Agent-specific utility commands.
#[derive(Subcommand)]
enum AgentCommand {
    /// Stop the standalone agent recorded in the application PID file.
    Stop,
    /// Report whether the standalone agent PID file lock is held.
    Status,
    /// Print the configured standalone-agent file log.
    Logs(AgentLogsArgs),
    /// Install or manage the agent as a systemd service.
    Systemd(service::SystemdArgs),
    /// Install or manage the agent as a macOS LaunchAgent.
    Launchd(service::LaunchdArgs),
    /// Start an agent on an SSH host and relay it through this machine to a redoor server.
    Relay(Box<RelayCommandArgs>),
}

/// Requires an explicit lifecycle action for one configured relay.
#[derive(Args)]
struct RelayCommandArgs {
    #[command(subcommand)]
    command: RelayCommand,
}

/// Relay-specific lifecycle commands keyed by configured relay ID.
#[derive(Subcommand)]
enum RelayCommand {
    /// List the relay IDs available in configuration.
    Ls(RelayListArgs),
    /// Start one configured relay in the foreground or background.
    Start(RelayStartArgs),
    /// Stop one named relay using its locked runtime file.
    Stop(RelayIdArgs),
    /// Report whether one named relay runtime-file lock is held.
    Status(RelayIdArgs),
    /// Print one named relay's file log.
    Logs(RelayLogsArgs),
}

/// Options used to find the configured relay IDs.
#[derive(Args)]
struct RelayListArgs {
    /// Override the shared TOML config path.
    #[arg(long)]
    config: Option<String>,
}

/// Options required to start one configured relay.
#[derive(Args)]
struct RelayStartArgs {
    /// Stable relay ID from a `[[relays]]` entry.
    #[arg(value_parser = config::parse_relay_id)]
    id: String,
    /// Override the shared TOML config path.
    #[arg(long)]
    config: Option<String>,
    /// Detach the relay from the terminal and continue in the background.
    #[arg(long)]
    daemon: bool,
}

/// Selects a named relay for a lifecycle operation that does not need TOML.
#[derive(Args)]
struct RelayIdArgs {
    /// Stable relay ID used by its runtime file.
    #[arg(value_parser = config::parse_relay_id)]
    id: String,
}

/// Options used to locate and limit relay file logs.
#[derive(Args)]
struct RelayLogsArgs {
    /// Stable relay ID from configuration or an existing runtime file.
    #[arg(value_parser = config::parse_relay_id)]
    id: String,
    /// Number of trailing lines to print.
    #[arg(short = 'n', default_value_t = 500)]
    lines: usize,
    /// Override the shared TOML config path when the relay is stopped.
    #[arg(long)]
    config: Option<String>,
}

/// Options used to locate and limit standalone-agent file logs.
#[derive(Args)]
struct AgentLogsArgs {
    /// Number of trailing lines to print.
    #[arg(short = 'n', default_value_t = 500)]
    lines: usize,
    /// Continue printing new log entries until interrupted.
    #[arg(short = 'f', long)]
    follow: bool,
    /// Override the shared TOML config path.
    #[arg(long)]
    config: Option<String>,
    /// Override `REDOOR_AGENT_LOG` and `[agent].log`.
    #[arg(long, env = "REDOOR_AGENT_LOG")]
    log: Option<String>,
}

/// Parses the CLI, runs the selected role on a Tokio runtime and maps its outcome to an exit code.
///
/// Every role returns `anyhow::Result<ExitCode>`; this is the single place that prints a
/// failure (`{:#}` shows the whole context chain) so no role needs `process::exit`.
fn main() -> ExitCode {
    // OpenSSH execs SSH_ASKPASS as this binary with the prompt as argv[1].
    // Detect that role before clap treats the prompt as a subcommand.
    if std::env::var_os(redoor_server::ssh::askpass::ENV).is_some() {
        return redoor_server::ssh::askpass::run();
    }
    // Snapshot before anything can await: the kernel will not replay a parent death later.
    let launch_parent = LaunchParent::capture();
    let cli = Cli::parse();
    let context = RuntimeContext {
        paths: AppPaths::from_env(cli.app_name),
        // Applied before any listener starts so tests can shrink idle waits via env.
        ws_timeouts: WebSocketTimeouts {
            keepalive: cli.websocket_keepalive,
            stale_timeout: cli.websocket_stale_timeout,
            stale_check_interval: cli.websocket_stale_check_interval,
        },
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Failed to start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run(cli.command, context, launch_parent));
    // Blocking stdin readers (agents, remote exec) never finish on their own; waiting for
    // them would keep a finished command alive.
    runtime.shutdown_background();
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Dispatches the parsed top-level command to its role.
async fn run(command: Commands, context: RuntimeContext, parent: LaunchParent) -> Result<ExitCode> {
    match command {
        Commands::Remote(args) => {
            let store = redoor_cli::session::SessionStore::new(context.paths.data_dir()?);
            let code = redoor_cli::run(args, store).await?;
            // Wrapping matches what the OS does with an out-of-range process status.
            Ok(ExitCode::from(code as u8))
        }
        Commands::Server(args) => run_server_command(args, context, parent).await,
        Commands::Agent(args) => run_agent_command(*args, context, parent).await,
    }
}

/// Runs `redoor server` or one of its utilities.
async fn run_server_command(
    args: ServerArgs,
    context: RuntimeContext,
    parent: LaunchParent,
) -> Result<ExitCode> {
    let paths = &context.paths;
    match args.command {
        Some(ServerCommand::Logs(logs)) => {
            let log_args = process_logs::LogArgs {
                config: logs.config,
                log: logs.log,
                lines: logs.lines,
                follow: logs.follow,
            };
            process_logs::run(process_logs::LogRole::Server, log_args, paths).await?;
        }
        Some(ServerCommand::Stop) => process_control::stop(ProcessSlot::Server, paths).await?,
        Some(ServerCommand::Status) => process_control::status(ProcessSlot::Server, paths).await?,
        Some(ServerCommand::Systemd(systemd)) => {
            service::run_systemd(systemd, ServiceRole::Server, paths).await?
        }
        Some(ServerCommand::Launchd(launchd)) => {
            service::run_launchd(launchd, ServiceRole::Server, paths).await?
        }
        Some(ServerCommand::McpToken(tokens)) => {
            redoor_server::http::machine_tokens::run_cli(tokens, paths).await?
        }
        None => {
            let daemon = args.run.daemon;
            return run_role(ProcessSlot::Server, daemon, &context, parent, || {
                redoor_server::startup::run_server(args.run, context.clone())
            })
            .await;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Runs `redoor agent`, one of its utilities, or a named relay.
async fn run_agent_command(
    args: AgentCommandArgs,
    context: RuntimeContext,
    parent: LaunchParent,
) -> Result<ExitCode> {
    let paths = &context.paths;
    match args.command {
        Some(AgentCommand::Logs(logs)) => {
            let log_args = process_logs::LogArgs {
                config: logs.config,
                log: logs.log,
                lines: logs.lines,
                follow: logs.follow,
            };
            process_logs::run(process_logs::LogRole::Agent, log_args, paths).await?;
        }
        Some(AgentCommand::Stop) => process_control::stop(ProcessSlot::Agent, paths).await?,
        Some(AgentCommand::Status) => process_control::status(ProcessSlot::Agent, paths).await?,
        Some(AgentCommand::Systemd(systemd)) => {
            service::run_systemd(systemd, ServiceRole::Agent, paths).await?
        }
        Some(AgentCommand::Launchd(launchd)) => {
            service::run_launchd(launchd, ServiceRole::Agent, paths).await?
        }
        Some(AgentCommand::Relay(relay)) => match relay.command {
            RelayCommand::Ls(list) => list_relays(list, paths).await?,
            RelayCommand::Start(start) => return run_named_relay(start, &context, parent).await,
            RelayCommand::Stop(relay) => process_control::stop_relay(&relay.id, paths).await?,
            RelayCommand::Status(relay) => process_control::status_relay(&relay.id, paths).await?,
            RelayCommand::Logs(logs) => {
                process_logs::run_relay(&logs.id, logs.config, logs.lines, paths).await?
            }
        },
        None => {
            let daemon = args.run.daemon;
            if daemon {
                redoor_agent::prepare_daemon_config(&args.run, paths).await?;
            }
            return run_role(ProcessSlot::Agent, daemon, &context, parent, || {
                redoor_agent::run(args.run, context.clone())
            })
            .await;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Resolves an explicit `--config` or the namespace's conventional config path.
fn config_path(explicit: Option<String>, paths: &AppPaths) -> Result<PathBuf> {
    match explicit {
        Some(path) => Ok(PathBuf::from(path)),
        None => paths.default_config_path(),
    }
}

/// Prints configured relay IDs in declaration order for interactive and scripted selection.
async fn list_relays(args: RelayListArgs, paths: &AppPaths) -> Result<()> {
    let parsed = config::parse_config_file(&config_path(args.config, paths)?).await?;
    for relay in parsed.relays {
        println!("{}", relay.id);
    }
    Ok(())
}

/// Loads one named relay, establishes its isolated runtime identity, and starts it.
async fn run_named_relay(
    args: RelayStartArgs,
    context: &RuntimeContext,
    parent: LaunchParent,
) -> Result<ExitCode> {
    let paths = &context.paths;
    let parsed = config::parse_config_file(&config_path(args.config, paths)?).await?;
    let mut relay = config::require_relay(&parsed, &args.id)?.clone();
    let log = match relay.agent.log.clone() {
        Some(log) => log,
        None => paths.relay_log_path(&relay.id)?,
    };
    relay.agent.log = Some(log.clone());
    if args.daemon {
        process_control::spawn_relay_daemon(&relay.id, paths).await?;
        return Ok(ExitCode::SUCCESS);
    }
    parent.bind_lifetime();
    let agent_name = relay
        .agent
        .name
        .clone()
        .unwrap_or_else(|| config::default_agent_name(&relay.agent.target));
    let agent_app_name = relay
        .agent_app_name
        .clone()
        .unwrap_or_else(|| format!("{}-relay-{}", paths.app_name(), relay.id));
    let metadata = process_control::RelayPidMetadata {
        pid: 0,
        id: relay.id.clone(),
        started_at: jiff::Timestamp::now().to_string(),
        target: relay.agent.target.clone(),
        server: relay.server.clone(),
        agent_name,
        agent_app_name: agent_app_name.clone(),
        log,
    };
    // Held until return; dropping it removes this process's runtime file on every path.
    let _pid_file = process_control::acquire_relay(metadata, paths).await?;
    let token = std::env::var("REDOOR_AGENT_TOKEN")
        .ok()
        .filter(|token| !token.is_empty())
        .unwrap_or(parsed.agent_token);
    redoor_server::ssh::relay::run_relay(relay, token, agent_app_name, paths.clone()).await?;
    Ok(ExitCode::SUCCESS)
}

/// Applies daemon and PID-file behavior consistently around a long-lived role.
async fn run_role<F, Fut>(
    slot: ProcessSlot,
    daemon: bool,
    context: &RuntimeContext,
    parent: LaunchParent,
    run: F,
) -> Result<ExitCode>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<ExitCode>>,
{
    if daemon {
        process_control::spawn_daemon(slot, &context.paths).await?;
        return Ok(ExitCode::SUCCESS);
    }
    parent.bind_lifetime();
    // Held for the role's lifetime; its Drop removes the claim on success and failure alike.
    let _pid_file = process_control::acquire(slot, &context.paths).await?;
    run().await
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{AgentCommand, Cli, Commands, RelayCommand};

    /// Keeps service-manager commands nested under their selected process role.
    #[test]
    fn parses_role_scoped_service_commands() {
        let timeouts = Cli::try_parse_from([
            "redoor",
            "--websocket-keepalive",
            "200ms",
            "--websocket-stale-timeout",
            "1s",
            "--websocket-stale-check-interval",
            "100ms",
            "server",
            "status",
        ])
        .expect("global websocket timeout flags should parse on any subcommand");
        // Flags exist so integration tests can shrink idle waits without rebuilding.
        assert_eq!(
            timeouts.websocket_keepalive,
            std::time::Duration::from_millis(200)
        );
        assert_eq!(
            timeouts.websocket_stale_timeout,
            std::time::Duration::from_secs(1)
        );
        assert_eq!(
            timeouts.websocket_stale_check_interval,
            std::time::Duration::from_millis(100)
        );
        assert!(
            Cli::try_parse_from(["redoor", "agent", "systemd", "status"]).is_ok(),
            "agent systemd status should parse without a redundant mode flag"
        );
        assert!(
            Cli::try_parse_from(["redoor", "server", "launchd", "install", "--start"]).is_ok(),
            "server launchd install should accept explicit startup"
        );
        assert!(
            Cli::try_parse_from([
                "redoor",
                "agent",
                "launchd",
                "refresh-local-network-permission"
            ])
            .is_ok(),
            "launchd should expose the macOS Local Network permission repair"
        );
        assert!(
            Cli::try_parse_from([
                "redoor",
                "agent",
                "systemd",
                "refresh-local-network-permission"
            ])
            .is_err(),
            "the macOS-specific repair must not appear under systemd"
        );
        assert!(
            Cli::try_parse_from(["redoor", "server", "launchd", "--verbose", "start"]).is_ok()
                && Cli::try_parse_from(["redoor", "server", "launchd", "start", "--verbose",])
                    .is_ok(),
            "launchd verbosity should parse on either side of its selected action"
        );
        assert!(
            Cli::try_parse_from(["redoor", "systemd", "status", "--mode", "agent"]).is_err(),
            "the former top-level systemd command must no longer be accepted"
        );
        assert!(
            Cli::try_parse_from(["redoor", "agent", "launchd", "status", "--mode", "agent",])
                .is_err(),
            "role-scoped service commands must reject the removed mode flag"
        );
        for manager in ["systemd", "launchd"] {
            assert!(
                Cli::try_parse_from(["redoor", "agent", manager, "disable", "--now"]).is_ok(),
                "disable --now should parse for {manager}"
            );
            assert!(
                Cli::try_parse_from(["redoor", "agent", manager, "uninstall"]).is_ok(),
                "uninstall should parse for {manager}"
            );
            assert!(
                Cli::try_parse_from(["redoor", "agent", manager, "setup"]).is_err(),
                "the removed setup command should not parse for {manager}"
            );
            assert!(
                Cli::try_parse_from(["redoor", "agent", manager, "logs"]).is_err(),
                "service-manager logs should not parse for {manager}"
            );
        }
        assert!(
            Cli::try_parse_from([
                "redoor",
                "--app-name",
                "preview",
                "agent",
                "systemd",
                "status",
            ])
            .is_ok(),
            "the global app name should be the service installation identity"
        );
        assert!(
            Cli::try_parse_from([
                "redoor",
                "agent",
                "systemd",
                "status",
                "--unit-name",
                "custom",
            ])
            .is_err()
                && Cli::try_parse_from([
                    "redoor",
                    "agent",
                    "launchd",
                    "status",
                    "--service-name",
                    "custom",
                ])
                .is_err(),
            "manager-specific identity flags should be rejected"
        );
    }

    /// Keeps following on the role log commands rather than service-manager commands.
    #[test]
    fn parses_role_log_follow_options() {
        assert!(
            Cli::try_parse_from(["redoor", "server", "logs", "-f", "-n", "25"]).is_ok(),
            "server logs should support short follow and line-count options"
        );
        assert!(
            Cli::try_parse_from(["redoor", "agent", "logs", "--follow", "--log", "agent.log"])
                .is_ok(),
            "agent logs should preserve explicit log overrides while following"
        );
    }

    /// Keeps process status nested under each long-lived role, including named relays.
    #[test]
    fn parses_process_status_commands() {
        assert!(
            Cli::try_parse_from(["redoor", "agent", "status"]).is_ok(),
            "standalone agent status should parse without startup flags"
        );
        assert!(
            Cli::try_parse_from(["redoor", "server", "status"]).is_ok(),
            "server status should parse without startup flags"
        );
        assert!(
            Cli::try_parse_from(["redoor", "agent", "relay", "status", "production"]).is_ok(),
            "relay status should require only its stable ID"
        );
        assert!(
            Cli::try_parse_from(["redoor", "agent", "relay", "stop", "production"]).is_ok(),
            "relay stop should require only its stable ID"
        );
        assert!(
            Cli::try_parse_from(["redoor", "agent", "relay", "logs", "production"]).is_ok(),
            "relay logs should require only its stable ID"
        );
        assert!(
            Cli::try_parse_from(["redoor", "agent", "relay", "ls", "--config", "relay.toml"])
                .is_ok(),
            "relay ls should accept an explicit shared config path"
        );
    }

    /// Keeps relay startup aligned with other daemon-capable process commands.
    #[test]
    fn agent_relay_start_accepts_id_config_and_daemon_only() {
        let start_cli = Cli::try_parse_from([
            "redoor",
            "agent",
            "relay",
            "start",
            "production",
            "--config",
            "relay.toml",
            "--daemon",
        ])
        .unwrap();
        let Commands::Agent(start_agent) = start_cli.command else {
            panic!("agent relay start should parse into the agent command");
        };
        let Some(AgentCommand::Relay(start_relay)) = start_agent.command else {
            panic!("agent relay start should preserve its relay command wrapper");
        };
        let RelayCommand::Start(start) = start_relay.command else {
            panic!("relay start should select the configured relay");
        };
        // Daemon mode remains a start option rather than a separate lifecycle verb.
        assert!(start.daemon);
        assert_eq!(start.id, "production");
        assert!(
            Cli::try_parse_from([
                "redoor",
                "agent",
                "relay",
                "start",
                "production",
                "--server",
                "http://redoor.example:3000",
            ])
            .is_err(),
            "removed ad hoc relay flags must be rejected"
        );
        let stop_cli =
            Cli::try_parse_from(["redoor", "agent", "relay", "stop", "production"]).unwrap();
        let Commands::Agent(stop_agent) = stop_cli.command else {
            panic!("agent relay stop should parse into the agent command");
        };
        let Some(AgentCommand::Relay(stop_relay)) = stop_agent.command else {
            panic!("agent relay stop should preserve its relay command wrapper");
        };
        assert!(matches!(stop_relay.command, RelayCommand::Stop(_)));
    }
}
