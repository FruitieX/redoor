//! Shared TOML config used by the server, standalone agent, and service installers.
//!
//! Top-level `agent_token` is the shared registration secret. Optional
//! `[server]` holds listener and browser-auth settings; optional `[agent]`
//! holds standalone agent connection settings; optional `[[agents]]` lists
//! server-managed local/SSH-backed agents; optional `[[relays]]` lists independently
//! started SSH relays. Server mode requires `[server]`; agent
//! mode resolves required fields from CLI > env > config > default.

mod bootstrap;
mod edit;
mod file;
mod import;
mod local_agent;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub use bootstrap::{create_default_config_if_missing, default_local_agent_name};
pub use edit::{append_local_agent, append_ssh_agent, edit_local_agent, edit_ssh_agent};
pub use import::import_agent_config_from_stdin;
pub use local_agent::spawn_local_agent;

use crate::logging::LogFormat;
use redoor_protocol::wire::log::Level;

/// Derives a default agent name from the ssh target by stripping any
/// `user@` prefix so the name reflects the host being connected to.
pub fn default_agent_name(target: &str) -> String {
    target.rsplit('@').next().unwrap_or(target).to_string()
}

/// Why a config file could not be loaded; every variant renders a complete operator message.
///
/// Callers only display these, but a typed error keeps the read/syntax/semantic split
/// visible and lets the restart route answer 400 without string matching.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read at all.
    #[error("Failed to read config file '{}': {source}", path.display())]
    Read {
        /// File the caller asked for.
        path: PathBuf,
        /// Underlying I/O failure (missing file, permissions, ...).
        source: std::io::Error,
    },
    /// TOML syntax, unknown keys or wrongly typed values, with serde's location snippet.
    #[error("Failed to parse config file '{}': {message}", path.display())]
    Syntax {
        /// File whose content was rejected.
        path: PathBuf,
        /// Rendered `toml_edit` diagnostic (line, column, offending key and reason).
        message: String,
    },
    /// A rule spanning several keys; the message names the key path and the fix.
    #[error("Invalid config file '{}': {message}", path.display())]
    Invalid {
        /// File whose content broke the rule.
        path: PathBuf,
        /// Key path plus reason, e.g. `server.password is required when ...`.
        message: String,
    },
    /// A server entry point was given a file without `[server]`.
    #[error("config file must contain a [server] table")]
    MissingServerSection,
}

/// Configuration for one SSH-backed agent, independent of any specific CLI
/// surface so both `redoor agent relay` and `redoor server --agents` can construct it
/// without depending on clap.
///
/// `remote_bin` is optional so callers that want the versioned default
/// (`${XDG_DATA_HOME:-$HOME/.local/share}/<app-name>/binaries/<version>/redoor`)
/// don't have to compute it themselves;
/// `start_relay` fills it in when `None`.
#[derive(Clone)]
pub struct SshBackedAgentConfig {
    /// SSH login username. Forwarded to ssh via `-l`. When `None`, ssh config
    /// or the `user@host` target syntax supplies the username.
    pub username: Option<String>,
    /// SSH server port override. `None` preserves the port resolved by OpenSSH
    /// from its host configuration or built-in default.
    pub ssh_port: Option<u16>,
    /// Name the remote agent registers with on the server. When `None`,
    /// defaults to the host portion of `target`.
    pub name: Option<String>,
    /// Path to the redoor binary on the remote host. When `None`, defaults to
    /// the versioned install layout.
    pub remote_bin: Option<String>,
    /// Default UI home directory; when absent the remote process user's home is published.
    pub home: Option<String>,
    /// Remote ssh target in `user@host` form.
    pub target: String,
    /// Optional local log file path. When set, the ssh process's
    /// stdout/stderr is redirected to this file so the remote agent's
    /// logs (forwarded through ssh) are captured locally. The file is
    /// opened in append mode so agent restarts accumulate logs.
    pub log: Option<String>,
    /// Optional SSH login password. When set, OpenSSH is given `SSH_ASKPASS`
    /// pointing at this binary so password auth can run without a TTY or
    /// stealing the stdin used for tokens and uploads.
    pub password: Option<String>,
}

impl std::fmt::Debug for SshBackedAgentConfig {
    /// Redacts the SSH password so debug logging cannot leak it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshBackedAgentConfig")
            .field("username", &self.username)
            .field("ssh_port", &self.ssh_port)
            .field("name", &self.name)
            .field("remote_bin", &self.remote_bin)
            .field("home", &self.home)
            .field("target", &self.target)
            .field("log", &self.log)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .finish()
    }
}

/// Configuration for one local agent, parsed from the agents toml.
///
/// Mirrors the subset of [`SshBackedAgentConfig`] fields that make
/// sense for an in-process agent: a display name, an optional UI default
/// directory, and an optional log file. The server reuses its own binary
/// (via `std::env::current_exe`) to start the agent, so no binary path
/// needs to be configured.
#[derive(Debug, Clone)]
pub struct LocalAgentConfig {
    /// Name the local agent registers with on the server. When `None`,
    /// defaults to the system hostname so multiple local agents on
    /// different machines are naturally distinguishable.
    pub name: Option<String>,
    /// Home directory the spawned agent publishes for UI tab navigation.
    pub home: Option<String>,
    /// Log file path. When set, the spawned `redoor agent` process's
    /// stdout/stderr is redirected (append mode) to this file. When
    /// `None`, stdio is inherited so the agent's logs appear in the
    /// server's terminal.
    pub log: Option<String>,
}

/// One configured agent entry from the agents toml. The variant decides
/// whether the server can start an SSH-backed agent or a plain local one,
/// so the dispatcher in `register_agents` can pick the right transport
/// without inspecting the per-variant fields itself.
#[derive(Debug, Clone)]
pub enum AgentConfig {
    SshBacked(SshBackedAgentConfig),
    Local(LocalAgentConfig),
}

/// Browser login backend selected by the `[server]` credentials.
///
/// An enum instead of two optional strings: the "exactly both or neither" rule is
/// enforced once at parse time, and the PAM variant only exists where PAM does, so
/// no consumer has to handle impossible combinations.
#[derive(Clone)]
pub enum AuthMode {
    /// `username`/`password` pair from the TOML file.
    Configured {
        /// Login name, already restricted to a portable path component.
        username: String,
        /// Plaintext password; hashed by the server at startup.
        password: String,
    },
    /// Linux PAM for the account running the server, chosen when TOML omits both credentials.
    #[cfg(target_os = "linux")]
    SystemAccount,
}

impl std::fmt::Debug for AuthMode {
    /// Redacts the configured password so config debugging cannot leak it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configured { username, .. } => f
                .debug_struct("Configured")
                .field("username", username)
                .field("password", &"***")
                .finish(),
            #[cfg(target_os = "linux")]
            Self::SystemAccount => f.write_str("SystemAccount"),
        }
    }
}

impl From<&AuthMode> for redoor_protocol::api::server::ServerAuthMode {
    /// Projects the backend into the public server-info vocabulary shown by the UI.
    fn from(mode: &AuthMode) -> Self {
        match mode {
            AuthMode::Configured { .. } => Self::Toml,
            #[cfg(target_os = "linux")]
            AuthMode::SystemAccount => Self::Pam,
        }
    }
}

/// Parsed `[server]` table with optional listener settings and the resolved login backend.
///
/// The shared `agent_token` lives at the document root, not here.
#[derive(Debug, Clone)]
pub struct ServerSection {
    /// Listener port; CLI and env override it.
    pub port: Option<u16>,
    /// Listener address; CLI overrides it.
    pub bind: Option<String>,
    /// Server log file; CLI and env override it.
    pub log: Option<String>,
    /// Initial server threshold; runtime API changes deliberately do not rewrite it.
    pub log_level: Option<Level>,
    /// Initial process output format; browser streams remain structured.
    pub log_format: Option<LogFormat>,
    /// Browser login backend derived from `username`/`password`.
    pub auth: AuthMode,
    /// When true, session cookies are marked `Secure` for HTTPS deployments.
    pub cookie_secure: bool,
    /// Browser origins allowed to call MCP; origin-less native clients do not need an entry.
    pub mcp_allowed_origins: Vec<String>,
}

/// Parsed `[agent]` table for a standalone `redoor agent` process.
///
/// All fields are optional in the file so CLI and env can supply missing
/// values; agent startup requires `server` and top-level `agent_token`
/// after applying CLI > env > config > default precedence. The name defaults
/// to the computer hostname.
#[derive(Debug, Clone, Default)]
pub struct AgentSection {
    /// Redoor server URL (`http(s)://` or `ws(s)://`); path optional and forced to `/ws`.
    pub server: Option<String>,
    /// Registration name; defaults to the hostname.
    pub name: Option<String>,
    /// Directory the UI opens first.
    pub home: Option<String>,
    /// Agent log file.
    pub log: Option<String>,
    /// Initial standalone-agent threshold before runtime control becomes available.
    pub log_level: Option<Level>,
    /// Initial process output format; browser streams remain structured.
    pub log_format: Option<LogFormat>,
}

/// Full parsed config file shared by server and agent processes.
#[derive(Debug, Clone)]
pub struct RedoorConfig {
    /// Shared secret agents present when registering over `/ws`.
    pub agent_token: String,
    /// Present when the file configures a server process.
    pub server: Option<ServerSection>,
    /// Present when the file configures a standalone agent process.
    pub agent: Option<AgentSection>,
    /// Server-managed local/SSH-backed agents (server mode only).
    pub agents: Vec<AgentConfig>,
    /// Named SSH relays that operators start and stop independently.
    pub relays: Vec<RelayConfig>,
}

/// Configuration for one independently managed SSH relay.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// Stable local identity used for lifecycle commands and runtime files.
    pub id: String,
    /// Redoor server URL reached from the machine running the relay.
    pub server: String,
    /// Whether routed TLS certificate verification is intentionally disabled.
    pub insecure: bool,
    /// Optional local binary that is uploaded before starting the remote agent.
    pub binary_source: Option<PathBuf>,
    /// Optional remote process namespace; defaults to one derived from the local app and relay ID.
    pub agent_app_name: Option<String>,
    /// SSH-backed agent settings shared with server-managed agents.
    pub agent: SshBackedAgentConfig,
}

/// Reads and validates the shared config file used by both server and agent.
///
/// Top-level non-empty `agent_token` is always required. `[server]`, `[agent]`,
/// and `[[agents]]` are optional at parse time; each process validates the
/// sections it needs after applying CLI/env overrides. Reading goes through serde
/// (`toml_edit::de`) so unknown keys and wrong types are reported with their line;
/// `toml_edit::DocumentMut` is used only by the format-preserving edit helpers.
pub async fn parse_config_file(path: &Path) -> Result<RedoorConfig, ConfigError> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    parse_config_str(path, &content)
}

/// Parses already-loaded file content; split out so tests can exercise rules without files.
fn parse_config_str(path: &Path, content: &str) -> Result<RedoorConfig, ConfigError> {
    let raw: file::RawConfig =
        toml_edit::de::from_str(content).map_err(|error| ConfigError::Syntax {
            path: path.to_path_buf(),
            message: error.to_string().trim_end().to_string(),
        })?;
    raw.validate()
        .map_err(|file::Invalid(message)| ConfigError::Invalid {
            path: path.to_path_buf(),
            message,
        })
}

/// Returns the named relay or an actionable error listing the missing identity.
pub fn require_relay<'a>(config: &'a RedoorConfig, id: &str) -> Result<&'a RelayConfig> {
    config
        .relays
        .iter()
        .find(|relay| relay.id == id)
        .with_context(|| format!("relay '{id}' is not configured"))
}

/// Restricts login names to one portable path component so account files cannot escape the data dir.
pub fn parse_server_username(value: &str) -> Result<String, String> {
    if value.is_empty() || value == "." || value == ".." {
        return Err("server.username must not be empty, '.' or '..'".to_string());
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(
            "server.username may contain only ASCII letters, numbers, '.', '_' and '-'".to_string(),
        );
    }
    Ok(value.to_string())
}

/// Restricts relay IDs to safe, portable runtime-file components.
pub fn parse_relay_id(value: &str) -> Result<String, String> {
    if value.is_empty() || value == "." || value == ".." {
        return Err("relay ID must not be empty, '.' or '..'".to_string());
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(
            "relay ID may contain only ASCII letters, numbers, '.', '_' and '-'".to_string(),
        );
    }
    Ok(value.to_string())
}

/// Returns the `[server]` section or a clear error for server-only entry points.
pub fn require_server_section(config: &RedoorConfig) -> Result<&ServerSection, ConfigError> {
    config
        .server
        .as_ref()
        .ok_or(ConfigError::MissingServerSection)
}

/// Returns whether `[agent]` supplies every configured field a bare `redoor agent` needs.
///
/// The name may be omitted because agent startup uses the computer hostname.
/// Used by systemd agent setup so the unit can omit CLI flags safely.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn standalone_agent_is_fully_configured(config: &RedoorConfig) -> bool {
    let Some(agent) = config.agent.as_ref() else {
        return false;
    };
    !config.agent_token.is_empty() && agent.server.as_ref().is_some_and(|value| !value.is_empty())
}

// The REST views live with the config types so the server can convert without orphan wrappers.
impl From<LocalAgentConfig>
    for redoor_protocol::api::agents::ManagedLocalAgentConfigurationResponse
{
    /// Exposes the same optional local fields the add/edit form can persist.
    fn from(config: LocalAgentConfig) -> Self {
        Self {
            name: config.name,
            home: config.home,
            log: config.log,
        }
    }
}

impl From<SshBackedAgentConfig>
    for redoor_protocol::api::agents::ManagedSshAgentConfigurationResponse
{
    /// Exposes editable fields without the stored password so GET cannot leak the secret.
    fn from(config: SshBackedAgentConfig) -> Self {
        Self {
            target: config.target,
            username: config.username,
            ssh_port: config.ssh_port,
            name: config.name,
            remote_bin: config.remote_bin,
            home: config.home,
            log: config.log,
            password: None,
            has_password: config.password.is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates an owned directory and places the config fixture beneath it.
    fn test_config_path() -> (crate::test_support::TempDir, PathBuf) {
        let directory = crate::test_support::TempDir::create();
        let path = directory.path().join("config.toml");
        (directory, path)
    }

    /// Ensures login names stay inside the same allowlist used for on-disk account directories.
    #[test]
    fn parse_server_username_allows_portable_path_components() {
        for valid in ["test-user", "alice", "user.name", "user_1"] {
            assert_eq!(
                parse_server_username(valid).as_deref(),
                Ok(valid),
                "a portable login name should be accepted: {valid}"
            );
        }
        for invalid in [
            "",
            ".",
            "..",
            "alice/bob",
            "alice\\bob",
            "al\0ice",
            "user name",
            "user@host",
            "ålice",
        ] {
            assert!(
                parse_server_username(invalid).is_err(),
                "a non-portable login name should be rejected: {invalid:?}"
            );
        }
    }

    /// Adds required top-level token and server credentials without obscuring each test's payload.
    fn write_test_config(path: &std::path::Path, content: impl AsRef<str>) -> std::io::Result<()> {
        let content = content.as_ref();
        let token_line = "agent_token = \"test-agent-token\"\n";
        let credentials = "username = \"test-user\"\npassword = \"test-password\"\n";
        let server_header = "[server]\n";
        let with_server = if content.contains(server_header) {
            content.replacen(server_header, &format!("{server_header}{credentials}"), 1)
        } else {
            format!(
                r#"[server]
{credentials}
{content}"#
            )
        };
        let complete = if with_server.contains("agent_token") {
            with_server
        } else {
            format!("{token_line}\n{with_server}")
        };
        std::fs::write(path, complete)
    }

    /// Verifies that all optional fields remain unset when omitted so OpenSSH
    /// can apply host-specific configuration for a minimal target-only entry.
    #[tokio::test]
    async fn test_parse_config_file_minimal_entry() {
        let (_directory, temp) = test_config_path();
        write_test_config(
            &temp,
            r#"[[agents]]
target = "user@example.com"
"#,
        )
        .unwrap();

        let config = parse_config_file(&temp).await.unwrap();

        assert_eq!(
            config.agents.len(),
            1,
            "exactly one agent entry should be parsed"
        );
        let agent = match &config.agents[0] {
            AgentConfig::SshBacked(config) => config,
            AgentConfig::Local(_) => panic!("entry without `local = true` should be SSH-backed"),
        };
        assert_eq!(agent.target, "user@example.com");
        // An omitted port must defer to OpenSSH host configuration rather than force port 22.
        assert_eq!(agent.ssh_port, None);
        // username, name, remote_bin and home are None so the SSH launcher can
        // derive them (default name from target, default remote_bin from
        // versioned layout, default home from the remote process user).
        assert!(agent.username.is_none());
        assert!(agent.name.is_none());
        assert!(agent.remote_bin.is_none());
        assert!(agent.home.is_none());
        assert!(agent.log.is_none(), "log should be None when not specified");
    }

    /// Verifies that every supported field is read from the toml file so
    /// operators can override the defaults per agent.
    #[tokio::test]
    async fn test_parse_config_file_full_entry() {
        let (_directory, temp) = test_config_path();
        let content = r#"
[[agents]]
target = "prod-db"
username = "deploy"
ssh_port = 2222
name = "db-agent"
remote_bin = "/usr/local/bin/redoor"
home = "/srv/app"
log = "log/db-agent.log"

[[agents]]
target = "web-1"
"#;
        write_test_config(&temp, content).unwrap();

        let config = parse_config_file(&temp).await.unwrap();

        assert_eq!(config.agents.len(), 2, "both entries should be parsed");
        let first = match &config.agents[0] {
            AgentConfig::SshBacked(config) => config,
            AgentConfig::Local(_) => panic!("entry without `local = true` should be SSH-backed"),
        };
        assert_eq!(first.target, "prod-db");
        assert_eq!(first.username.as_deref(), Some("deploy"));
        assert_eq!(first.ssh_port, Some(2222));
        assert_eq!(first.name.as_deref(), Some("db-agent"));
        assert_eq!(first.remote_bin.as_deref(), Some("/usr/local/bin/redoor"));
        assert_eq!(first.home.as_deref(), Some("/srv/app"));
        assert_eq!(first.log.as_deref(), Some("log/db-agent.log"));

        // The second entry defers to OpenSSH while the first overrides its port explicitly.
        let second = match &config.agents[1] {
            AgentConfig::SshBacked(config) => config,
            AgentConfig::Local(_) => panic!("entry without `local = true` should be SSH-backed"),
        };
        assert_eq!(second.target, "web-1");
        assert_eq!(second.ssh_port, None);
    }

    /// Verifies that credentials-only configuration can run without managed agents.
    #[tokio::test]
    async fn test_parse_config_file_accepts_missing_agents_key() {
        let (_directory, temp) = test_config_path();
        write_test_config(&temp, "port = 3000\n").unwrap();

        let config = parse_config_file(&temp).await.unwrap();

        assert!(
            config.agents.is_empty(),
            "a credentials-only server should not require managed agents"
        );
    }

    /// Verifies that an entry without a `target` is rejected so the parser
    /// fails fast on an incomplete entry instead of producing an agent with
    /// nothing to connect to.
    #[tokio::test]
    async fn test_parse_config_file_rejects_entry_without_target() {
        let (_directory, temp) = test_config_path();
        write_test_config(
            &temp,
            r#"[[agents]]
name = "no-target"
"#,
        )
        .unwrap();

        let result = parse_config_file(&temp).await;

        assert!(
            result.is_err(),
            "an entry without a target should be rejected"
        );
    }

    /// Verifies that a present-but-non-integer `ssh_port` is rejected rather
    /// than silently falling back to 22, so a typo like a string value is
    /// surfaced as an explicit operator error.
    #[tokio::test]
    async fn test_parse_config_file_rejects_non_integer_ssh_port() {
        let (_directory, temp) = test_config_path();
        std::fs::write(
            &temp,
            r#"[[agents]]
target = "host"
ssh_port = "not-a-port"
"#,
        )
        .unwrap();

        let result = parse_config_file(&temp).await;

        assert!(result.is_err(), "a non-integer ssh_port should be rejected");
    }

    /// Verifies that an out-of-range `ssh_port` is rejected rather than
    /// silently truncating, so the operator gets a clear error for a typo.
    #[tokio::test]
    async fn test_parse_config_file_rejects_out_of_range_port() {
        let (_directory, temp) = test_config_path();
        write_test_config(
            &temp,
            r#"[[agents]]
target = "host"
ssh_port = 99999
"#,
        )
        .unwrap();

        let result = parse_config_file(&temp).await;

        assert!(
            result.is_err(),
            "an out-of-range ssh_port should be rejected"
        );
    }

    /// Verifies that a `local = true` entry without any other fields parses
    /// into a [`AgentConfig::Local`] with all optional fields `None`, so the
    /// runtime can fall back to its own defaults (hostname, process user home,
    /// inherited stdio).
    #[tokio::test]
    async fn test_parse_config_file_minimal_local_entry() {
        let (_directory, temp) = test_config_path();
        write_test_config(
            &temp,
            r#"[[agents]]
local = true
"#,
        )
        .unwrap();

        let config = parse_config_file(&temp).await.unwrap();

        assert_eq!(
            config.agents.len(),
            1,
            "exactly one agent entry should be parsed"
        );
        let agent = match &config.agents[0] {
            AgentConfig::Local(config) => config,
            AgentConfig::SshBacked(_) => panic!("entry with `local = true` should be local"),
        };
        assert!(
            agent.name.is_none(),
            "name should be None so the runtime defaults to hostname"
        );
        assert!(agent.home.is_none(), "home should be None by default");
        assert!(agent.log.is_none(), "log should be None by default");
    }

    /// Verifies that every supported local field is read from the toml file
    /// so operators can override the defaults per agent.
    #[tokio::test]
    async fn test_parse_config_file_full_local_entry() {
        let (_directory, temp) = test_config_path();
        let content = r#"
[[agents]]
local = true
name = "my-local"
home = "/var/work"
log = "/var/log/my-local.log"
"#;
        write_test_config(&temp, content).unwrap();

        let config = parse_config_file(&temp).await.unwrap();

        assert_eq!(
            config.agents.len(),
            1,
            "exactly one agent entry should be parsed"
        );
        let agent = match &config.agents[0] {
            AgentConfig::Local(config) => config,
            AgentConfig::SshBacked(_) => panic!("entry with `local = true` should be local"),
        };
        assert_eq!(agent.name.as_deref(), Some("my-local"));
        assert_eq!(agent.home.as_deref(), Some("/var/work"));
        assert_eq!(agent.log.as_deref(), Some("/var/log/my-local.log"));
    }

    /// Verifies that a single agents file can mix ssh and local entries,
    /// parsing each into the correct variant, so an operator can manage
    /// remote hosts and a local agent from the same file.
    #[tokio::test]
    async fn test_parse_config_file_mixed_entries() {
        let (_directory, temp) = test_config_path();
        let content = r#"
[[agents]]
target = "remote-1"

[[agents]]
local = true
name = "local-1"

[[agents]]
target = "remote-2"
name = "web-agent"
"#;
        write_test_config(&temp, content).unwrap();

        let config = parse_config_file(&temp).await.unwrap();

        assert_eq!(config.agents.len(), 3, "all three entries should be parsed");

        let first = match &config.agents[0] {
            AgentConfig::SshBacked(config) => config,
            AgentConfig::Local(_) => panic!("first entry has no `local = true`"),
        };
        assert_eq!(first.target, "remote-1");

        let second = match &config.agents[1] {
            AgentConfig::Local(config) => config,
            AgentConfig::SshBacked(_) => panic!("second entry has `local = true`"),
        };
        assert_eq!(second.name.as_deref(), Some("local-1"));

        let third = match &config.agents[2] {
            AgentConfig::SshBacked(config) => config,
            AgentConfig::Local(_) => panic!("third entry has no `local = true`"),
        };
        assert_eq!(third.target, "remote-2");
        assert_eq!(third.name.as_deref(), Some("web-agent"));
    }

    /// Verifies that an entry with `local = true` AND a `target` is rejected
    /// so the operator gets a clear error instead of a silently misconfigured
    /// agent that the dispatcher would then ignore the `target` for.
    #[tokio::test]
    async fn test_parse_config_file_rejects_local_with_target() {
        let (_directory, temp) = test_config_path();
        write_test_config(
            &temp,
            r#"[[agents]]
local = true
target = "host"
"#,
        )
        .unwrap();

        let result = parse_config_file(&temp).await;

        let error = result.expect_err("local + target should be rejected");
        // The error should mention both fields so the operator immediately
        // sees which fields conflict.
        assert!(
            error.to_string().contains("local") && error.to_string().contains("target"),
            "error should mention both 'local' and 'target': {}",
            error
        );
    }

    /// Verifies that ssh-only fields on a local entry are rejected with a
    /// field-specific error, so an operator who pastes an ssh entry and
    /// just adds `local = true` gets a clear pointer to each mis-placed
    /// field rather than a single vague "config error".
    #[tokio::test]
    async fn test_parse_config_file_rejects_local_with_ssh_fields() {
        for field in ["username", "ssh_port", "remote_bin", "password"] {
            let (_directory, temp) = test_config_path();
            write_test_config(
                &temp,
                format!(
                    r#"[[agents]]
local = true
{field} = "x"
"#
                ),
            )
            .unwrap();

            let result = parse_config_file(&temp).await;

            let error = result.expect_err(&format!("local + {} should be rejected", field));
            assert!(
                error.to_string().contains(field),
                "error should mention '{}': {}",
                field,
                error
            );
        }
    }

    /// Keeps existing managed-agent configs working after the home rename.
    #[tokio::test]
    async fn test_parse_config_file_ssh_entry_with_dir() {
        let (_directory, temp) = test_config_path();
        let content = r#"
[[agents]]
target = "prod-db"
dir = "/var/www/app"
"#;
        write_test_config(&temp, content).unwrap();

        let config = parse_config_file(&temp).await.unwrap();

        assert_eq!(
            config.agents.len(),
            1,
            "exactly one agent entry should be parsed"
        );
        let agent = match &config.agents[0] {
            AgentConfig::SshBacked(config) => config,
            AgentConfig::Local(_) => panic!("entry without `local = true` should be SSH-backed"),
        };
        assert_eq!(
            agent.home.as_deref(),
            Some("/var/www/app"),
            "legacy dir should populate home"
        );
    }

    /// Verifies that a `log` on an ssh entry is accepted and forwarded into
    /// the SshBackedAgentConfig so the operator can capture a remote agent's
    /// forwarded stdout/stderr into a local log file.
    #[tokio::test]
    async fn test_parse_config_file_ssh_entry_with_log() {
        let (_directory, temp) = test_config_path();
        let content = r#"
[[agents]]
target = "prod-db"
log = "log/prod-db.log"
"#;
        write_test_config(&temp, content).unwrap();

        let config = parse_config_file(&temp).await.unwrap();

        assert_eq!(config.agents.len(), 1);
        let agent = match &config.agents[0] {
            AgentConfig::SshBacked(config) => config,
            AgentConfig::Local(_) => panic!("entry without `local = true` should be SSH-backed"),
        };
        assert_eq!(
            agent.log.as_deref(),
            Some("log/prod-db.log"),
            "log should be read from the ssh toml entry"
        );
    }

    /// Stores the SSH password as written so OpenSSH askpass can replay it later.
    #[tokio::test]
    async fn test_parse_config_file_ssh_entry_with_password() {
        let (_directory, temp) = test_config_path();
        let content = r#"
[[agents]]
target = "prod-db"
password = "  keep spaces  "
"#;
        write_test_config(&temp, content).unwrap();

        let config = parse_config_file(&temp).await.unwrap();

        let agent = match &config.agents[0] {
            AgentConfig::SshBacked(config) => config,
            AgentConfig::Local(_) => panic!("entry without `local = true` should be SSH-backed"),
        };
        assert_eq!(
            agent.password.as_deref(),
            Some("  keep spaces  "),
            "password spaces are significant and must not be trimmed"
        );
    }

    /// Verifies agent-only configs may omit `[server]` while still requiring the top-level token.
    #[tokio::test]
    async fn test_parse_config_file_allows_missing_server_section() {
        let (_directory, temp) = test_config_path();
        std::fs::write(
            &temp,
            r#"agent_token = "test-agent-token"

[agent]
server = "http://127.0.0.1:3000"
"#,
        )
        .unwrap();

        let config = parse_config_file(&temp).await.unwrap();

        assert!(
            config.server.is_none(),
            "agent-only hosts should not need a [server] table"
        );
        assert!(
            require_server_section(&config).is_err(),
            "server entry points must still reject agent-only configs"
        );
        assert!(
            standalone_agent_is_fully_configured(&config),
            "[agent] server + token should satisfy bare startup without a name"
        );
    }

    /// Verifies omitting the shared top-level token fails for every process role.
    #[tokio::test]
    async fn test_parse_config_file_rejects_missing_agent_token() {
        let (_directory, temp) = test_config_path();
        std::fs::write(
            &temp,
            r#"[server]
username = "test-user"
password = "test-password"
"#,
        )
        .unwrap();

        let result = parse_config_file(&temp).await;

        assert!(result.is_err(), "top-level agent_token must be required");
    }

    /// Verifies that all three [server] fields are read from the file so
    /// operators can pin the whole server surface from one config file.
    #[tokio::test]
    async fn test_parse_config_file_reads_server_section() {
        let (_directory, temp) = test_config_path();
        let content = r#"
[server]
port = 4000
bind = "127.0.0.1"
log = "/tmp/x"

[[agents]]
target = "host"
"#;
        write_test_config(&temp, content).unwrap();

        let config = parse_config_file(&temp).await.unwrap();

        let server = config.server.expect("[server] should be present");
        assert_eq!(server.port, Some(4000));
        assert_eq!(server.bind.as_deref(), Some("127.0.0.1"));
        assert_eq!(server.log.as_deref(), Some("/tmp/x"));
        assert_eq!(config.agent_token, "test-agent-token");
    }

    /// Verifies that an unknown key in [server] is rejected so a typo is surfaced
    /// at startup instead of silently being ignored.
    #[tokio::test]
    async fn test_parse_config_file_rejects_unknown_server_key() {
        let (_directory, temp) = test_config_path();
        let content = r#"
[server]
post = 3000

[[agents]]
target = "host"
"#;
        write_test_config(&temp, content).unwrap();

        let result = parse_config_file(&temp).await;

        assert!(
            result.is_err(),
            "an unknown [server] key should be rejected"
        );
        assert!(result.unwrap_err().to_string().contains("post"));
    }

    /// Verifies that an out-of-range port in [server] is rejected so the
    /// operator gets a clear error for a typo, mirroring the existing
    /// ssh_port range test.
    #[tokio::test]
    async fn test_parse_config_file_rejects_out_of_range_server_port() {
        let (_directory, temp) = test_config_path();
        let content = r#"
[server]
port = 99999

[[agents]]
target = "host"
"#;
        write_test_config(&temp, content).unwrap();

        let result = parse_config_file(&temp).await;

        assert!(
            result.is_err(),
            "an out-of-range server.port should be rejected"
        );
    }

    /// Verifies first startup creates a minimal, parseable demo config and later startups preserve it.
    #[tokio::test]
    async fn test_create_default_config_if_missing() {
        let directory = crate::test_support::TempDir::create();
        let path = directory.path().join("nested/config.toml");

        let created = create_default_config_if_missing(&path).await.unwrap();
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        let config = parse_config_file(&path).await.unwrap();

        let bootstrap = created.expect("a missing conventional config should be created");
        assert_eq!(
            config.agent_token, bootstrap.agent_token,
            "the generated agent_token should match the one-time printed secret"
        );
        assert!(
            bootstrap.agent_token.len() >= 32,
            "bootstrap agent_token must be high entropy"
        );
        let server = config
            .server
            .as_ref()
            .expect("server bootstrap must write a [server] table");
        assert!(
            config.agent.is_none(),
            "minimal starter config is server-only; standalone [agent] is not required for demo"
        );

        #[cfg(target_os = "linux")]
        {
            // Linux starter configs omit credentials so login uses PAM.
            assert!(
                matches!(server.auth, AuthMode::SystemAccount),
                "Linux default config should omit username/password for PAM login"
            );
            assert!(
                bootstrap.password.is_none(),
                "Linux bootstrap should not print a generated password"
            );
            assert!(
                content.contains("# username =") && content.contains("# password ="),
                "Linux starter config should leave username/password commented"
            );
            assert!(
                content.contains("PAM"),
                "Linux starter config should document PAM login"
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            let AuthMode::Configured { username, password } = &server.auth;
            assert_eq!(
                username, "redoor",
                "macOS demo login should use the fixed redoor username"
            );
            assert_eq!(
                password, "changeme",
                "macOS demo login should use the fixed changeme password"
            );
            assert_eq!(
                bootstrap.password.as_deref(),
                Some("changeme"),
                "bootstrap should report the fixed demo password once"
            );
        }

        assert!(
            server.log.is_none(),
            "starter config should rely on conventional runtime log paths"
        );
        let local_agent = config.agents.iter().find_map(|entry| match entry {
            AgentConfig::Local(config) => Some(config),
            AgentConfig::SshBacked(_) => None,
        });
        let local_agent =
            local_agent.expect("starter config must include a managed local [[agents]] entry");
        assert_eq!(
            local_agent.name.as_deref(),
            Some("local"),
            "demo local agent should use the README name"
        );
        assert!(
            local_agent.log.is_none(),
            "starter managed agents should inherit output unless a log is configured"
        );
        for option in [
            "agent_token =",
            "# port = 7666",
            "# bind = \"0.0.0.0\" # default 127.0.0.1",
            "# cookie_secure = false",
            "local = true",
            "name = \"local\"",
        ] {
            assert!(
                content.contains(option),
                "the starter config should document option {option}"
            );
        }

        let created_again = create_default_config_if_missing(&path).await.unwrap();
        let unchanged = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(
            created_again.is_none(),
            "an existing config must never be overwritten by startup"
        );
        assert_eq!(
            unchanged, content,
            "checking an existing config must leave every byte unchanged"
        );
    }

    /// Verifies nested agent_token under [server] fails with a migration hint.
    #[tokio::test]
    async fn test_parse_config_file_rejects_server_agent_token() {
        let (_directory, path) = test_config_path();
        tokio::fs::write(&path, "[server]\nagent_token = \"legacy-token\"\n")
            .await
            .unwrap();

        let result = parse_config_file(&path).await;

        let error = result
            .expect_err("legacy server.agent_token must be rejected")
            .to_string();
        assert!(
            error.contains("top level"),
            "error should tell operators to move agent_token to the top level: {error}"
        );
    }

    /// Verifies the shared parser reads a complete [agent] table.
    #[tokio::test]
    async fn test_parse_config_file_reads_agent_section() {
        let (_directory, temp) = test_config_path();
        std::fs::write(
            &temp,
            r#"agent_token = "test-agent-token"

[agent]
server = "https://example.com"
name = "edge"
home = "/var/app"
log = "log/agent.log"
"#,
        )
        .unwrap();

        let config = parse_config_file(&temp).await.unwrap();

        let agent = config.agent.expect("[agent] should be present");
        assert_eq!(agent.server.as_deref(), Some("https://example.com"));
        assert_eq!(agent.name.as_deref(), Some("edge"));
        assert_eq!(agent.home.as_deref(), Some("/var/app"));
        assert_eq!(agent.log.as_deref(), Some("log/agent.log"));
    }

    /// Keeps existing agent configs working after the `server` rename.
    #[tokio::test]
    async fn test_parse_config_file_accepts_legacy_ws_address() {
        let (_directory, temp) = test_config_path();
        std::fs::write(
            &temp,
            r#"agent_token = "test-agent-token"

[agent]
ws_address = "wss://example.com/ws"
"#,
        )
        .unwrap();

        let config = parse_config_file(&temp).await.unwrap();

        let agent = config.agent.as_ref().expect("[agent] should be present");
        // Legacy key maps into the same field so resolve/start paths stay single-keyed.
        assert_eq!(agent.server.as_deref(), Some("wss://example.com/ws"));
        assert!(
            standalone_agent_is_fully_configured(&config),
            "legacy ws_address must still satisfy standalone agent setup checks"
        );
    }

    /// Prevents ambiguous configs that set both the new and legacy server keys.
    #[tokio::test]
    async fn test_parse_config_file_rejects_server_and_ws_address() {
        let (_directory, temp) = test_config_path();
        std::fs::write(
            &temp,
            r#"agent_token = "test-agent-token"

[agent]
server = "https://example.com"
ws_address = "wss://example.com/ws"
"#,
        )
        .unwrap();

        let result = parse_config_file(&temp).await;

        let error = result
            .expect_err("both server keys must be rejected")
            .to_string();
        assert!(
            error.contains("server") && error.contains("ws_address"),
            "error should name both keys: {error}"
        );
    }

    /// Verifies Linux accepts a credentials-free [server] table for PAM login.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn test_parse_config_file_allows_missing_credentials_on_linux() {
        let (_directory, temp) = test_config_path();
        std::fs::write(
            &temp,
            r#"agent_token = "test-agent-token"

[server]
"#,
        )
        .unwrap();

        let config = parse_config_file(&temp).await.unwrap();

        let server = config.server.expect("[server] should be present");
        assert!(
            matches!(server.auth, AuthMode::SystemAccount),
            "omitted credentials should select PAM mode"
        );
        assert_eq!(config.agent_token, "test-agent-token");
    }

    /// Verifies a half-specified credential pair is rejected instead of mixed auth modes.
    #[tokio::test]
    async fn test_parse_config_file_rejects_partial_credentials() {
        for content in [
            r#"agent_token = "test-agent-token"

[server]
username = "only-user"
"#,
            r#"agent_token = "test-agent-token"

[server]
password = "only-password"
"#,
        ] {
            let (_directory, temp) = test_config_path();
            std::fs::write(&temp, content).unwrap();
            let result = parse_config_file(&temp).await;
            assert!(
                result.is_err(),
                "username and password must be provided together"
            );
        }
    }

    /// Rejects login names that would escape `users/<username>/` before the server starts.
    #[tokio::test]
    async fn test_parse_config_file_rejects_unsafe_username() {
        let (_directory, temp) = test_config_path();
        std::fs::write(
            &temp,
            r#"agent_token = "test-agent-token"

[server]
username = "alice/../bob"
password = "test-password"
"#,
        )
        .unwrap();
        let result = parse_config_file(&temp).await;
        assert!(
            result.is_err(),
            "a path-escaping server.username must not parse"
        );
        assert!(
            format!("{:#}", result.unwrap_err())
                .contains("server.username may contain only ASCII letters"),
            "operators should see the allowlist rule rather than a later filesystem error"
        );
    }

    /// Reads the complete named-relay schema into the shared SSH transport model.
    #[tokio::test]
    async fn test_parse_config_file_reads_named_relays() {
        let (_directory, temp) = test_config_path();
        std::fs::write(
            &temp,
            r#"agent_token = "test-agent-token"

[[relays]]
id = "production"
target = "user@example.com"
server = "https://redoor.example.com"
username = "deploy"
ssh_port = 2200
name = "production-agent"
agent_app_name = "redoor-production-agent"
remote_bin = "/opt/redoor"
binary_source = "/tmp/redoor"
home = "/srv/app"
log = "/tmp/relay.log"
insecure = true
"#,
        )
        .unwrap();

        let config = parse_config_file(&temp).await.unwrap();
        let relay = require_relay(&config, "production").unwrap();
        // Identity and connection fields prove lifecycle and transport settings stay associated.
        assert_eq!(relay.id, "production");
        assert_eq!(relay.agent.target, "user@example.com");
        assert_eq!(relay.agent.ssh_port, Some(2200));
        assert!(relay.insecure);
        assert_eq!(
            relay.agent_app_name.as_deref(),
            Some("redoor-production-agent")
        );
        assert_eq!(
            relay.binary_source.as_deref(),
            Some(std::path::Path::new("/tmp/redoor"))
        );
    }

    /// Rejects duplicate or path-like IDs before they can alias one runtime file.
    #[tokio::test]
    async fn test_parse_config_file_rejects_unsafe_and_duplicate_relay_ids() {
        for content in [
            r#"agent_token = "test-agent-token"
[[relays]]
id = "../production"
target = "example.com"
server = "http://redoor.example.com"
"#,
            r#"agent_token = "test-agent-token"
[[relays]]
id = "production"
target = "one.example.com"
server = "http://redoor.example.com"
[[relays]]
id = "production"
target = "two.example.com"
server = "http://redoor.example.com"
"#,
        ] {
            let (_directory, temp) = test_config_path();
            std::fs::write(&temp, content).unwrap();
            let result = parse_config_file(&temp).await;
            // Invalid identities must fail before any process or PID file is created.
            assert!(result.is_err());
        }
    }

    /// Serde rejects misspelled managed-agent keys with their line instead of ignoring them.
    #[test]
    fn unknown_managed_agent_key_points_at_the_line() {
        let error = parse_config_str(
            Path::new("/etc/redoor/config.toml"),
            r#"agent_token = "t"
[[agents]]
target = "host"
remotebin = "/opt/redoor"
"#,
        )
        .expect_err("a misspelled key must not be silently ignored");
        let message = error.to_string();
        // The file path tells operators which config was rejected.
        assert!(message.contains("/etc/redoor/config.toml"), "{message}");
        // The offending key and its location make the typo actionable.
        assert!(
            message.contains("remotebin") && message.contains("line 4"),
            "{message}"
        );
    }

    /// Two rows that would register under one name fail at parse time, before any supervisor.
    #[test]
    fn duplicate_managed_agent_names_are_rejected() {
        let error = parse_config_str(
            Path::new("config.toml"),
            r#"agent_token = "t"
[[agents]]
target = "deploy@web"
[[agents]]
target = "web"
"#,
        )
        .expect_err("both rows default to the name 'web'");
        // The message names the colliding identity and how to resolve it.
        assert!(
            error
                .to_string()
                .contains("duplicate managed agent name 'web'"),
            "{error}"
        );
    }

    /// Enum-like settings keep the key path and the accepted vocabulary in the error.
    #[test]
    fn invalid_log_level_names_key_and_choices() {
        let error = parse_config_str(
            Path::new("config.toml"),
            r#"agent_token = "t"
[agent]
log_level = "loud"
"#,
        )
        .expect_err("unknown levels must be rejected");
        // Operators need the file, the key path and the valid values to fix it.
        assert_eq!(
            error.to_string(),
            "Invalid config file 'config.toml': agent.log_level: logging level must be one of: trace, debug, info, warning, error"
        );
    }
}
