//! Agent <-> server websocket protocol: the control envelope and its framing helpers.

pub mod command;
pub mod exec;
pub mod log;
pub mod policy;
pub mod registration;
pub mod stream;
pub mod terminal;
pub mod transfer;

use crate::ids::{AgentId, RequestId};
use command::{Command, CommandResult};
use registration::{AgentCapabilities, AgentIdentity};
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
    TransferSocketOpen {
        token: String,
        /// Confirms per-stream credit for this control session.
        ///
        /// Sent only to agents that advertised `supports_stream_credit`, so both directions
        /// switch to credit together; absent (older servers) means the agent keeps the
        /// uncredited behavior where only socket backpressure limits a stream.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        stream_credit: bool,
    },

    /// Returns flow-control credit for one agent-to-server stream (download or exec output).
    ///
    /// The agent's per-stream writer may have at most the granted number of non-terminal
    /// frames in flight, so one slow consumer stalls only its own stream instead of the shared
    /// transfer socket. Sent on the priority lane as the server-side consumer drains frames.
    #[serde(rename = "stream_credit")]
    StreamCredit { request_id: RequestId, frames: u32 },

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
        /// Shared secret proving this agent was started with server credentials.
        token: String,
        /// Platform, account and binary; flattened so the JSON keeps its flat field names.
        #[serde(flatten)]
        identity: AgentIdentity,
        /// Optional features; flattened, and each flag defaults to off for older agents.
        #[serde(flatten)]
        capabilities: AgentCapabilities,
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

    /// Returns flow-control credit for one server-to-agent stream (upload, edit or copy
    /// destination) as the agent's worker stages frames, so a slow destination stalls only
    /// its own stream. Only sent when the server confirmed `stream_credit`.
    #[serde(rename = "stream_credit")]
    StreamCredit { request_id: RequestId, frames: u32 },
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
                    stream_credit: false,
                },
                r#"{"type":"transfer_socket_open","token":"t"}"#,
            ),
            (
                ServerToAgent::TransferSocketOpen {
                    token: "t".to_string(),
                    stream_credit: true,
                },
                r#"{"type":"transfer_socket_open","token":"t","stream_credit":true}"#,
            ),
            (
                ServerToAgent::StreamCredit {
                    request_id: RequestId::new(4),
                    frames: 2,
                },
                r#"{"type":"stream_credit","request_id":4,"frames":2}"#,
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
            (
                AgentToServer::StreamCredit {
                    request_id: RequestId::new(6),
                    frames: 2,
                },
                r#"{"type":"stream_credit","request_id":6,"frames":2}"#,
            ),
        ];
        for (frame, expected) in frames {
            // The server and fake agents decode these exact tags and field names.
            assert_eq!(serde_json::to_string(&frame).unwrap(), expected);
        }
    }

    /// Verifies older peers that omit the credit fields keep the uncredited protocol.
    #[test]
    fn credit_negotiation_fields_default_to_disabled() {
        let open: ServerToAgent =
            serde_json::from_str(r#"{"type":"transfer_socket_open","token":"t"}"#).unwrap();
        // An older server never confirms credit, so a new agent must not wait for grants.
        assert!(matches!(
            open,
            ServerToAgent::TransferSocketOpen {
                stream_credit: false,
                ..
            }
        ));
        let register: AgentToServer = serde_json::from_str(
            r#"{"type":"agent_register","agent_id":"a","agent_name":"a","os":"linux","arch":"x86_64","hostname":"h","username":"u","cwd":"/","token":"t","binary":{"version":"0.0.0","git_rev":"r","git_dirty":false,"version_dirty":false,"build_mode":"debug","build_date":"d"}}"#,
        )
        .unwrap();
        // Fake agents in the integration tests register without the capability field.
        assert!(matches!(
            register,
            AgentToServer::AgentRegister {
                capabilities: AgentCapabilities {
                    supports_stream_credit: false,
                    ..
                },
                ..
            }
        ));
    }

    /// Verifies flattening keeps the flat registration JSON that agents and fixtures send.
    #[test]
    fn registration_round_trips_its_flat_json() {
        let json = r#"{"type":"agent_register","agent_id":"a","agent_name":"n","token":"t","os":"linux","arch":"x86_64","hostname":"h","username":"u","cwd":"/home/u","binary":{"version":"0.0.0","git_rev":"r","git_dirty":false,"version_dirty":false,"build_mode":"debug","build_date":"d"},"uid":1000,"is_root":false,"supports_self_exec":true,"supports_native_open":false,"supports_trash":true,"supports_move_to_trash":false,"supports_stream_credit":true}"#;
        let register: AgentToServer = serde_json::from_str(json).unwrap();
        let AgentToServer::AgentRegister {
            identity,
            capabilities,
            ..
        } = &register
        else {
            panic!("expected a registration");
        };
        // Flat wire fields land in the nested values, including the renamed directory.
        assert_eq!(identity.default_directory, "/home/u");
        assert_eq!(identity.uid, Some(1000));
        assert!(capabilities.supports_trash && capabilities.supports_stream_credit);
        // Serializing yields the same flat object, so agents of either version interoperate.
        assert_eq!(
            serde_json::to_value(&register).unwrap(),
            serde_json::from_str::<serde_json::Value>(json).unwrap()
        );
        // Older agents that only advertised inventory support can still move to trash.
        assert!(capabilities.normalized().supports_move_to_trash);
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
