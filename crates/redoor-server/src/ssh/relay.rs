//! `redoor agent relay start`: keeps one SSH-launched agent connected to a server.
//!
//! The relay prepares the target once (retrying until the host is reachable), then
//! relaunches the SSH session whenever it exits, each time on a fresh random reverse
//! port. OpenSSH's forwarding failure for an occupied port is detected on stderr so it
//! is reported as such rather than as an unexplained exit.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use redoor_core::config::{RelayConfig, SshBackedAgentConfig};
use redoor_core::paths::AppPaths;
use redoor_core::server_address::ServerAddress;
use redoor_core::{log, log_error};
use redoor_protocol::wire::log::Level;

use super::ProvisionContext;
use super::prepare::{
    Preparation, PreparedSshBackedAgent, RelayOptions, SecureRelayServer, TunnelDestination,
};
use crate::watchdog::ProvisioningStatusSink;

/// Initial delay keeps transient SSH failures responsive without spinning.
const RELAY_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Maximum delay bounds how long a persistent relay remains unavailable between attempts.
const RELAY_MAX_BACKOFF: Duration = Duration::from_secs(30);
/// A long-lived SSH session proves the route is healthy enough to reset retry escalation.
const RELAY_STABLE_RUNTIME: Duration = Duration::from_secs(30);

/// Stable fragment emitted by OpenSSH when `ExitOnForwardFailure` rejects a
/// remote listener, independent of the particular port that was occupied.
const REMOTE_FORWARD_FAILURE_MESSAGE: &[u8] = b"remote port forwarding failed for listen port";

/// Validates a configured relay, starts its log and runs it until the process is stopped.
///
/// Reverse port forwarding (`-R`) makes the SSH target listen on a random dynamic port
/// and asks the local SSH client to connect to the server. Plain routes use the tunnel
/// URL directly; WSS routes connect TCP through the tunnel while retaining the route
/// hostname for TLS and HTTP. Only returns on a startup error.
pub async fn run_relay(
    relay: RelayConfig,
    token: String,
    agent_app_name: String,
    paths: AppPaths,
) -> Result<()> {
    let server = relay
        .server
        .parse::<ServerAddress>()
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("invalid server in relay '{}'", relay.id))?;
    if relay.insecure && !server.is_secure() {
        bail!("relay insecure = true requires an https:// or wss:// server URL");
    }
    if relay.insecure {
        eprintln!(
            "WARNING: relay insecure = true disables TLS certificate verification for the routed server"
        );
    }
    let log = relay
        .agent
        .log
        .clone()
        .context("relay startup must resolve a log path")?;
    redoor_core::logging::init(Some(log)).await?;
    let provision = ProvisionContext {
        app_name: paths.app_name().to_string(),
        // Relays never have managed keys; the store is only consulted for `managed_ssh_key`.
        ssh_keys: crate::ssh::keys::SshKeyStore::for_data_dir(&paths.data_dir()?),
        binaries: crate::binaries::BinaryCache::new(paths),
    };
    Relay {
        secure_server: server.is_secure().then(|| SecureRelayServer {
            authority: server.authority(),
            insecure: relay.insecure,
        }),
        destination: TunnelDestination {
            host: server.host().to_string(),
            port: server.port(),
        },
        config: relay.agent,
        binary_source: relay.binary_source,
        agent_app_name,
        token,
        provision,
        backoff: Backoff::default(),
    }
    .run()
    .await
}

/// One running relay: its target, where the tunnel ends, and the retry state.
struct Relay {
    /// SSH target settings shared with server-managed agents.
    config: SshBackedAgentConfig,
    /// Where the reverse tunnel ends, as reached from this machine.
    destination: TunnelDestination,
    /// TLS identity kept in the agent URL for `wss://` servers.
    secure_server: Option<SecureRelayServer>,
    /// Local binary uploaded before the first launch, when configured.
    binary_source: Option<PathBuf>,
    /// Remote namespace isolating this relay's agent state.
    agent_app_name: String,
    /// Agent registration secret.
    token: String,
    /// Local namespace and release cache used during preparation.
    provision: ProvisionContext,
    /// Delay before the next attempt.
    backoff: Backoff,
}

/// How one SSH session ended.
enum AttemptOutcome {
    /// OpenSSH could not bind the random remote port (another listener owns it).
    PortOccupied {
        remote_port: u16,
        status: std::process::ExitStatus,
    },
    /// The session ran and exited for any other reason.
    Exited {
        remote_port: u16,
        status: std::process::ExitStatus,
    },
}

impl Relay {
    /// Prepares until it succeeds, then relaunches the SSH session forever.
    ///
    /// Preparation is not repeated after success: provisioning is the slow part and the
    /// installed binary stays valid across reconnects.
    async fn run(mut self) -> Result<()> {
        let prepared = self.prepare_until_ready().await;
        loop {
            let started = Instant::now();
            match Self::run_attempt(&prepared).await {
                Ok(AttemptOutcome::PortOccupied {
                    remote_port,
                    status,
                }) => log!(
                    Level::Warning,
                    "SSH remote port was occupied; relay will retry: remote_port={remote_port}, status={status}"
                ),
                Ok(AttemptOutcome::Exited {
                    remote_port,
                    status,
                }) => log!(
                    Level::Warning,
                    "Relay SSH session exited; relay will retry: remote_port={remote_port}, status={status}"
                ),
                Err(error) => log_error!(
                    anyhow::Error::new(error),
                    "Relay SSH attempt failed; relay will retry"
                ),
            }
            if started.elapsed() >= RELAY_STABLE_RUNTIME {
                self.backoff.reset();
            }
            self.backoff.wait().await;
        }
    }

    /// Retries preparation with backoff until the target is reachable and provisioned.
    async fn prepare_until_ready(&mut self) -> PreparedSshBackedAgent {
        loop {
            match self.prepare().await {
                Ok(prepared) => return prepared,
                Err(error) => {
                    log_error!(anyhow::Error::new(error), "Relay preparation failed");
                    self.backoff.wait().await;
                }
            }
        }
    }

    /// Runs one preparation with the relay-specific options.
    async fn prepare(&self) -> Result<PreparedSshBackedAgent, super::ProvisionError> {
        Preparation {
            config: &self.config,
            destination: self.destination.clone(),
            agent_token: &self.token,
            relay: RelayOptions {
                monitor_forward_failure: true,
                secure_server: self.secure_server.clone(),
                binary_source: self.binary_source.as_deref(),
                agent_app_name: Some(self.agent_app_name.clone()),
            },
            provision: &self.provision,
            status: &ProvisioningStatusSink::noop(),
        }
        .run()
        .await
    }

    /// Runs one random-port SSH session to completion and classifies how it ended.
    async fn run_attempt(prepared: &PreparedSshBackedAgent) -> std::io::Result<AttemptOutcome> {
        let (remote_port, mut child) = prepared.spawn_random().await?;
        let stderr = child.stderr.take().ok_or_else(|| {
            std::io::Error::other("standalone SSH stderr was not piped for forward monitoring")
        })?;
        let forward_failed = monitor_forward_failure(stderr, prepared.options.log_file().cloned())
            .await
            .unwrap_or(false);
        let status = child.wait().await?;
        Ok(if forward_failed {
            AttemptOutcome::PortOccupied {
                remote_port,
                status,
            }
        } else {
            AttemptOutcome::Exited {
                remote_port,
                status,
            }
        })
    }
}

/// Exponential retry delay shared by preparation and relaunch.
struct Backoff {
    /// Delay used by the next `wait`.
    current: Duration,
}

impl Default for Backoff {
    /// Starts at the shortest delay so the first retry is quick.
    fn default() -> Self {
        Self {
            current: RELAY_INITIAL_BACKOFF,
        }
    }
}

impl Backoff {
    /// Returns to the shortest delay after a session proved the route healthy.
    fn reset(&mut self) {
        self.current = RELAY_INITIAL_BACKOFF;
    }

    /// Returns the delay to wait now and doubles the next one up to the cap.
    fn advance(&mut self) -> Duration {
        let delay = self.current;
        self.current = (self.current * 2).min(RELAY_MAX_BACKOFF);
        delay
    }

    /// Waits so unavailable SSH hosts do not consume a CPU core.
    async fn wait(&mut self) {
        let delay = self.advance();
        log!(Level::Info, "Relay retry scheduled: backoff={delay:?}");
        tokio::time::sleep(delay).await;
    }
}

/// Incrementally detects OpenSSH's forwarding failure across fixed-size stderr
/// chunks without retaining an unbounded agent log line in memory.
#[derive(Default)]
struct ForwardFailureDetector {
    /// Keeps only enough bytes to detect a message split between two chunks.
    tail: Vec<u8>,
}

impl ForwardFailureDetector {
    /// Observes one stderr chunk and reports whether it completes the known
    /// OpenSSH failure phrase, comparing ASCII output case-insensitively.
    fn observe(&mut self, chunk: &[u8]) -> bool {
        let mut searchable = Vec::with_capacity(self.tail.len() + chunk.len());
        searchable.extend_from_slice(&self.tail);
        searchable.extend_from_slice(chunk);
        searchable.make_ascii_lowercase();
        let found = searchable
            .windows(REMOTE_FORWARD_FAILURE_MESSAGE.len())
            .any(|window| window == REMOTE_FORWARD_FAILURE_MESSAGE);
        let retained = REMOTE_FORWARD_FAILURE_MESSAGE
            .len()
            .saturating_sub(1)
            .min(searchable.len());
        self.tail.clear();
        self.tail
            .extend_from_slice(&searchable[searchable.len() - retained..]);
        found
    }
}

/// Copies piped SSH stderr to the relay's terminal and log while reporting whether
/// OpenSSH said its random remote listener could not bind.
fn monitor_forward_failure<R>(
    mut input: R,
    log_path: Option<String>,
) -> tokio::sync::oneshot::Receiver<bool>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let (sender, receiver) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut sender = Some(sender);
        let mut detector = ForwardFailureDetector::default();
        let mut output = tokio::io::stderr();
        let mut log_file = match log_path {
            Some(path) => tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await
                .ok(),
            None => None,
        };
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            match input.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let chunk = &buffer[..read];
                    let _ = output.write_all(chunk).await;
                    if let Some(log_file) = &mut log_file {
                        let _ = log_file.write_all(chunk).await;
                    }
                    if detector.observe(chunk)
                        && let Some(sender) = sender.take()
                    {
                        let _ = sender.send(true);
                    }
                }
            }
        }
        if let Some(sender) = sender {
            let _ = sender.send(false);
        }
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies forwarding failures are recognized even when the fixed-size
    /// stderr reader splits OpenSSH's message across chunks.
    #[test]
    fn detects_split_remote_forward_failure() {
        let mut detector = ForwardFailureDetector::default();

        // A partial phrase must not trigger a retry before OpenSSH reports the failure.
        assert!(!detector.observe(b"Warning: remote port forward"));
        // Completing the phrase across the next chunk must identify the occupied port.
        assert!(detector.observe(b"ing failed for listen port 52000\n"));
    }

    /// The delay doubles per failure, stays capped, and resets after a healthy session.
    #[test]
    fn backoff_doubles_caps_and_resets() {
        let mut backoff = Backoff::default();
        // Consecutive failures wait progressively longer.
        assert_eq!(backoff.advance(), Duration::from_secs(1));
        assert_eq!(backoff.advance(), Duration::from_secs(2));
        for _ in 0..10 {
            backoff.advance();
        }
        // Repeated failures must never wait longer than the documented maximum.
        assert_eq!(backoff.advance(), RELAY_MAX_BACKOFF);
        backoff.reset();
        // A stable session makes the next failure retry quickly again.
        assert_eq!(backoff.current, RELAY_INITIAL_BACKOFF);
    }

    /// Verifies URL servers preserve the destination independently of the port
    /// exposed to the agent on the SSH target.
    #[test]
    fn parses_hostname_server_url() {
        let server = "http://redoor.example:4000"
            .parse::<ServerAddress>()
            .expect("hostname server URL should parse");

        // The SSH client must resolve the exact host supplied by the operator.
        assert_eq!(server.host(), "redoor.example");
        // The destination port may differ from the remote listener port.
        assert_eq!(server.port(), 4000);
        // http:// selects plain WebSocket routing through the tunnel.
        assert!(!server.is_secure());
    }

    /// Verifies https/wss schemes select secure tunnel routing without a separate flag.
    #[test]
    fn parses_secure_server_url() {
        let server = "https://redoor.example.com"
            .parse::<ServerAddress>()
            .expect("https server URL should parse");

        assert!(server.is_secure());
        assert_eq!(server.port(), 443);
        assert_eq!(server.authority(), "redoor.example.com:443");
    }

    /// Verifies bracketed IPv6 servers are accepted without retaining syntax
    /// brackets in the destination model.
    #[test]
    fn parses_bracketed_ipv6_server_url() {
        let server = "ws://[2001:db8::10]:4000"
            .parse::<ServerAddress>()
            .expect("bracketed IPv6 server URL should parse");

        // Transport formatting adds brackets itself, so the model stores only the address.
        assert_eq!(server.host(), "2001:db8::10");
        // IPv6 servers use the same validated destination-port representation.
        assert_eq!(server.port(), 4000);
    }

    /// Verifies bare host:port is rejected so relay matches the agent URL contract.
    #[test]
    fn rejects_server_without_url_scheme() {
        let error = "redoor.example:4000"
            .parse::<ServerAddress>()
            .expect_err("bare host:port should be rejected");

        // A focused error steers operators toward the shared URL forms.
        assert!(error.contains("http") || error.contains("ws") || error.contains("URL"));
    }
}
