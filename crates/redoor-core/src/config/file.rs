//! Serde model of the TOML file plus the semantic checks serde cannot express.
//!
//! The raw structs mirror the file one-to-one so `deny_unknown_fields` surfaces typos
//! with the offending line, and type/range errors come from serde with the exact
//! location. Everything that depends on more than one key (credential pairs,
//! local-vs-SSH entries, duplicate identities, platform support) lives in
//! the `validate` methods, which produce the public, already-checked config types.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::Deserialize;

use super::{
    AgentConfig, AgentSection, AuthMode, LocalAgentConfig, RedoorConfig, RelayConfig,
    ServerSection, SshBackedAgentConfig, default_agent_name, default_local_agent_name,
    parse_relay_id, parse_server_username,
};

/// Whole-document shape; every section is optional because roles validate their own needs.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawConfig {
    /// Shared registration secret; required, but checked in `validate` for a clearer message.
    agent_token: Option<String>,
    server: Option<RawServerSection>,
    agent: Option<RawAgentSection>,
    #[serde(default)]
    agents: Vec<RawAgentEntry>,
    #[serde(default)]
    relays: Vec<RawRelay>,
}

/// `[server]` as written; credentials stay optional until the pair rule is applied.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServerSection {
    port: Option<u16>,
    bind: Option<String>,
    log: Option<String>,
    log_level: Option<String>,
    log_format: Option<String>,
    username: Option<String>,
    password: Option<String>,
    #[serde(default)]
    cookie_secure: bool,
    #[serde(default)]
    mcp_allowed_origins: Vec<String>,
}

/// `[agent]` as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgentSection {
    server: Option<String>,
    name: Option<String>,
    home: Option<String>,
    log: Option<String>,
    log_level: Option<String>,
    log_format: Option<String>,
}

/// One `[[agents]]` row; the union of SSH and local keys because `local` selects the kind.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgentEntry {
    #[serde(default)]
    local: bool,
    target: Option<String>,
    username: Option<String>,
    ssh_port: Option<u16>,
    name: Option<String>,
    remote_bin: Option<String>,
    home: Option<String>,
    log: Option<String>,
    password: Option<String>,
}

/// One `[[relays]]` row.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRelay {
    id: Option<String>,
    target: Option<String>,
    server: Option<String>,
    username: Option<String>,
    ssh_port: Option<u16>,
    name: Option<String>,
    remote_bin: Option<String>,
    binary_source: Option<String>,
    home: Option<String>,
    log: Option<String>,
    #[serde(default)]
    insecure: bool,
    agent_app_name: Option<String>,
    password: Option<String>,
}

/// A violated cross-key rule; the text names the key path and how to fix it.
///
/// Kept private and path-less: `parse_config_str` attaches the file path when it turns
/// this into the public [`super::Invalid`].
pub(super) struct Invalid(pub(super) String);

/// Shorthand for building a rule violation from any message.
fn invalid(message: impl Into<String>) -> Invalid {
    Invalid(message.into())
}

impl RawConfig {
    /// Applies cross-key rules and converts into the public config model.
    pub(super) fn validate(self) -> Result<RedoorConfig, Invalid> {
        let agent_token = self
            .agent_token
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                invalid(
                    "agent_token must be a non-empty string at the top level of the config file",
                )
            })?;
        let server = self.server.map(RawServerSection::validate).transpose()?;
        let agent = self.agent.map(RawAgentSection::validate).transpose()?;
        let agents = self
            .agents
            .into_iter()
            .enumerate()
            .map(|(index, entry)| entry.validate(index))
            .collect::<Result<Vec<_>, _>>()?;
        let mut relays: Vec<RelayConfig> = Vec::with_capacity(self.relays.len());
        for (index, relay) in self.relays.into_iter().enumerate() {
            let relay = relay.validate(index)?;
            if relays.iter().any(|existing| existing.id == relay.id) {
                return Err(invalid(format!("duplicate relay ID '{}'", relay.id)));
            }
            relays.push(relay);
        }
        Ok(RedoorConfig {
            agent_token,
            server,
            agent,
            agents,
            relays,
        })
    }
}

/// Rejects two managed rows that would register under one name, before any supervisor exists.
///
/// Not part of `validate`: only the server registers managed agents, so other roles reading
/// the same file (logs, relays, service install) must not fail on it.
pub(super) fn reject_duplicate_agent_names(agents: &[AgentConfig]) -> Result<(), Invalid> {
    let mut names = HashSet::new();
    for agent in agents {
        let name = match agent {
            AgentConfig::Local(local) => {
                local.name.clone().unwrap_or_else(default_local_agent_name)
            }
            AgentConfig::SshBacked(ssh) => ssh
                .name
                .clone()
                .unwrap_or_else(|| default_agent_name(&ssh.target)),
        };
        if !names.insert(name.clone()) {
            return Err(invalid(format!(
                "duplicate managed agent name '{name}' in [[agents]]; set a distinct 'name' on one of the entries"
            )));
        }
    }
    Ok(())
}

impl RawServerSection {
    /// Resolves the login backend and checks values serde only sees as plain strings.
    fn validate(self) -> Result<ServerSection, Invalid> {
        let log_level = parse_named(self.log_level, "server.log_level")?;
        let log_format = parse_named(self.log_format, "server.log_format")?;
        let username = self
            .username
            .map(|value| parse_server_username(&value).map_err(invalid))
            .transpose()?;
        if self.password.as_deref() == Some("") {
            return Err(invalid(
                "server.password must be a non-empty string when set",
            ));
        }
        // Requiring the pair keeps a half-configured file from silently falling back to
        // system auth while still embedding one of the secrets.
        let auth = match (username, self.password) {
            (Some(username), Some(password)) => AuthMode::Configured { username, password },
            (Some(_), None) => {
                return Err(invalid(
                    "server.password is required when server.username is set",
                ));
            }
            (None, Some(_)) => {
                return Err(invalid(
                    "server.username is required when server.password is set",
                ));
            }
            (None, None) => system_account_auth()?,
        };
        if let Some(origin) = self
            .mcp_allowed_origins
            .iter()
            .find(|origin| !origin.starts_with("http://") && !origin.starts_with("https://"))
        {
            return Err(invalid(format!(
                "server.mcp_allowed_origins entries must be absolute HTTP(S) origins; got '{origin}'"
            )));
        }
        Ok(ServerSection {
            port: self.port,
            bind: self.bind,
            log: self.log,
            log_level,
            log_format,
            auth,
            cookie_secure: self.cookie_secure,
            mcp_allowed_origins: self.mcp_allowed_origins,
        })
    }
}

/// Selects PAM login where it exists; other platforms need explicit TOML credentials.
fn system_account_auth() -> Result<AuthMode, Invalid> {
    #[cfg(target_os = "linux")]
    {
        Ok(AuthMode::SystemAccount)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(invalid(
            "server.username and server.password are required on this platform; system-account (PAM) login is only supported on Linux",
        ))
    }
}

impl RawAgentSection {
    /// Rejects empty overrides and parses the enum-like settings.
    fn validate(self) -> Result<AgentSection, Invalid> {
        let non_empty = |value: Option<String>, key: &str| match value {
            Some(value) if value.is_empty() => Err(invalid(format!(
                "agent.{key} must be a non-empty string when set"
            ))),
            other => Ok(other),
        };
        Ok(AgentSection {
            server: non_empty(self.server, "server")?,
            name: non_empty(self.name, "name")?,
            home: non_empty(self.home, "home")?,
            log: non_empty(self.log, "log")?,
            log_level: parse_named(non_empty(self.log_level, "log_level")?, "agent.log_level")?,
            log_format: parse_named(
                non_empty(self.log_format, "log_format")?,
                "agent.log_format",
            )?,
        })
    }
}

impl RawAgentEntry {
    /// Splits the row into the SSH or local model selected by `local`.
    fn validate(mut self, index: usize) -> Result<AgentConfig, Invalid> {
        let home = self.home.take();
        if self.local {
            self.validate_local(index, home)
        } else {
            self.validate_ssh(index, home)
        }
    }

    /// Rejects SSH-only keys so a pasted SSH row with `local = true` fails clearly instead of
    /// starting an agent that silently ignores them.
    fn validate_local(self, index: usize, home: Option<String>) -> Result<AgentConfig, Invalid> {
        if self.target.is_some() {
            return Err(invalid(format!(
                "agents entry #{index} has both 'local = true' and a 'target'; local agents do not use ssh, remove 'target'"
            )));
        }
        let ssh_only = [
            ("username", self.username.is_some()),
            ("ssh_port", self.ssh_port.is_some()),
            ("remote_bin", self.remote_bin.is_some()),
            ("password", self.password.is_some()),
        ];
        if let Some((key, _)) = ssh_only.into_iter().find(|(_, present)| *present) {
            return Err(invalid(format!(
                "agents entry #{index} has '{key}' which only applies to SSH-backed agents (local = true); remove '{key}'"
            )));
        }
        Ok(AgentConfig::Local(LocalAgentConfig {
            name: self.name,
            home,
            log: self.log,
        }))
    }

    /// Requires the SSH destination; every other field defers to OpenSSH or agent defaults.
    fn validate_ssh(self, index: usize, home: Option<String>) -> Result<AgentConfig, Invalid> {
        let target = self.target.ok_or_else(|| {
            invalid(format!(
                "agents entry #{index} is missing a 'target' string"
            ))
        })?;
        if self.password.as_deref() == Some("") {
            return Err(invalid(format!(
                "agents entry #{index} 'password' must be non-empty when set"
            )));
        }
        Ok(AgentConfig::SshBacked(SshBackedAgentConfig {
            username: self.username,
            ssh_port: self.ssh_port,
            name: self.name,
            remote_bin: self.remote_bin,
            home,
            target,
            log: self.log,
            password: self.password,
        }))
    }
}

impl RawRelay {
    /// Checks identity, URL and TLS settings before any PID file or SSH session exists.
    fn validate(self, index: usize) -> Result<RelayConfig, Invalid> {
        let non_blank = |value: Option<String>, key: &str| match value {
            Some(value) if value.trim().is_empty() => Err(invalid(format!(
                "relays entry #{index} '{key}' must be non-empty"
            ))),
            other => Ok(other),
        };
        let required = |value: Option<String>, key: &str| {
            non_blank(value, key)?.ok_or_else(|| {
                invalid(format!("relays entry #{index} is missing a '{key}' string"))
            })
        };
        let id = parse_relay_id(&required(self.id, "id")?).map_err(invalid)?;
        let target = required(self.target, "target")?;
        let server = required(self.server, "server")?;
        let server_address = server
            .parse::<crate::server_address::ServerAddress>()
            .map_err(|error| invalid(format!("invalid server in relay '{id}': {error}")))?;
        if self.insecure && !server_address.is_secure() {
            return Err(invalid(format!(
                "relay '{id}' insecure = true requires an https:// or wss:// server URL"
            )));
        }
        let agent_app_name = non_blank(self.agent_app_name, "agent_app_name")?
            .map(|value| crate::app_name::parse_app_name(&value).map_err(invalid))
            .transpose()?;
        Ok(RelayConfig {
            id,
            server,
            insecure: self.insecure,
            binary_source: non_blank(self.binary_source, "binary_source")?.map(PathBuf::from),
            agent_app_name,
            agent: SshBackedAgentConfig {
                username: non_blank(self.username, "username")?,
                ssh_port: self.ssh_port,
                name: non_blank(self.name, "name")?,
                remote_bin: non_blank(self.remote_bin, "remote_bin")?,
                home: non_blank(self.home, "home")?,
                target,
                log: non_blank(self.log, "log")?,
                password: non_blank(self.password, "password")?,
            },
        })
    }
}

/// Parses an enum-like string setting with the type's own vocabulary and the key path.
fn parse_named<T>(value: Option<String>, key: &str) -> Result<Option<T>, Invalid>
where
    T: std::str::FromStr<Err = String>,
{
    value
        .map(|value| {
            value
                .parse::<T>()
                .map_err(|error| invalid(format!("{key}: {error}")))
        })
        .transpose()
}
