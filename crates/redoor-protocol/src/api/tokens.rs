//! Access token permissions, device scopes and the token management bodies.
//!
//! One permission vocabulary is shared by the server (authorization), the remote CLI (login
//! prompt) and the UI (generated bindings), so a new permission cannot be enforced by one
//! side and unknown to another.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::AgentId;

/// One grantable capability on a device.
///
/// Token management, server and agent configuration are deliberately not permissions: they
/// stay browser-session-only so a token can never escalate itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum Permission {
    /// List, stat, read, download and search files.
    #[serde(rename = "files:read")]
    FilesRead,
    /// Upload, create, overwrite and copy into files.
    #[serde(rename = "files:write")]
    FilesWrite,
    /// Trash or permanently delete files, and remove move sources.
    #[serde(rename = "files:delete")]
    FilesDelete,
    /// Run arbitrary commands as the agent's OS user, which bypasses every file permission.
    #[serde(rename = "commands:execute")]
    CommandsExecute,
    /// Start, stop, restart and upgrade managed agents.
    #[serde(rename = "agents:manage")]
    AgentsManage,
}

impl Permission {
    /// Every permission in display order, so prompts and "all" expand from one list.
    pub const ALL: [Permission; 5] = [
        Permission::FilesRead,
        Permission::FilesWrite,
        Permission::FilesDelete,
        Permission::CommandsExecute,
        Permission::AgentsManage,
    ];

    /// The wire name, also used for CLI flags and error messages so users see one spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::FilesRead => "files:read",
            Permission::FilesWrite => "files:write",
            Permission::FilesDelete => "files:delete",
            Permission::CommandsExecute => "commands:execute",
            Permission::AgentsManage => "agents:manage",
        }
    }

    /// One-line human description shown by the CLI login prompt, so users know what they grant.
    pub fn summary(self) -> &'static str {
        match self {
            Permission::FilesRead => "List, stat, read, search and download files",
            Permission::FilesWrite => "Upload, create, overwrite and copy into files",
            Permission::FilesDelete => "Trash and delete files, move sources",
            Permission::CommandsExecute => "Run commands as the agent's OS user (high trust)",
            Permission::AgentsManage => "Start/stop managed agents, used by auto-start",
        }
    }
}

impl fmt::Display for Permission {
    /// Prints the wire name so messages match what users type in flags and see in the UI.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Permission {
    type Err = String;

    /// Rejects unknown names instead of silently granting nothing.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Permission::ALL
            .into_iter()
            .find(|permission| permission.as_str() == value)
            .ok_or_else(|| {
                let known = Permission::ALL.map(Permission::as_str).join(", ");
                format!("unknown permission '{value}' (expected one of: {known})")
            })
    }
}

/// Which devices a token may touch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeviceScope {
    /// Every device, including devices added later; the CLI default.
    All,
    /// Only these device ids, for least-privilege clients. Must be non-empty.
    Only { device_ids: Vec<AgentId> },
}

impl DeviceScope {
    /// Whether this scope includes a device, kept here so every caller shares one rule.
    pub fn contains(&self, device: &AgentId) -> bool {
        match self {
            DeviceScope::All => true,
            DeviceScope::Only { device_ids } => device_ids.contains(device),
        }
    }
}

/// Secret-free token metadata, safe to cache and show in lists.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AccessTokenResponse {
    pub id: String,
    pub name: String,
    pub permissions: Vec<Permission>,
    pub devices: DeviceScope,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339; null means the token never expires.
    pub expires_at: Option<String>,
    /// RFC 3339; may lag by up to a minute because last use is persisted lazily.
    pub last_used_at: Option<String>,
}

/// List envelope so the response can grow without breaking clients.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AccessTokenListResponse {
    pub tokens: Vec<AccessTokenResponse>,
}

/// Input for minting a token; the secret is generated by the server.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateAccessTokenRequest {
    pub name: String,
    pub permissions: Vec<Permission>,
    pub devices: DeviceScope,
    /// Optional RFC 3339 timestamp; null means no automatic expiry.
    pub expires_at: Option<String>,
}

/// Replaces the editable policy without changing the secret or the expiry.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateAccessTokenRequest {
    pub name: String,
    pub permissions: Vec<Permission>,
    pub devices: DeviceScope,
}

/// Creation or rotation result; the only time the secret ever leaves the server.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AccessTokenSecretResponse {
    pub token: String,
    pub metadata: AccessTokenResponse,
}

/// Confirms deletion without echoing credential material.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DeleteAccessTokenResponse {
    pub deleted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire names are part of the REST contract, CLI flags and stored tokens.
    #[test]
    fn permissions_round_trip_through_wire_names() {
        for permission in Permission::ALL {
            let json = serde_json::to_string(&permission).unwrap();
            // Serde and as_str must agree, or stored tokens and flags would disagree.
            assert_eq!(json, format!("\"{}\"", permission.as_str()));
            // FromStr accepts exactly what Display prints.
            assert_eq!(
                permission.to_string().parse::<Permission>().unwrap(),
                permission
            );
        }
        // Unknown names are rejected rather than ignored.
        assert!("devices:read".parse::<Permission>().is_err());
        assert!(serde_json::from_str::<Permission>("\"devices:read\"").is_err());
    }

    /// The tagged device scope shape is what the UI and CLI send.
    #[test]
    fn device_scope_uses_tagged_json() {
        // "All" carries no payload, so it never needs updating when devices are added.
        assert_eq!(
            serde_json::to_value(DeviceScope::All).unwrap(),
            serde_json::json!({"kind": "all"})
        );
        let a = AgentId::generate();
        let b = AgentId::generate();
        let only = DeviceScope::Only {
            device_ids: vec![a.clone()],
        };
        let json = serde_json::to_value(&only).unwrap();
        // Explicit ids are listed under the "only" kind.
        assert_eq!(
            json,
            serde_json::json!({"kind": "only", "device_ids": [a.as_str()]})
        );
        assert_eq!(serde_json::from_value::<DeviceScope>(json).unwrap(), only);
        // Membership follows the scope kind.
        assert!(only.contains(&a));
        assert!(!only.contains(&b));
        assert!(DeviceScope::All.contains(&b));
    }
}
