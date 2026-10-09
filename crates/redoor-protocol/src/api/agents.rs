//! Agent inventory, lifecycle, configuration, and host-detail bodies.

use crate::identity::BinaryIdentity;
use crate::ids::{AgentId, SocketId, UnixTimestampMillis, UnixTimestampSeconds};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Selects which server-side executable should replace a connected agent.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "source", rename_all = "snake_case")]
#[ts(tag = "source", rename_all = "snake_case")]
pub enum UpgradeAgentRequest {
    /// Downloads or reuses a published release for the agent platform.
    PublishedRelease { target_version: String },
    /// Force-installs the exact executable currently running the server.
    RunningServer,
}

/// Confirms the agent acknowledged execution of the newly installed target version.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpgradeAgentResponse {
    pub upgrading: bool,
    pub target_version: String,
}

/// Confirms that the agent's graphical desktop accepted a path-open request.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct OpenPathResponse {
    /// The absolute path passed to the platform's native launcher.
    pub path: String,
}

/// Returns the complete in-process inventory rather than only live sockets.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentListResponse {
    pub agents: Vec<AgentInfoResponse>,
}

/// Describes one SSH-backed managed agent to persist and register at runtime.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateSshAgentRequest {
    /// SSH host, alias, or destination accepted by the local OpenSSH client.
    pub target: String,
    /// Optional explicit SSH username instead of the target or SSH config default.
    pub username: Option<String>,
    /// Optional explicit SSH port instead of the SSH config default.
    pub ssh_port: Option<u16>,
    /// Stable managed-agent name; defaults to the target hostname when omitted.
    pub name: Option<String>,
    /// Optional path where the remote Redoor binary is installed.
    pub remote_bin: Option<String>,
    /// Optional initial browser directory advertised before the first connection.
    pub home: Option<String>,
    /// Optional local path receiving SSH subprocess diagnostics.
    pub log: Option<String>,
    /// New password to persist; omit or send empty on update to keep the existing secret.
    pub password: Option<String>,
    /// When true, drop any stored password so key or ssh-agent auth is used.
    /// Separate from an omitted password because that still means "keep" on update.
    pub clear_password: Option<bool>,
}

/// Describes replacement SSH settings separately from the create contract.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateSshAgentRequest {
    pub target: String,
    pub username: Option<String>,
    pub ssh_port: Option<u16>,
    pub name: Option<String>,
    pub remote_bin: Option<String>,
    pub home: Option<String>,
    pub log: Option<String>,
    pub password: Option<String>,
    /// Distinguishes keeping an omitted password from deleting the stored secret.
    pub clear_password: Option<bool>,
}

/// Returns the newly visible dormant inventory record after persistence succeeds.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateSshAgentResponse {
    pub agent: AgentInfoResponse,
}

/// Returns the editable TOML fields for one SSH-backed managed agent.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ManagedSshAgentConfigurationResponse {
    /// SSH host, alias, or destination accepted by the local OpenSSH client.
    pub target: String,
    /// Optional explicit SSH username instead of the target or SSH config default.
    pub username: Option<String>,
    /// Optional explicit SSH port instead of the SSH config default.
    pub ssh_port: Option<u16>,
    /// Stable managed-agent name; defaults to the target hostname when omitted.
    pub name: Option<String>,
    /// Optional path where the remote Redoor binary is installed.
    pub remote_bin: Option<String>,
    /// Optional initial browser directory advertised before the first connection.
    pub home: Option<String>,
    /// Optional local path receiving SSH subprocess diagnostics.
    pub log: Option<String>,
    /// Always null on GET so the stored SSH password is never sent to the browser.
    pub password: Option<String>,
    /// Reports whether a password is stored so the edit form can choose the matching auth radio.
    pub has_password: bool,
}

/// Returns the replacement dormant inventory record after an SSH configuration edit.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateSshAgentResponse {
    pub agent: AgentInfoResponse,
}

/// Describes one local managed agent to persist and register at runtime.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateLocalAgentRequest {
    /// Stable managed-agent name; defaults to the server hostname when omitted.
    pub name: Option<String>,
    /// Optional initial browser directory advertised before the first connection.
    pub home: Option<String>,
    /// Optional local path receiving the spawned agent process diagnostics.
    pub log: Option<String>,
}

/// Describes replacement local-agent settings separately from the create contract.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateLocalAgentRequest {
    pub name: Option<String>,
    pub home: Option<String>,
    pub log: Option<String>,
}

/// Returns the newly visible dormant inventory record after local persistence succeeds.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateLocalAgentResponse {
    pub agent: AgentInfoResponse,
}

/// Returns the editable TOML fields for one local managed agent.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ManagedLocalAgentConfigurationResponse {
    /// Stable managed-agent name; defaults to the server hostname when omitted.
    pub name: Option<String>,
    /// Optional initial browser directory advertised before the first connection.
    pub home: Option<String>,
    /// Optional local path receiving the spawned agent process diagnostics.
    pub log: Option<String>,
}

/// Returns the replacement dormant inventory record after a local configuration edit.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateLocalAgentResponse {
    pub agent: AgentInfoResponse,
}

/// Confirms that a managed entry and its dormant runtime registration were removed.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DeleteManagedAgentResponse {
    pub deleted: bool,
}

/// Identifies the user-visible connection lifecycle independently from ownership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum AgentConnectionStatus {
    Stopped,
    Starting,
    Connected,
    Disconnected,
}

/// One sticky provisioning line so the starting UI can show SSH progress without a log stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProvisioningStatusMessage {
    /// Operator-facing step text accumulated for the current start attempt.
    pub message: String,
    /// Server-side millisecond stamp so elapsed labels can show secs + ms.
    pub at: UnixTimestampMillis,
}

/// Summarizes one known agent without requiring a current WebSocket connection.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentInfoResponse {
    pub id: AgentId,
    pub name: String,
    pub cwd: Option<String>,
    pub managed: bool,
    /// Whether this managed TOML entry can be edited or deleted from the UI.
    pub configuration_editable: bool,
    /// Configured SSH destination so list views can label remotes without a second request.
    pub ssh_target: Option<String>,
    pub status: AgentConnectionStatus,
    pub connected_at: Option<UnixTimestampSeconds>,
    /// Current WebSocket generation; changes whenever this agent reconnects.
    pub connection_id: Option<SocketId>,
    pub last_seen_at: Option<UnixTimestampSeconds>,
    pub connection_issue: Option<String>,
    /// Ordered SSH start steps for the current attempt; empty for local or dormant agents.
    pub provisioning_status: Vec<ProvisioningStatusMessage>,
    /// Binary identity from the latest registration; absent until first connect.
    pub binary: Option<BinaryIdentity>,
    /// Whether the latest agent session can safely complete an in-place upgrade.
    pub supports_self_exec: bool,
    /// Whether the latest agent session can launch paths in a graphical desktop.
    pub supports_native_open: bool,
    /// Whether the latest agent session can move entries to its platform trash.
    pub supports_move_to_trash: bool,
    /// Whether the latest agent session can also list and restore trash entries.
    pub supports_trash: bool,
    /// Effective UID from the latest registration so chmod gating does not need a details round-trip.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub uid: Option<u32>,
    /// Whether the latest session's effective UID is root. False until first connect or for older agents.
    #[serde(default)]
    pub is_root: bool,
}

/// Confirms that a managed supervisor accepted an idempotent start request.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StartAgentResponse {
    pub agent: AgentInfoResponse,
}

/// Confirms that the previous managed startup attempt was canceled before its replacement began.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RetryAgentStartResponse {
    pub agent: AgentInfoResponse,
}

/// Confirms that intentional shutdown completed child cleanup.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ShutdownAgentResponse {
    pub agent: AgentInfoResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EchoRequest {
    pub message: String,
    #[serde(default)]
    pub random_sleep: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EchoResponse {
    pub message: String,
}

/// Connected-agent detail view including the same binary identity as the server home.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentDetailsResponse {
    pub id: AgentId,
    pub name: String,
    pub pid: u32,
    pub cwd: String,
    /// Absolute path of the TOML config file this agent loaded, or empty when none.
    pub config_path: String,
    /// Absolute path of the running agent binary.
    pub exe_path: String,
    pub load_average_one: f64,
    pub load_average_five: f64,
    pub load_average_fifteen: f64,
    #[ts(type = "number")]
    pub system_uptime: u64,
    pub os: String,
    pub arch: String,
    pub hostname: String,
    /// Primary non-loopback IP selected from the agent's local routing table.
    pub external_ip: Option<String>,
    pub username: String,
    pub connected_at: UnixTimestampSeconds,
    /// Compile-time identity of the agent binary currently serving this connection.
    pub binary: BinaryIdentity,
    /// Current filesystems are optional on the wire so newer servers can still inspect older agents.
    #[serde(default)]
    pub mount_points: Vec<MountPoint>,
    /// Effective UID of the connected agent process; defaults for older agents that omit it.
    #[serde(default)]
    pub uid: u32,
    /// Whether the connected agent process is running as root; defaults false for older agents.
    #[serde(default)]
    pub is_root: bool,
}

/// Describes one mounted filesystem for capacity checks and direct browser navigation.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MountPoint {
    /// Absolute path where the filesystem is mounted.
    pub path: String,
    /// Bytes available to the user running the agent, when reported by the platform.
    #[ts(type = "number | null")]
    pub available_bytes: Option<u64>,
    /// Total filesystem capacity in bytes, when reported by the platform.
    #[ts(type = "number | null")]
    pub total_bytes: Option<u64>,
    /// Filesystem format such as ext4, tmpfs, APFS, or NTFS.
    pub mount_type: Option<String>,
}

impl MountPoint {
    /// Hides pseudo-filesystems that add noise without representing operator-managed storage.
    pub fn is_visible(&self) -> bool {
        !matches!(
            self.mount_type.as_deref(),
            Some(
                "devpts"
                    | "devtmpfs"
                    | "proc"
                    | "fuse.lxcfs"
                    | "sysfs"
                    | "efivarfs"
                    | "cgroup2"
                    | "fusectl"
                    | "pstore"
                    | "debugfs"
                    | "securityfs"
                    | "tmpfs"
                    | "mqueue"
                    | "binfmt_misc"
            )
        )
    }
}

/// One NSS user that a root details view can select as an owner.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentAccountUser {
    pub name: String,
    pub uid: u32,
}

/// One NSS group that a root details view can select as a group.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentAccountGroup {
    pub name: String,
    pub gid: u32,
}

/// Host account catalog fetched on demand so connect metadata stays small.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentAccountsResponse {
    pub users: Vec<AgentAccountUser>,
    pub groups: Vec<AgentAccountGroup>,
    /// True when NSS returned more accounts than the agent is willing to serialize.
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keeps the shared agent, server, and UI exclusion contract exact.
    #[test]
    fn test_mount_point_visibility() {
        for mount_type in [
            "devpts",
            "devtmpfs",
            "proc",
            "fuse.lxcfs",
            "sysfs",
            "efivarfs",
            "cgroup2",
            "fusectl",
            "pstore",
            "debugfs",
            "securityfs",
            "tmpfs",
            "mqueue",
            "binfmt_misc",
        ] {
            let mount = MountPoint {
                path: "/ignored".to_string(),
                available_bytes: Some(0),
                total_bytes: Some(0),
                mount_type: Some(mount_type.to_string()),
            };
            assert!(
                !mount.is_visible(),
                "{mount_type} should be excluded from mount inventory"
            );
        }

        let storage_mount = MountPoint {
            path: "/".to_string(),
            available_bytes: Some(1),
            total_bytes: Some(2),
            mount_type: Some("ext4".to_string()),
        };
        assert!(
            storage_mount.is_visible(),
            "storage filesystems should remain visible"
        );
    }
}
