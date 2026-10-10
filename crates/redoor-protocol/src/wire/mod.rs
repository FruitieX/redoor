//! Agent <-> server websocket protocol: the control envelope and its framing helpers.
//!
//! Identity is a property of the authenticated connection, not of individual frames: only the
//! first frame (`hello`) names the agent, and every later frame is attributed to whoever owns the
//! socket. Request ids are scoped to one connection and allocated by the server.

pub mod command;
pub mod exec;
pub mod handshake;
pub mod log;
pub mod policy;
pub mod registration;
pub mod stream;
pub mod terminal;
pub mod transfer;

use crate::ids::{AgentId, RequestId, SocketId};
use command::{Command, CommandResult};
use handshake::RejectReason;
use registration::{AgentCapabilities, AgentIdentity};
use serde::{Deserialize, Serialize};

/// Control frames sent by the server to an agent.
///
/// Split from [`AgentToServer`] so each side matches only the frames it can receive and a new
/// variant cannot be silently ignored by a `_` arm.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerToAgent {
    /// Accepts the agent's `hello`; the connection now speaks for that agent.
    Welcome {
        /// Server-side identity of this connection, shown in diagnostics and the UI.
        connection_id: SocketId,
    },

    /// Refuses the agent's `hello`; the agent must not retry with the same identity.
    Rejected {
        reason: RejectReason,
        message: String,
    },

    /// Ends an accepted connection on purpose (replaced by a newer connection of the same
    /// agent, or the managed agent was stopped), so the agent exits instead of reconnecting.
    Evicted { message: String },

    /// Returns flow-control credit for one agent-to-server stream (download or exec output).
    ///
    /// The agent may have at most the granted number of non-terminal frames in flight per
    /// stream, so one slow consumer stalls only its own stream instead of the shared socket.
    Credit { stream: RequestId, frames: u32 },

    /// Starts a request on the agent; `id` correlates its response and any stream frames.
    Command { id: RequestId, command: Command },

    /// Orders upload publication against cancellation in the router's authoritative lifecycle.
    PublishDecision { id: RequestId, allowed: bool },

    /// Instructs the agent to open its persistent payload socket with a secret that only the
    /// current connection knows, so stale connections cannot attach to its streams.
    TransferSocketOpen { token: String },

    /// Asks the agent to abort a request-cancelable command.
    Cancel { id: RequestId },

    /// Requests one ephemeral dedicated terminal connection from this agent.
    TerminalOpen {
        terminal_id: terminal::TerminalId,
        token: String,
        size: terminal::TerminalSize,
        cwd: String,
    },

    /// Requests one ephemeral dedicated log connection from this agent.
    LogStreamOpen {
        log_stream_id: log::LogStreamId,
        token: String,
    },
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
            id: Option<RequestId>,
        }

        let probe: CommandProbe = serde_json::from_str(text).ok()?;
        if probe.frame_type == "command" {
            probe.id
        } else {
            None
        }
    }
}

/// Control frames sent by an agent to the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentToServer {
    /// First frame of every connection: binds the authenticated socket to a stable agent id
    /// and publishes the metadata the router, UI and detail responses use.
    Hello {
        agent_id: AgentId,
        agent_name: String,
        /// Platform, account and binary; flattened so the JSON keeps its flat field names.
        #[serde(flatten)]
        identity: AgentIdentity,
        /// Optional features; flattened, and each flag defaults to off.
        #[serde(flatten)]
        capabilities: AgentCapabilities,
    },

    /// Confirms that an upload worker exists before bytes cross the transfer socket.
    TransferReady { id: RequestId },

    /// Final result of one request.
    CommandResponse {
        id: RequestId,
        result: CommandResult,
    },

    /// Copy byte progress or a later-discovered directory-download tar total.
    ///
    /// Direct uploads and file downloads derive transferred counts from streamed frames.
    /// Directory archives also send this once so the router can set `total_bytes` without
    /// overwriting those counted bytes.
    TransferProgress {
        id: RequestId,
        transferred_bytes: u64,
        total_bytes: Option<u64>,
    },

    /// Requests router authorization before staged upload content can change the destination.
    PublishRequest { id: RequestId },

    /// Returns flow-control credit for one server-to-agent stream (upload, edit or copy
    /// destination) as the agent's worker stages frames, so a slow destination stalls only
    /// its own stream.
    Credit { stream: RequestId, frames: u32 },
}

impl AgentToServer {
    /// Wire tag of the frame, for logs that must not echo payloads (results can carry file
    /// contents).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::TransferReady { .. } => "transfer_ready",
            Self::CommandResponse { .. } => "command_response",
            Self::TransferProgress { .. } => "transfer_progress",
            Self::PublishRequest { .. } => "publish_request",
            Self::Credit { .. } => "credit",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies server frames keep the exact JSON shape fake agents and agents decode.
    #[test]
    fn server_frames_keep_their_wire_tags() {
        let frames = [
            (
                ServerToAgent::Rejected {
                    reason: RejectReason::DuplicateName,
                    message: "m".to_string(),
                },
                r#"{"type":"rejected","reason":"duplicate_name","message":"m"}"#,
            ),
            (
                ServerToAgent::Evicted {
                    message: "m".to_string(),
                },
                r#"{"type":"evicted","message":"m"}"#,
            ),
            (
                ServerToAgent::TransferSocketOpen {
                    token: "t".to_string(),
                },
                r#"{"type":"transfer_socket_open","token":"t"}"#,
            ),
            (
                ServerToAgent::Credit {
                    stream: RequestId::new(4),
                    frames: 2,
                },
                r#"{"type":"credit","stream":4,"frames":2}"#,
            ),
            (
                ServerToAgent::Command {
                    id: RequestId::new(1),
                    command: Command::ListTrash,
                },
                r#"{"type":"command","id":1,"command":{"type":"ListTrash"}}"#,
            ),
            (
                ServerToAgent::PublishDecision {
                    id: RequestId::new(2),
                    allowed: true,
                },
                r#"{"type":"publish_decision","id":2,"allowed":true}"#,
            ),
            (
                ServerToAgent::Cancel {
                    id: RequestId::new(3),
                },
                r#"{"type":"cancel","id":3}"#,
            ),
        ];
        for (frame, expected) in frames {
            // Fake agents in the integration tests parse these exact shapes from the raw socket.
            assert_eq!(serde_json::to_string(&frame).unwrap(), expected);
        }
    }

    /// Verifies agent frames carry no agent id, because the connection identifies the agent.
    #[test]
    fn agent_frames_keep_their_wire_tags() {
        let frames = [
            (
                AgentToServer::TransferReady {
                    id: RequestId::new(1),
                },
                r#"{"type":"transfer_ready","id":1}"#,
            ),
            (
                AgentToServer::CommandResponse {
                    id: RequestId::new(2),
                    result: CommandResult::RawUpload,
                },
                r#"{"type":"command_response","id":2,"result":{"type":"RawUpload"}}"#,
            ),
            (
                AgentToServer::TransferProgress {
                    id: RequestId::new(3),
                    transferred_bytes: 4,
                    total_bytes: None,
                },
                r#"{"type":"transfer_progress","id":3,"transferred_bytes":4,"total_bytes":null}"#,
            ),
            (
                AgentToServer::PublishRequest {
                    id: RequestId::new(5),
                },
                r#"{"type":"publish_request","id":5}"#,
            ),
            (
                AgentToServer::Credit {
                    stream: RequestId::new(6),
                    frames: 2,
                },
                r#"{"type":"credit","stream":6,"frames":2}"#,
            ),
        ];
        for (frame, expected) in frames {
            // The server and fake agents decode these exact tags and field names.
            assert_eq!(serde_json::to_string(&frame).unwrap(), expected);
        }
    }

    /// Verifies flattening keeps the flat hello JSON that agents and fixtures send.
    #[test]
    fn hello_round_trips_its_flat_json() {
        let json = r#"{"type":"hello","agent_id":"a","agent_name":"n","os":"linux","arch":"x86_64","hostname":"h","username":"u","cwd":"/home/u","binary":{"version":"0.0.0","git_rev":"r","git_dirty":false,"version_dirty":false,"build_mode":"debug","build_date":"d"},"uid":1000,"is_root":false,"supports_self_exec":true,"supports_native_open":false,"supports_trash":true,"supports_move_to_trash":false}"#;
        let hello: AgentToServer = serde_json::from_str(json).unwrap();
        let AgentToServer::Hello {
            identity,
            capabilities,
            ..
        } = &hello
        else {
            panic!("expected a hello");
        };
        // Flat wire fields land in the nested values, including the renamed directory.
        assert_eq!(identity.default_directory, "/home/u");
        assert_eq!(identity.uid, Some(1000));
        assert!(capabilities.supports_trash);
        // Serializing yields the same flat object the fake agents build by hand.
        assert_eq!(
            serde_json::to_value(&hello).unwrap(),
            serde_json::from_str::<serde_json::Value>(json).unwrap()
        );
        // Agents with trash inventory support can always move entries to trash.
        assert!(capabilities.normalized().supports_move_to_trash);
    }

    /// Verifies unknown command variants still expose their request id.
    #[test]
    fn unknown_command_frames_still_yield_their_request_id() {
        let unknown = r#"{"type":"command","id":9,"command":{"type":"FutureCommand"}}"#;

        // The variant is unknown to this build, so normal parsing must fail.
        assert!(serde_json::from_str::<ServerToAgent>(unknown).is_err());
        // The correlation id survives so the agent can answer instead of leaving the caller waiting.
        assert_eq!(
            ServerToAgent::unparsed_command_request_id(unknown),
            Some(RequestId::new(9))
        );
        // Non-command frames have no response channel and must not be answered.
        assert_eq!(
            ServerToAgent::unparsed_command_request_id(r#"{"type":"cancel","id":"bad"}"#),
            None
        );
    }
}
