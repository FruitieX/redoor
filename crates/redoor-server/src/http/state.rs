//! Shared handler state and server CLI arguments.

use std::path::PathBuf;
use std::sync::Arc;

use crate::actors;
use crate::log_registry::LogRegistry;
use crate::one_time_token_registry::OneTimeTokenRegistry;
use crate::terminal_registry::TerminalRegistry;
use crate::watchdog::WatchdogRegistry;
use clap::Args;
use redoor_core::logging::LogFormat;
use redoor_protocol::api::server::ServerAuthMode;
use redoor_protocol::wire::log::Level;

use super::auth::AuthState;
use super::machine_tokens::MachineTokenService;
use super::mcp::McpState;

/// Everything handlers need, cloned cheaply per request because every member is a handle.
#[derive(Clone)]
pub(crate) struct ServerState {
    /// Namespace, data directories and websocket timings chosen at process start.
    pub(crate) runtime: redoor_core::runtime::RuntimeContext,
    /// Release cache shared by agent upgrades and SSH provisioning.
    pub(crate) binaries: crate::binaries::BinaryCache,
    /// Registers managed supervisors for startup inventory and configuration routes alike.
    pub(crate) launcher: super::watchdog::ManagedLauncher,
    pub(crate) router_ref: actors::router::RouterHandle,
    /// Typed one-shot agent commands with their deadlines and cancel-on-abandon behavior.
    pub(crate) agents: super::agent_client::AgentClient,
    /// Starts copies and moves for REST and MCP alike.
    pub(crate) transfers: super::services::transfer::TransferService,
    /// Shared registry of agent supervisors. The WebSocket session uses
    /// this to look up the supervisor for the agent it just registered
    /// and signal it when the connection goes stale.
    pub(crate) watchdog_registry: WatchdogRegistry,
    /// Pairs short-lived browser and dedicated agent terminal connections.
    pub(crate) terminal_registry: TerminalRegistry,
    /// Pairs short-lived browser and dedicated agent log connections.
    pub(crate) log_registry: LogRegistry,
    /// Keeps download credentials process-local and atomically single-use.
    pub(crate) one_time_token_registry: OneTimeTokenRegistry,
    /// Correlates client-owned upload requests while their destination workers are still active.
    pub(crate) upload_requests: super::upload_requests::UploadRequests,
    /// Validates opaque cookies against durable, server-side session files.
    pub(crate) auth: AuthState,
    /// Reads durable MCP grants on every request so CLI revocations take effect immediately.
    #[allow(dead_code)]
    pub(crate) machine_tokens: MachineTokenService,
    /// Enforces MCP-specific origin, rate, and concurrency policy outside browser auth.
    pub(crate) mcp: McpState,
    /// Absolute path of the TOML config loaded at process start (for the server
    /// home UI and so restart can re-validate the same file).
    pub(crate) config_path: PathBuf,
    /// Serializes read-modify-write config edits so concurrent submissions cannot lose entries.
    pub(crate) config_edit_lock: Arc<tokio::sync::Mutex<()>>,
    /// Login backend resolved from the TOML credentials (or their absence).
    pub(crate) auth_mode: ServerAuthMode,
    /// Signals axum graceful shutdown; restart fires this after a successful
    /// pre-validation so the listener is dropped before exec.
    pub(crate) shutdown_tx: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

/// Arguments for `redoor server`.
///
/// All server-level fields are `Option` so `run_server` can apply
/// CLI > env > config file > default. Clap `env` fills each field when the
/// flag is omitted, so resolution is simply `args.or(config).or(default)`.
#[derive(Args)]
#[command(author, version, about)]
pub struct CoordinatorArgs {
    /// Detach the server from the terminal and continue running in the background.
    #[arg(long)]
    pub daemon: bool,
    /// Port to listen on. Overrides `REDOOR_PORT` and `[server].port`.
    /// Defaults to 7666 when not set anywhere.
    #[arg(long, env = "REDOOR_PORT")]
    pub(crate) port: Option<u16>,
    /// Address to bind the HTTP listener on (e.g. "0.0.0.0" to expose
    /// beyond localhost). Overrides `[server].bind`. Defaults to 127.0.0.1
    /// when not set anywhere.
    #[arg(long)]
    pub(crate) bind: Option<String>,
    /// Server log file path. Overrides `[server].log`. Defaults to
    /// `~/.local/share/<app-name>/server.log` for non-root users.
    #[arg(long, env = "REDOOR_SERVER_LOG")]
    pub(crate) log: Option<String>,
    /// Initial threshold. CLI overrides role environment, TOML, and the info default.
    #[arg(long = "log-level")]
    pub(crate) log_level: Option<Level>,
    /// Process output format. CLI overrides role environment, TOML, and line.
    #[arg(long = "log-format")]
    pub(crate) log_format: Option<LogFormat>,
    /// Path to the TOML config file. When omitted, Redoor loads or creates
    /// `/etc/<app-name>/config.toml` as root, otherwise `~/.config/<app-name>/config.toml`.
    /// Top-level `agent_token` is required; `[server]` holds listener/auth
    /// settings (browser `username`/`password` may be omitted together on Linux
    /// for PAM). Optional `[[agents]]` entries are either SSH-backed (`target`)
    /// or local (`local = true`) and start lazily from the UI or management API.
    #[arg(long)]
    pub(crate) config: Option<String>,
}
