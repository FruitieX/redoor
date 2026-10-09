//! Agent <-> server websocket protocol: the control envelope and its framing helpers.

pub mod command;
pub mod exec;
pub mod log;
pub mod policy;
pub mod stream;
pub mod terminal;
pub mod transfer;

use crate::identity::BinaryIdentity;
use crate::ids::{AgentId, RequestId};
use command::{Command, CommandResult};
use serde::{Deserialize, Serialize};

/// Control-socket frames sent by the server to an agent.
///
/// Split from [`AgentToServer`] so each side matches only the frames it can receive and a new
/// variant cannot be silently ignored by a `_` arm. The serde representation (internally tagged
/// by `type`) is identical to the former shared envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerToAgent {
    /// Instructs the authoritative control session to establish its persistent
    /// payload socket with a secret that stale sessions cannot reuse.
    #[serde(rename = "transfer_socket_open")]
    TransferSocketOpen { token: String },

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

    /// Orders publication against accepted cancellation in the router's authoritative lifecycle.
    #[serde(rename = "upload_publication_decision")]
    UploadPublicationDecision {
        request_id: RequestId,
        allowed: bool,
    },

    /// Sent by the router to ask an agent to abort an active request-cancelable command.
    ///
    /// This is used when the REST-side consumer goes away or router cleanup
    /// cancels the transfer; the agent applies it to uploads, downloads, exec and
    /// local copy or move workers registered under the request id.
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

impl ServerToAgent {
    /// Recovers the request id of a `command` frame whose body did not parse.
    ///
    /// A newer server may send a command variant this agent does not know; answering with a
    /// typed error lets the waiting REST caller fail immediately instead of timing out. Other
    /// frame types are not answered because they have no command response to correlate.
    pub fn unparsed_command_request_id(text: &str) -> Option<RequestId> {
        /// Minimal view of a frame that only needs its tag and correlation id.
        #[derive(Deserialize)]
        struct CommandProbe {
            #[serde(rename = "type")]
            frame_type: String,
            request_id: Option<RequestId>,
        }

        let probe: CommandProbe = serde_json::from_str(text).ok()?;
        if probe.frame_type == "command" {
            probe.request_id
        } else {
            None
        }
    }
}

/// Control-socket frames sent by an agent to the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentToServer {
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

    /// Optional explicit teardown signal from an agent asking the router to
    /// remove it from the active registry; websocket session shutdown also
    /// triggers the same cleanup automatically.
    #[serde(rename = "agent_unregister")]
    AgentUnregister { agent_id: AgentId },

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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies server frames keep the exact JSON shape of the former shared envelope.
    #[test]
    fn server_frames_keep_their_wire_tags() {
        let frames = [
            (
                ServerToAgent::TransferSocketOpen {
                    token: "t".to_string(),
                },
                r#"{"type":"transfer_socket_open","token":"t"}"#,
            ),
            (
                ServerToAgent::Command {
                    agent_id: AgentId::from("a"),
                    request_id: RequestId::new(1),
                    command: Command::ListTrash,
                },
                r#"{"type":"command","agent_id":"a","request_id":1,"command":{"type":"ListTrash"}}"#,
            ),
            (
                ServerToAgent::UploadPublicationDecision {
                    request_id: RequestId::new(2),
                    allowed: true,
                },
                r#"{"type":"upload_publication_decision","request_id":2,"allowed":true}"#,
            ),
            (
                ServerToAgent::CancelTransfer {
                    request_id: RequestId::new(3),
                },
                r#"{"type":"cancel_transfer","request_id":3}"#,
            ),
            (
                ServerToAgent::Error {
                    message: "m".to_string(),
                },
                r#"{"type":"error","message":"m"}"#,
            ),
        ];
        for (frame, expected) in frames {
            // Fake agents in the integration tests parse these exact shapes from the raw socket.
            assert_eq!(serde_json::to_string(&frame).unwrap(), expected);
        }
    }

    /// Verifies agent frames keep the exact JSON shape of the former shared envelope.
    #[test]
    fn agent_frames_keep_their_wire_tags() {
        let frames = [
            (
                AgentToServer::AgentUnregister {
                    agent_id: AgentId::from("a"),
                },
                r#"{"type":"agent_unregister","agent_id":"a"}"#,
            ),
            (
                AgentToServer::TransferReady {
                    agent_id: AgentId::from("a"),
                    request_id: RequestId::new(1),
                },
                r#"{"type":"transfer_ready","agent_id":"a","request_id":1}"#,
            ),
            (
                AgentToServer::CommandResponse {
                    agent_id: AgentId::from("a"),
                    request_id: RequestId::new(2),
                    result: CommandResult::RawUpload,
                },
                r#"{"type":"command_response","agent_id":"a","request_id":2,"result":{"type":"RawUpload"}}"#,
            ),
            (
                AgentToServer::TransferProgressUpdate {
                    agent_id: AgentId::from("a"),
                    request_id: RequestId::new(3),
                    transferred_bytes: 4,
                    total_bytes: None,
                },
                r#"{"type":"transfer_progress_update","agent_id":"a","request_id":3,"transferred_bytes":4,"total_bytes":null}"#,
            ),
            (
                AgentToServer::UploadPublicationRequest {
                    agent_id: AgentId::from("a"),
                    request_id: RequestId::new(5),
                },
                r#"{"type":"upload_publication_request","agent_id":"a","request_id":5}"#,
            ),
        ];
        for (frame, expected) in frames {
            // The server and fake agents decode these exact tags and field names.
            assert_eq!(serde_json::to_string(&frame).unwrap(), expected);
        }
    }

    /// Verifies unknown command variants still expose their request id.
    #[test]
    fn unknown_command_frames_still_yield_their_request_id() {
        let unknown = r#"{"type":"command","agent_id":"a","request_id":9,"command":{"type":"FutureCommand"}}"#;

        // The variant is unknown to this build, so normal parsing must fail.
        assert!(serde_json::from_str::<ServerToAgent>(unknown).is_err());
        // The correlation id survives so the agent can answer instead of leaving the caller waiting.
        assert_eq!(
            ServerToAgent::unparsed_command_request_id(unknown),
            Some(RequestId::new(9))
        );
        // Non-command frames have no response channel and must not be answered.
        assert_eq!(
            ServerToAgent::unparsed_command_request_id(
                r#"{"type":"cancel_transfer","request_id":"bad"}"#
            ),
            None
        );
    }
}
