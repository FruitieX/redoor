//! Server process startup: resolves configuration, builds shared state, and serves HTTP.

use std::{path::PathBuf, sync::Arc};

use crate::actors::router::spawn_router;
use crate::http::auth::{AuthState, LoginCredentials};
use crate::http::routes::build_app;
use crate::http::state::{CoordinatorArgs, ServerState};
use crate::http::watchdog::register_agents;
use crate::log_registry::LogRegistry;
use crate::one_time_token_registry::OneTimeTokenRegistry;
use crate::terminal_registry::TerminalRegistry;
use crate::watchdog::WatchdogRegistry;
use redoor_core::log;
use redoor_protocol::{api::server::ServerAuthMode, wire::log::Level};

/// Starts the long-lived server using the established flat startup arguments.
pub async fn run_server(args: CoordinatorArgs) -> anyhow::Result<()> {
    let app_name = redoor_core::app_name::app_name().expect("Clap validated the application name");
    // Only this command bootstraps the conventional path so first-run secrets and
    // the local agent entry appear from a single intentional entry point.
    let mut created_default_config = false;
    let config_path = match args.config.clone() {
        Some(path) => PathBuf::from(path),
        None => {
            let path = match redoor_core::config::default_config_path() {
                Ok(path) => path,
                Err(error) => {
                    eprintln!("{error:#}");
                    std::process::exit(1);
                }
            };
            match redoor_core::config::create_default_config_if_missing(&path).await {
                Ok(Some(created)) => {
                    created_default_config = true;
                    if let Some(password) = created.password {
                        eprintln!(
                            "Created default config '{}'.\n  username: redoor\n  password: {password}\n  agent_token: {}\nStore these secrets securely; they will not be shown again.",
                            path.display(),
                            created.agent_token
                        );
                    } else {
                        eprintln!(
                            "Created default config '{}'.\n  browser login: process owner's system username/password (PAM)\n  agent_token: {}\nStore the agent_token securely; it will not be shown again.",
                            path.display(),
                            created.agent_token
                        );
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    eprintln!(
                        "Failed to create default config '{}': {error}",
                        path.display()
                    );
                    std::process::exit(1);
                }
            }
            path
        }
    };
    let config_path = tokio::fs::canonicalize(&config_path)
        .await
        .unwrap_or_else(|error| {
            eprintln!(
                "Failed to resolve config file '{}': {error}",
                config_path.display()
            );
            std::process::exit(1);
        });
    let config = match redoor_core::config::parse_config_file(&config_path.to_string_lossy()).await
    {
        Ok(config) => config,
        Err(error) => {
            eprintln!(
                "Failed to parse config file '{}': {error}",
                config_path.display()
            );
            std::process::exit(1);
        }
    };
    let server_section = match redoor_core::config::require_server_section(&config) {
        Ok(section) => section.clone(),
        Err(error) => {
            eprintln!(
                "Failed to parse config file '{}': {error}",
                config_path.display()
            );
            std::process::exit(1);
        }
    };

    // Precedence: CLI > env > config file > default.
    // Clap already merged CLI and env into `args`; config is the next fallback.
    let port = args.port.or(server_section.port).unwrap_or(7666);

    let bind = args
        .bind
        .clone()
        .or_else(|| server_section.bind.clone())
        .unwrap_or_else(|| "127.0.0.1".to_string());

    let log = match args.log.clone().or_else(|| server_section.log.clone()) {
        Some(path) => Some(path),
        None => match redoor_core::config::default_server_log_path() {
            Ok(path) => Some(path),
            Err(error) => {
                eprintln!("{error:#}");
                std::process::exit(1);
            }
        },
    };

    let log_level = redoor_core::logging::resolve_initial_level(
        args.log_level,
        "REDOOR_SERVER_LOG_LEVEL",
        server_section.log_level,
    )
    .unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(1);
    });
    let log_format = redoor_core::logging::resolve_initial_format(
        args.log_format,
        "REDOOR_SERVER_LOG_FORMAT",
        server_section.log_format,
    )
    .unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(1);
    });

    if let Err(error) =
        redoor_core::logging::init_with_options(log.clone(), log_level, log_format).await
    {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
    log!(
        Level::Info,
        "Loaded server config: path={}",
        config_path.display()
    );

    let (credentials, auth_mode) = match (
        server_section.username.clone(),
        server_section.password.clone(),
    ) {
        (Some(username), Some(password)) => (
            LoginCredentials::Configured { username, password },
            ServerAuthMode::Toml,
        ),
        (None, None) => {
            // Config parser already rejects this pair on non-Linux platforms.
            #[cfg(target_os = "linux")]
            {
                (LoginCredentials::SystemUser, ServerAuthMode::Pam)
            }
            #[cfg(not(target_os = "linux"))]
            {
                unreachable!("config parser requires username/password on non-Linux");
            }
        }
        _ => unreachable!("config parser rejects partial username/password pairs"),
    };
    let auth = AuthState::new(
        credentials,
        config.agent_token.clone(),
        server_section.cookie_secure,
    )
    .await
    .unwrap_or_else(|error| {
        eprintln!("Failed to initialize authentication: {error}");
        std::process::exit(1);
    });
    let machine_tokens = crate::http::machine_tokens::MachineTokenService::for_application()
        .await
        .unwrap_or_else(|error| {
            eprintln!("Failed to initialize MCP credential storage: {error:#}");
            std::process::exit(1);
        });
    let mcp = crate::http::mcp::McpState::new(
        machine_tokens.clone(),
        server_section.mcp_allowed_origins.clone(),
    )
    .await
    .unwrap_or_else(|error| {
        eprintln!("Failed to initialize MCP operation storage: {error:#}");
        std::process::exit(1);
    });

    let terminal_registry = TerminalRegistry::new();
    let log_registry = LogRegistry::new();
    let one_time_token_registry = OneTimeTokenRegistry::new();
    let (router_ref, _router_task) = spawn_router(terminal_registry.clone(), log_registry.clone());

    // Build the watchdog registry up front so the axum state and the
    // supervisor spawn loop share the same map of agent name →
    // supervisor signal. Built before binding the listener so the
    // registry exists by the time the first WebSocket connects.
    let watchdog_registry = WatchdogRegistry::new();

    // Oneshot so restart can drop the axum listener before self-exec; keeping
    // the listen FD open would race the restarted process on the same port.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let app = build_app(ServerState {
        app_name,
        router_ref: router_ref.clone(),
        agents: crate::http::agent_client::AgentClient::new(router_ref.clone()),
        transfers: crate::http::services::transfer::TransferService::new(router_ref.clone()),
        watchdog_registry: watchdog_registry.clone(),
        terminal_registry,
        log_registry,
        one_time_token_registry,
        upload_requests: crate::http::upload_requests::UploadRequests::default(),
        auth,
        machine_tokens,
        mcp,
        config_path,
        config_edit_lock: Arc::new(tokio::sync::Mutex::new(())),
        port,
        auth_mode,
        shutdown_tx: Arc::new(tokio::sync::Mutex::new(Some(shutdown_tx))),
    });

    let addr = format!("{bind}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|_| panic!("Failed to bind to address {}", addr));
    if let Err(error) = redoor_core::systemd_notify::ready().await {
        log!(
            Level::Warning,
            "Failed to notify systemd readiness: {error}"
        );
    }
    println!("Server running on http://{addr}");

    // Register configured agents after binding so later lazy starts can connect to
    // a resolved port. Registration creates dormant supervisors only; no local or
    // SSH subprocess starts until a tab, direct status route, or management action
    // requests it. Duplicate effective names remain fatal before HTTP serving.
    if let Err(error) = register_agents(
        &config.agents,
        port,
        &config.agent_token,
        &watchdog_registry,
        &router_ref,
    )
    .await
    {
        eprintln!("Failed to register managed agents: {error}");
        std::process::exit(1);
    }

    // First-run demo: open the login page with a platform-specific credential hint
    // when a graphical desktop is available so `redoor server` is enough to try the UI.
    if created_default_config && redoor_core::desktop::first_run_should_open_browser() {
        let login_url = redoor_core::desktop::first_run_login_url(&bind, port);
        println!("Opening {login_url}");
        tokio::spawn(async move {
            if let Err(error) = redoor_core::desktop::open_with_desktop(&login_url).await {
                log!(
                    Level::Warning,
                    "Failed to open first-run login URL: {error}"
                );
            }
        });
    }

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        shutdown_rx.await.ok();
    })
    .await
    .unwrap();

    // Exec preserves process memory rather than dropping Rust values, so explicitly
    // stop and reap managed children before replacing the server image. Configured
    // inventory will be recreated dormant by the new process.
    watchdog_registry.shutdown_all().await;

    // Only reached after restart (or future shutdown paths). Replace this process
    // with the same binary and arguments so startup re-reads config.toml.
    redoor_core::process::reexec_current_process();
}
