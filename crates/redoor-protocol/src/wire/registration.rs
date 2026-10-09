//! What an agent announces about itself when its control session registers.
//!
//! The same two values travel from the wire (`agent_register`, flattened so the JSON keeps its
//! flat field names) through the router's live connection and retained inventory to the REST
//! projection, instead of a dozen loose fields copied between four structs.

use crate::identity::BinaryIdentity;
use serde::{Deserialize, Serialize};

/// Who the agent process is: platform, account, start directory and binary.
///
/// Fixed for one control session; a reconnect may report a different identity (an upgraded
/// binary, a different account), which replaces the previous one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentIdentity {
    /// Operating system reported by the agent.
    pub os: String,
    /// CPU architecture reported by the agent.
    pub arch: String,
    /// Hostname reported by the agent.
    pub hostname: String,
    /// Account the agent process runs as.
    pub username: String,
    /// Immutable absolute directory opened when the UI selects this agent.
    #[serde(rename = "cwd")]
    pub default_directory: String,
    /// Compile-time identity of the agent binary.
    pub binary: BinaryIdentity,
    /// Effective UID captured at connect so chmod gating does not depend on `$USER`.
    #[serde(default)]
    pub uid: Option<u32>,
    /// Derived from the effective UID, never from the username, so systemd `User=` is correct.
    #[serde(default)]
    pub is_root: bool,
}

/// Which optional protocol features the agent implements.
///
/// Every flag defaults to `false` on the wire so older agents and test fixtures that omit a
/// flag are never sent commands they do not understand.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCapabilities {
    /// Understands the replacement-path self-exec command used by upgrades.
    #[serde(default)]
    pub supports_self_exec: bool,
    /// Can launch filesystem paths in a graphical desktop.
    #[serde(default)]
    pub supports_native_open: bool,
    /// Implements the trash inventory and restore commands.
    #[serde(default)]
    pub supports_trash: bool,
    /// Can move filesystem entries to its platform trash.
    #[serde(default)]
    pub supports_move_to_trash: bool,
    /// Implements per-stream credit in both transfer directions.
    #[serde(default)]
    pub supports_stream_credit: bool,
}

impl AgentCapabilities {
    /// Applies implications older agents left implicit: an agent with full trash inventory
    /// support predates the separate move flag but can always move entries to trash.
    pub fn normalized(self) -> Self {
        Self {
            supports_move_to_trash: self.supports_move_to_trash || self.supports_trash,
            ..self
        }
    }
}
