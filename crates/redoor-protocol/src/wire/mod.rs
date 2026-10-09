//! Agent <-> server websocket protocol: the control envelope and its framing helpers.

pub mod command;
pub mod exec;
pub mod log;
pub mod stream;
pub mod terminal;
pub mod transfer;

use crate::identity::BinaryIdentity;
use crate::ids::{AgentId, RequestId};
use command::{Command, CommandResult};
use serde::{Deserialize, Serialize};

/// Control-socket envelope exchanged between the server and its agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Message {
    /// Sent by a newly connected agent to bind the websocket session to its
    /// stable agent id and publish metadata used by the router, UI, and agent
    /// detail responses.
    #[serde(rename = "agent_register")]
    AgentRegister {
        agent_id: AgentId,
        agent_name: String,
        os: String,
        arch: String,
        hostname: String,
        username: String,
        /// Immutable absolute directory opened when the UI selects this agent.
        cwd: String,
        /// Shared secret proving this agent was started with server credentials.
        token: String,
        /// Compile-time identity of the connecting agent binary.
        binary: BinaryIdentity,
        /// Whether this agent understands the replacement-path self-exec command.
        #[serde(default)]
        supports_self_exec: bool,
        /// Whether this agent can launch filesystem paths in a graphical desktop.
        #[serde(default)]
        supports_native_open: bool,
        /// Whether this agent implements the platform-neutral trash command contract.
        #[serde(default)]
        supports_trash: bool,
        /// Whether this agent can move filesystem entries to its platform trash.
        #[serde(default)]
        supports_move_to_trash: bool,
        /// Effective UID captured at connect so chmod gating does not depend on `$USER`.
        #[serde(default)]
        uid: Option<u32>,
        /// Root is derived from `Uid::effective()`, never from username, so systemd `User=` is correct.
        #[serde(default)]
        is_root: bool,
    },

    /// Instructs the authoritative control session to establish its persistent
    /// payload socket with a secret that stale sessions cannot reuse.
    #[serde(rename = "transfer_socket_open")]
    TransferSocketOpen { token: String },

    /// Optional explicit teardown signal from an agent asking the router to
    /// remove it from the active registry; websocket session shutdown also
    /// triggers the same cleanup automatically.
    #[serde(rename = "agent_unregister")]
    AgentUnregister { agent_id: AgentId },

    /// Sent by the router to start a request on an agent.
    ///
    /// The `request_id` becomes the correlation key for the request's terminal
    /// `CommandResponse` and, for streaming commands, any binary stream frames.
    #[serde(rename = "command")]
    Command {
        agent_id: AgentId,
        request_id: RequestId,
        command: Command,
    },

    /// Confirms that an upload worker exists before bytes cross the independent transfer socket.
    #[serde(rename = "transfer_ready")]
    TransferReady {
        agent_id: AgentId,
        request_id: RequestId,
    },

    /// Sent by an agent when a request reaches its terminal result.
    ///
    /// The router routes this either back to a waiting one-shot REST caller or
    /// uses it to complete router-managed uploads and copy operations.
    #[serde(rename = "command_response")]
    CommandResponse {
        agent_id: AgentId,
        request_id: RequestId,
        result: CommandResult,
    },

    /// Sent by an agent to report copy byte progress or a later-discovered
    /// directory-download tar total.
    ///
    /// Direct uploads and file downloads still derive transferred counts from
    /// streamed chunks. Directory archives also send this once so the router can
    /// set `total_bytes` without overwriting those counted bytes.
    #[serde(rename = "transfer_progress_update")]
    TransferProgressUpdate {
        agent_id: AgentId,
        request_id: RequestId,
        transferred_bytes: u64,
        total_bytes: Option<u64>,
    },

    /// Requests router authorization before staged upload content can change the destination.
    #[serde(rename = "upload_publication_request")]
    UploadPublicationRequest {
        agent_id: AgentId,
        request_id: RequestId,
    },

    /// Orders publication against accepted cancellation in the router's authoritative lifecycle.
    #[serde(rename = "upload_publication_decision")]
    UploadPublicationDecision {
        request_id: RequestId,
        allowed: bool,
    },

    /// Sent by the router to ask an agent to abort an active download-style
    /// stream for the given request id.
    ///
    /// This is used when the REST-side consumer goes away or router cleanup
    /// cancels the transfer; the agent applies it to active uploads and
    /// downloads.
    #[serde(rename = "cancel_transfer")]
    CancelTransfer { request_id: RequestId },

    /// Requests one ephemeral dedicated terminal connection from this agent.
    #[serde(rename = "terminal_open")]
    TerminalOpen {
        terminal_id: terminal::TerminalId,
        token: String,
        size: terminal::TerminalSize,
        cwd: String,
    },

    /// Requests one ephemeral dedicated log connection from this agent.
    #[serde(rename = "log_stream_open")]
    LogStreamOpen {
        log_stream_id: log::LogStreamId,
        token: String,
    },

    /// Protocol-level error outside any request-specific response flow.
    ///
    /// The router currently uses this to reject invalid agent registration such
    /// as duplicate live agent names, and the agent treats it as fatal.
    #[serde(rename = "error")]
    Error { message: String },
}
