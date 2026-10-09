//! Server-side glue between the generic [`crate::watchdog`] supervisor
//! and the configured [`AgentConfig`] entries.
//!
//! Each agent config gets one supervisor task. The supervisor uses a
//! closure that knows how to spawn that specific agent's subprocess
//! (local `redoor agent` or `ssh` wrapping a remote one). The closure
//! is re-invoked on every restart cycle, so it must be safe to call
//! repeatedly.

use crate::actors::router::{
    ApplyManagedLifecycleRequest, RegisterManagedAgentRequest, RouterHandle, RouterMsg,
};
use crate::watchdog::{SnapshotCallback, SpawnFn, WatchdogRegistry, spawn_supervisor};
use anyhow::Result;
use redoor_core::log;
use redoor_protocol::ids::AgentId;
use redoor_protocol::wire::log::Level;
use tokio::process::Child;

use redoor_core::config::{
    AgentConfig, LocalAgentConfig, default_local_agent_name, spawn_local_agent,
};

/// Process-wide inputs every managed agent spawn needs besides its own TOML row.
///
/// Built once by server startup and stored in the HTTP state, so create/update routes
/// register supervisors exactly like startup does.
#[derive(Clone)]
pub(crate) struct ManagedLauncher {
    /// Listener port spawned agents connect back to.
    redoor_port: u16,
    /// Registration secret handed to spawned agents.
    agent_token: String,
    /// Namespace and release cache used by local and SSH spawns.
    provision: crate::ssh::ProvisionContext,
}

impl ManagedLauncher {
    /// Captures the listener, secret and provisioning context shared by every managed agent.
    pub(crate) fn new(
        redoor_port: u16,
        agent_token: String,
        provision: crate::ssh::ProvisionContext,
    ) -> Self {
        Self {
            redoor_port,
            agent_token,
            provision,
        }
    }

    /// Registers configured inventory and dormant supervisors without starting subprocesses.
    ///
    /// Config parsing already rejected duplicate effective names, so a registry conflict
    /// here means an earlier registration of the same name is still alive.
    pub(crate) async fn register_all(
        &self,
        configs: &[AgentConfig],
        registry: &WatchdogRegistry,
        router: &RouterHandle,
    ) -> Result<()> {
        log!(
            Level::Info,
            "Registering {} managed agent(s) from config",
            configs.len()
        );
        for config in configs.iter().cloned() {
            self.register(config, registry, router).await?;
        }
        Ok(())
    }

    /// Registers one newly persisted agent without disturbing existing supervisors or connections.
    pub(crate) async fn register(
        &self,
        config: AgentConfig,
        registry: &WatchdogRegistry,
        router: &RouterHandle,
    ) -> Result<AgentId> {
        let key = supervisor_key(&config);
        let agent_id = AgentId::from(key.clone());
        let default_directory = configured_directory(&config);
        // Both local and SSH TOML entries are operator-owned, so the UI can edit either kind.
        let configuration_editable = true;
        let ssh_target = match &config {
            AgentConfig::SshBacked(config) => Some(config.target.clone()),
            AgentConfig::Local(_) => None,
        };
        let callback = lifecycle_projection(router.clone(), agent_id.clone());
        spawn_supervisor(key, self.spawn_fn(config), registry, callback)?;
        router
            .request(|reply| {
                RouterMsg::RegisterManagedAgent(RegisterManagedAgentRequest {
                    agent_id: agent_id.clone(),
                    default_directory,
                    configuration_editable,
                    ssh_target,
                    reply,
                })
            })
            .await
            .map_err(|error| anyhow::anyhow!("Failed to register managed inventory: {error}"))?;
        Ok(agent_id)
    }

    /// Builds the spawn closure for one agent config. The closure is re-invoked on every
    /// restart cycle, so it must be safe to call repeatedly.
    fn spawn_fn(&self, config: AgentConfig) -> SpawnFn {
        match config {
            AgentConfig::Local(config) => self.local_spawn_fn(config),
            AgentConfig::SshBacked(config) => self.ssh_backed_spawn_fn(config),
        }
    }

    /// Re-invoking just spawns a fresh `redoor agent` child; the supervisor handles restarts.
    fn local_spawn_fn(&self, config: LocalAgentConfig) -> SpawnFn {
        let diagnostic_log = config.log.clone();
        let launcher = self.clone();
        let spawn = SpawnFn::new(move |_status| {
            let config = config.clone();
            let launcher = launcher.clone();
            async move {
                spawn_local_agent(
                    &config,
                    &launcher.provision.app_name,
                    launcher.redoor_port,
                    &launcher.agent_token,
                )
                .await
                .map_err(|e| e.to_string())
            }
        });
        match diagnostic_log {
            Some(path) => spawn.with_diagnostic_log(path),
            None => spawn,
        }
    }

    /// Every SSH cycle re-runs sniff and install so a restart cannot hide behind a previous prepare.
    fn ssh_backed_spawn_fn(&self, config: redoor_core::config::SshBackedAgentConfig) -> SpawnFn {
        log!(
            Level::Debug,
            "Creating managed SSH spawn closure: target={}, ssh_server_port={}, name={:?}, remote_bin={:?}, home={:?}, log={:?}, username={:?}",
            config.target,
            config
                .ssh_port
                .map(|port| port.to_string())
                .unwrap_or_else(|| "ssh-config".to_string()),
            config.name,
            config.remote_bin,
            config.home,
            config.log,
            config.username
        );
        let diagnostic_log = config.log.clone();
        let config = std::sync::Arc::new(config);
        let launcher = self.clone();
        let spawn = SpawnFn::new(move |status| {
            let launcher = launcher.clone();
            let config = config.clone();
            async move { launcher.ssh_backed_spawn_once(&config, status).await }
        })
        .with_process_name("SSH process");
        match diagnostic_log {
            Some(path) => spawn.with_diagnostic_log(path),
            None => spawn,
        }
    }

    /// Prepares then spawns one SSH-backed child. Prepare is not cached because
    /// the starting UI must show sniff results on every attempt.
    async fn ssh_backed_spawn_once(
        &self,
        config: &redoor_core::config::SshBackedAgentConfig,
        status: crate::watchdog::ProvisioningStatusSink,
    ) -> Result<Child, String> {
        let redoor_port = self.redoor_port;
        let attempt_start = std::time::Instant::now();
        log!(
            Level::Debug,
            "Managed SSH spawn attempt started: target={}, ssh_server_port={}, name={:?}, remote_bin={:?}, home={:?}, redoor_port={}, has_password={}",
            config.target,
            config
                .ssh_port
                .map(|port| port.to_string())
                .unwrap_or_else(|| "ssh-config".to_string()),
            config.name,
            config.remote_bin,
            config.home,
            redoor_port,
            config.password.is_some()
        );
        let prepared = match crate::ssh::prepare_ssh_backed_agent(
            config,
            redoor_port,
            &self.agent_token,
            &self.provision,
            &status,
        )
        .await
        {
            Ok(prepared) => {
                log!(
                    Level::Debug,
                    "Managed SSH prepare succeeded: target={}, elapsed={:?}",
                    config.target,
                    attempt_start.elapsed()
                );
                prepared
            }
            Err(error) => {
                redoor_core::log_failure!(
                    Level::Error,
                    "ssh prepare failed, will retry next cycle: target={}, elapsed={:?}, error={:#}",
                    config.target,
                    attempt_start.elapsed(),
                    error
                );
                return Err(error.to_string());
            }
        };
        // ExitOnForwardFailure makes an occupied random remote port terminate the
        // child; the supervisor's next cycle calls this again with a new port.
        let spawn_start = std::time::Instant::now();
        let result = prepared
            .spawn_managed(&status)
            .await
            .map_err(|e| e.to_string());
        match &result {
            Ok(child) => log!(
                Level::Debug,
                "Managed SSH child spawned: target={}, elapsed={:?}, child_id={:?}",
                config.target,
                spawn_start.elapsed(),
                child.id()
            ),
            Err(error) => redoor_core::log_failure!(
                Level::Error,
                "Managed SSH child spawn failed: target={}, elapsed={:?}, error={:#}",
                config.target,
                spawn_start.elapsed(),
                error
            ),
        }
        result
    }
}

/// Forwards supervisor snapshots to the router in order without blocking the supervisor.
fn lifecycle_projection(router: RouterHandle, agent_id: AgentId) -> SnapshotCallback {
    let (snapshot_sender, mut snapshot_receiver) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(snapshot) = snapshot_receiver.recv().await {
            if router
                .send_async(RouterMsg::ApplyManagedLifecycle(
                    ApplyManagedLifecycleRequest {
                        agent_id: agent_id.clone(),
                        snapshot,
                        evict_existing: false,
                        reply: None,
                    },
                ))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    std::sync::Arc::new(move |snapshot| {
        let _ = snapshot_sender.send(snapshot);
    })
}

/// Extracts only the configured browser directory, leaving unknown SSH defaults nullable.
fn configured_directory(config: &AgentConfig) -> Option<String> {
    match config {
        AgentConfig::Local(config) => config.home.clone(),
        AgentConfig::SshBacked(config) => config.home.clone(),
    }
}

/// Computes the key used to look up the supervisor from the session.
/// The key must match the name the agent registers with via its
/// `AgentRegister` message. Falls back to the system hostname for
/// local agents and the target hostname for SSH-backed agents when the
/// config omits an explicit name.
pub(crate) fn supervisor_key(config: &AgentConfig) -> String {
    match config {
        AgentConfig::Local(c) => c.name.clone().unwrap_or_else(default_local_agent_name),
        AgentConfig::SshBacked(c) => c
            .name
            .clone()
            .unwrap_or_else(|| redoor_core::config::default_agent_name(&c.target)),
    }
}
