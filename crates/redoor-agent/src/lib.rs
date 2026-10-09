//! The Redoor agent: connects to a server over websockets and executes its commands.

pub mod args;
mod commands;
mod connection;
mod directory_measurement;
mod errors;
mod exec;
mod logs;
mod notification;
mod ownership;
mod platform;
mod runtime;
mod terminal;
mod transfer;
mod transfers;
mod trash;
mod ws;

use std::path::PathBuf;

use args::{AgentArgs, NotificationDelay};
use commands::ctx::AgentServices;
use redoor_core::log;
use redoor_protocol::{ids::AgentId, wire::log::Level};
use runtime::{AgentHandle, AgentProfile, AgentRuntime};
use sysinfo::System;

/// Bounded runtime mailbox; awaited sends backpressure websocket ingress instead of dropping.
const AGENT_MAILBOX_CAPACITY: usize = 256;

/// Runs an agent after resolving settings with CLI > env > config file > default.
pub async fn run(args: AgentArgs) -> Result<(), Box<dyn std::error::Error>> {
    let exit_on_stdin_eof = args.exit_on_stdin_eof;
    let resolved = resolve_agent_settings(args).await?;
    let configured_directory = resolved.home.map(std::path::PathBuf::from).unwrap_or(
        std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .ok_or("HOME is not set; pass --home with the agent home directory")?,
    );
    let default_directory = tokio::fs::canonicalize(&configured_directory)
        .await
        .map_err(|error| {
            format!(
                "Failed to resolve agent default directory '{}': {error}",
                configured_directory.display()
            )
        })?;
    if !tokio::fs::metadata(&default_directory).await?.is_dir() {
        return Err(format!(
            "Agent default directory is not a directory: {}",
            configured_directory.display()
        )
        .into());
    }
    let default_directory = default_directory
        .into_os_string()
        .into_string()
        .map_err(|_| "Agent default directory is not valid UTF-8")?;

    let connection = connection::AgentConnection::new(
        resolved.ws_address,
        resolved.connect_address,
        resolved.insecure_tls,
    )?;
    let agent_name = resolved.name;
    let log_file = resolved.log;
    let log_level = resolved.log_level;
    let log_format = resolved.log_format;
    let token = resolved.token;
    let notification_delay = resolved
        .notification_delay_seconds
        .map(tokio::time::Duration::from_secs);
    let trash = trash::Trash::select(resolved.trash_directory.map(PathBuf::from))
        .await
        .map_err(|error| format!("Failed to initialize trash: {error}"))?;
    let loaded_config_path = match resolved.loaded_config_path {
        Some(path) => match tokio::fs::canonicalize(&path).await {
            Ok(canonical) => Some(canonical),
            Err(_) => Some(path),
        },
        None => None,
    };
    // Details API reads this once-per-process value; set before any commands run.
    crate::commands::identity::set_agent_loaded_config_path(loaded_config_path.clone());

    let agent_id = AgentId::from(agent_name.clone());

    redoor_core::logging::init_with_options(log_file, log_level, log_format)
        .await
        .map_err(|error| format!("{error:#}"))?;
    match &loaded_config_path {
        Some(path) => {
            log!(Level::Info, "Loaded agent config: path={}", path.display());
        }
        None => {
            log!(
                Level::Info,
                "No agent config file loaded; using CLI/env settings"
            );
        }
    }
    log!(
        Level::Info,
        "Starting agent '{}': ws={}, home={}",
        agent_name,
        connection.server_url(),
        default_directory
    );

    let (handle, receiver, lifecycle_receiver) = AgentHandle::channel(AGENT_MAILBOX_CAPACITY);
    let runtime = AgentRuntime::new(
        AgentProfile {
            agent_id,
            agent_name,
            default_directory,
            token,
        },
        connection,
        AgentServices::new(trash),
        notification_delay,
    );

    runtime
        .run(receiver, lifecycle_receiver, handle, exit_on_stdin_eof)
        .await;

    Ok(())
}

/// Imports a missing daemon config before detaching closes the invoking terminal's stdin.
pub async fn prepare_daemon_config(args: &AgentArgs) -> anyhow::Result<()> {
    let required_settings_provided = args
        .server
        .as_deref()
        .is_some_and(|value| !value.is_empty())
        && args.token.as_deref().is_some_and(|value| !value.is_empty());
    if required_settings_provided {
        return Ok(());
    }

    let config_path = match args.config.as_ref() {
        Some(path) => PathBuf::from(path),
        None => match redoor_core::config::default_config_path() {
            Ok(path) => path,
            Err(_) => return Ok(()),
        },
    };
    let exists = tokio::fs::try_exists(&config_path).await.map_err(|error| {
        anyhow::anyhow!(
            "Failed to inspect config file '{}': {error}",
            config_path.display()
        )
    })?;
    if !exists {
        redoor_core::config::import_agent_config_from_stdin(&config_path).await?;
    }
    Ok(())
}

/// Fully resolved agent launch settings after applying source precedence.
struct ResolvedAgentSettings {
    /// Canonical `ws(s)://…/ws` URL after normalizing the operator-supplied server address.
    ws_address: String,
    /// Optional physical TCP endpoint used when a tunnel differs from the logical server URL.
    connect_address: Option<String>,
    /// Disables certificate verification only for explicitly tunneled WSS connections.
    insecure_tls: bool,
    name: String,
    token: String,
    home: Option<String>,
    /// Optional exact root used instead of platform trash-location discovery.
    trash_directory: Option<String>,
    log: Option<String>,
    /// Effective startup threshold resolved before logger initialization.
    log_level: Level,
    /// Effective process output format resolved before logger initialization.
    log_format: redoor_core::logging::LogFormat,
    /// Non-negative delay selected on the command line, or `None` when explicitly disabled.
    notification_delay_seconds: Option<u64>,
    /// Path of the TOML file that contributed settings, when one was loaded.
    loaded_config_path: Option<PathBuf>,
}

/// Applies CLI > env > config file > default for every agent setting.
///
/// Clap already merged CLI and env into `args`. The agent name defaults to the
/// machine hostname while `server` and `token` remain required.
async fn resolve_agent_settings(
    args: AgentArgs,
) -> Result<ResolvedAgentSettings, Box<dyn std::error::Error>> {
    let explicit_config = args.config.is_some();
    let required_settings_provided = args
        .server
        .as_deref()
        .is_some_and(|value| !value.is_empty())
        && args.token.as_deref().is_some_and(|value| !value.is_empty());
    let config_path = match args.config.clone() {
        Some(path) => Some(PathBuf::from(path)),
        // Conventional path is optional for agents so fully CLI/env-configured
        // runs still work when HOME is unset or the file is missing.
        None => redoor_core::config::default_config_path().ok(),
    };

    let (file_config, loaded_config_path) = match config_path {
        Some(mut path) => {
            // Explicit --config must exist; the conventional path is optional so
            // fully CLI/env-configured agents do not require a file.
            let exists = tokio::fs::try_exists(&path).await.map_err(|error| {
                format!(
                    "Failed to inspect config file '{}': {error}",
                    path.display()
                )
            })?;
            if !exists {
                if !required_settings_provided {
                    path = redoor_core::config::import_agent_config_from_stdin(&path).await?;
                } else if explicit_config {
                    return Err(format!("Failed to read config file '{}'", path.display()).into());
                } else {
                    return resolve_agent_settings_from_sources(args, None, None);
                }
            }
            let parsed = redoor_core::config::parse_config_file(&path.to_string_lossy())
                .await
                .map_err(|error| {
                    format!("Failed to parse config file '{}': {error}", path.display())
                })?;
            (Some(parsed), Some(path))
        }
        None => return resolve_agent_settings_from_sources(args, None, None),
    };

    resolve_agent_settings_from_sources(args, file_config, loaded_config_path)
}

/// Applies source precedence after optional config discovery or import has completed.
fn resolve_agent_settings_from_sources(
    args: AgentArgs,
    file_config: Option<redoor_core::config::RedoorConfig>,
    loaded_config_path: Option<PathBuf>,
) -> Result<ResolvedAgentSettings, Box<dyn std::error::Error>> {
    let agent_section = file_config
        .as_ref()
        .and_then(|config| config.agent.clone())
        .unwrap_or_default();

    // args already holds CLI or env; config is the next tier.
    let server = first_non_empty([args.server, agent_section.server])
        .ok_or("agent server is required; set it via CLI, REDOOR_AGENT_WS, or [agent].server")?;
    let ws_address = redoor_core::server_address::ServerAddress::parse_with_warning(&server)
        .map_err(|error| format!("invalid agent server address: {error}"))?
        .websocket_url();

    let name = first_non_empty([args.name, agent_section.name])
        .or_else(System::host_name)
        .ok_or("agent name is not configured and the computer hostname is unavailable")?;

    let token = first_non_empty([
        args.token,
        file_config
            .as_ref()
            .map(|config| config.agent_token.clone()),
    ])
    .ok_or(
        "agent token is required; set it via --token, REDOOR_AGENT_TOKEN, or top-level agent_token",
    )?;

    let home = first_non_empty([
        args.home,
        std::env::var("REDOOR_AGENT_DIR").ok(),
        agent_section.home,
    ]);
    let trash_directory = first_non_empty([args.trash_directory]);
    let notification_delay_seconds =
        match args.notification.unwrap_or(NotificationDelay::Seconds(5)) {
            NotificationDelay::Off => None,
            NotificationDelay::Seconds(seconds) => Some(seconds),
        };
    let log = Some(match first_non_empty([args.log, agent_section.log]) {
        Some(path) => path,
        None => redoor_core::config::default_agent_log_path()?,
    });
    let log_level = redoor_core::logging::resolve_initial_level(
        args.log_level,
        "REDOOR_AGENT_LOG_LEVEL",
        agent_section.log_level,
    )?;
    let log_format = redoor_core::logging::resolve_initial_format(
        args.log_format,
        "REDOOR_AGENT_LOG_FORMAT",
        agent_section.log_format,
    )?;

    Ok(ResolvedAgentSettings {
        ws_address,
        connect_address: args.connect_address,
        insecure_tls: args.insecure_tls,
        name,
        token,
        home,
        trash_directory,
        log,
        log_level,
        log_format,
        notification_delay_seconds,
        loaded_config_path,
    })
}

/// Returns the first non-empty string across precedence tiers.
fn first_non_empty<I>(values: I) -> Option<String>
where
    I: IntoIterator<Item = Option<String>>,
{
    values.into_iter().flatten().find(|value| !value.is_empty())
}
