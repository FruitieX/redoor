//! Starts redoor agents on remote hosts and connects them back to the local
//! server through SSH reverse forwarding.
//!
//! # How SSH-backed agent spawning works
//!
//! Preparation ([`prepare`]) and spawning are separate so TOML-managed agents can
//! restart cheaply. Preparation probes the remote operating system and architecture,
//! installs a compatible redoor binary when the operator did not provide one,
//! and stores the settings needed for subsequent launches.
//!
//! Each launch starts `ssh` with a reverse forward shaped like
//! `remote_port:destination_host:destination_port`. SSH listens on
//! `remote_port` on the remote host and asks the local SSH client to connect to
//! the destination. Managed agents use the local redoor server, while
//! a named standalone relay ([`relay`]) uses a server reachable from the
//! machine running the command. The remote agent is given
//! `ws://localhost:<remote_port>/ws`, so what looks like a local WebSocket
//! connection to the agent actually reaches the server through that tunnel.
//! Standalone WSS routes retain the real server authority for TLS and HTTP but
//! open their TCP connections through the same random localhost tunnel port.
//!
//! The agent token is not included in either process's command-line arguments.
//! The local process writes it as the first line of SSH stdin; a small remote
//! shell preamble reads it, exports it as `REDOOR_AGENT_TOKEN`, and `exec`s the
//! agent so it inherits the secret only through its environment. Remaining
//! stdin is then forwarded normally.
//!
//! Optional `password` values are delivered through OpenSSH `SSH_ASKPASS` by
//! re-executing this binary. That keeps the secret off argv and off the SSH
//! stdin already used for tokens, sniff scripts, and uploads.
//!
//! Both standalone and TOML-managed launches select a random dynamic remote
//! port for each SSH spawn attempt. `ExitOnForwardFailure=yes` makes SSH exit
//! when that port is already occupied. Managed agents let the watchdog retry;
//! standalone relays detect that specific bind failure and retry locally.

pub mod askpass;
mod prepare;
mod provision;
pub mod relay;
mod transport;

use redoor_core::config::SshBackedAgentConfig;

use crate::watchdog::ProvisioningStatusSink;
use prepare::{Preparation, PreparedSshBackedAgent, RelayOptions, TunnelDestination};

/// Why preparing an SSH target failed; every variant's text is shown to operators as is.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ProvisionError {
    /// Running `ssh` or reading a local file failed; SSH failures already carry
    /// sanitized, actionable guidance (authentication, host keys, timeouts).
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// The matching release binary could not be cached locally.
    #[error("{0}")]
    Release(#[from] crate::binaries::UpgradeBinaryError),
    /// The remote host answered unexpectedly (unsupported platform, failed command,
    /// a binary that does not verify after upload).
    #[error("{0}")]
    Remote(String),
}

/// Local facts every SSH preparation needs besides the target's own settings.
#[derive(Clone)]
pub(crate) struct ProvisionContext {
    /// Local namespace: installs land under it remotely and spawned agents inherit it.
    pub(crate) app_name: String,
    /// Release cache used when the target lacks a matching binary.
    pub(crate) binaries: crate::binaries::BinaryCache,
}

impl ProvisionContext {
    /// Prepares a server-managed agent whose tunnel ends at this server's own listener.
    ///
    /// Called on every watchdog cycle so the starting UI always reflects a fresh probe.
    pub(crate) async fn prepare_managed(
        &self,
        config: &SshBackedAgentConfig,
        redoor_port: u16,
        agent_token: &str,
        status: &ProvisioningStatusSink,
    ) -> Result<PreparedSshBackedAgent, ProvisionError> {
        Preparation {
            config,
            destination: TunnelDestination {
                host: "localhost".to_string(),
                port: redoor_port,
            },
            agent_token,
            relay: RelayOptions::default(),
            provision: self,
            status,
        }
        .run()
        .await
    }
}
