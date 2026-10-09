//! Typed outbound control frames and the per-request responder that produces them.
//!
//! Frames stay typed until the control writer serializes them, so workers and tests exchange
//! `AgentToServer` values instead of hand-serialized JSON, and actions that must happen only
//! after a frame reached the socket (re-exec) travel with the frame instead of being inferred by
//! re-parsing outgoing text.

use redoor_core::log;
use redoor_protocol::{
    ids::{AgentId, RequestId},
    wire::{AgentToServer, command::CommandResult, log::Level},
};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;

/// Bounded control lane shared by the runtime, command workers and the liveness monitor.
pub(crate) type ControlSender = mpsc::Sender<OutboundFrame>;

/// Action the control writer runs only after the carrying frame was written successfully.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PostSend {
    /// Replaces the process image; `None` re-executes the current binary.
    Reexec { path: Option<PathBuf> },
}

impl PostSend {
    /// Derives the post-send action implied by a command result.
    ///
    /// Restart and self-exec must answer before the process is replaced, otherwise the router
    /// would see a dropped connection instead of a successful response.
    pub(crate) fn for_result(result: &CommandResult) -> Option<Self> {
        match result {
            CommandResult::Restart => Some(Self::Reexec { path: None }),
            CommandResult::SelfExec { path } => Some(Self::Reexec {
                path: Some(PathBuf::from(path)),
            }),
            _ => None,
        }
    }

    /// Performs the action; re-exec replaces the process (failure exits it), so this does not
    /// return in practice.
    pub(crate) fn run(self) {
        match self {
            Self::Reexec { path: Some(path) } => redoor_core::process::reexec_process(&path),
            Self::Reexec { path: None } => redoor_core::process::reexec_current_process(),
        }
    }
}

/// Payload of one control-socket frame.
#[derive(Debug)]
pub(crate) enum ControlFrame {
    /// Protocol message serialized by the writer.
    Message(Box<AgentToServer>),
    /// Keepalive that lets the server's stale check observe a quiet but healthy agent.
    Ping,
    /// Operator text forwarded verbatim from stdin for manual protocol debugging.
    RawText(String),
}

/// One queued control frame plus the action to run once it has been written.
#[derive(Debug)]
pub(crate) struct OutboundFrame {
    pub(crate) frame: ControlFrame,
    pub(crate) after_send: Option<PostSend>,
}

impl OutboundFrame {
    /// Wraps a protocol message without a post-send action.
    pub(crate) fn message(message: AgentToServer) -> Self {
        Self {
            frame: ControlFrame::Message(Box::new(message)),
            after_send: None,
        }
    }

    /// Builds a keepalive ping frame.
    pub(crate) fn ping() -> Self {
        Self {
            frame: ControlFrame::Ping,
            after_send: None,
        }
    }

    /// Builds a verbatim text frame.
    pub(crate) fn raw_text(text: String) -> Self {
        Self {
            frame: ControlFrame::RawText(text),
            after_send: None,
        }
    }

    /// Serializes the frame for the websocket, returning `None` if encoding failed.
    pub(crate) fn encode(&self) -> Option<WsMessage> {
        match &self.frame {
            ControlFrame::Message(message) => match serde_json::to_string(message) {
                Ok(json) => Some(WsMessage::text(json)),
                Err(error) => {
                    log!(
                        Level::Warning,
                        "Dropping control frame that failed to serialize: {error}"
                    );
                    None
                }
            },
            ControlFrame::Ping => Some(WsMessage::Ping(bytes::Bytes::new())),
            ControlFrame::RawText(text) => Some(WsMessage::text(text.clone())),
        }
    }

    /// Returns the protocol message, if this frame carries one; used by tests and diagnostics.
    #[cfg(test)]
    pub(crate) fn as_message(&self) -> Option<&AgentToServer> {
        match &self.frame {
            ControlFrame::Message(message) => Some(message.as_ref()),
            ControlFrame::Ping | ControlFrame::RawText(_) => None,
        }
    }
}

/// Error returned when the control lane has closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ControlClosed;

/// Sends every control-lane message that belongs to one request.
#[derive(Clone)]
pub(crate) struct Responder {
    control: ControlSender,
    agent_id: AgentId,
    request_id: RequestId,
}

impl Responder {
    /// Binds a responder to one request on the current control connection.
    pub(crate) fn new(control: ControlSender, agent_id: AgentId, request_id: RequestId) -> Self {
        Self {
            control,
            agent_id,
            request_id,
        }
    }

    /// Request this responder answers.
    pub(crate) fn request_id(&self) -> RequestId {
        self.request_id
    }

    /// Whether the control connection that would carry responses has gone away.
    pub(crate) fn is_closed(&self) -> bool {
        self.control.is_closed()
    }

    /// Queues one frame, waiting for capacity so responses are backpressured rather than lost.
    async fn send(&self, frame: OutboundFrame) -> Result<(), ControlClosed> {
        self.control.send(frame).await.map_err(|_| ControlClosed)
    }

    /// Sends the terminal result and logs it as the request's completion.
    pub(crate) async fn respond(&self, result: CommandResult) -> Result<(), ControlClosed> {
        log!(
            Level::Info,
            "Command complete: agent_id={}, request_id={}, result={}",
            self.agent_id,
            self.request_id,
            result.summary()
        );
        let after_send = PostSend::for_result(&result);
        self.send(OutboundFrame {
            frame: ControlFrame::Message(Box::new(AgentToServer::CommandResponse {
                agent_id: self.agent_id.clone(),
                request_id: self.request_id,
                result,
            })),
            after_send,
        })
        .await
    }

    /// Reports byte progress (or a late-discovered total) for router-managed transfers.
    pub(crate) async fn progress(
        &self,
        transferred_bytes: u64,
        total_bytes: Option<u64>,
    ) -> Result<(), ControlClosed> {
        self.send(OutboundFrame::message(
            AgentToServer::TransferProgressUpdate {
                agent_id: self.agent_id.clone(),
                request_id: self.request_id,
                transferred_bytes,
                total_bytes,
            },
        ))
        .await
    }

    /// Announces that an upload worker exists so the server may start sending chunks.
    pub(crate) async fn transfer_ready(&self) -> Result<(), ControlClosed> {
        self.send(OutboundFrame::message(AgentToServer::TransferReady {
            agent_id: self.agent_id.clone(),
            request_id: self.request_id,
        }))
        .await
    }

    /// Asks the router to order publication of staged upload content against cancellation.
    pub(crate) async fn request_publication(&self) -> Result<(), ControlClosed> {
        self.send(OutboundFrame::message(
            AgentToServer::UploadPublicationRequest {
                agent_id: self.agent_id.clone(),
                request_id: self.request_id,
            },
        ))
        .await
    }

    /// Returns per-stream credit for an upload after its worker staged the drained frames.
    pub(crate) async fn stream_credit(&self, frames: u32) -> Result<(), ControlClosed> {
        self.send(OutboundFrame::message(AgentToServer::StreamCredit {
            request_id: self.request_id,
            frames,
        }))
        .await
    }

    /// Creates a responder whose frames are captured by the returned receiver.
    #[cfg(test)]
    pub(crate) fn recording(
        request_id: RequestId,
        capacity: usize,
    ) -> (Self, mpsc::Receiver<OutboundFrame>) {
        let (control, frames) = mpsc::channel(capacity);
        (
            Self::new(control, AgentId::from("test-agent"), request_id),
            frames,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies only process-replacing results produce a post-send action.
    #[test]
    fn restart_results_carry_a_reexec_action() {
        // Restart must replace the current binary only after its response is written.
        assert_eq!(
            PostSend::for_result(&CommandResult::Restart),
            Some(PostSend::Reexec { path: None })
        );
        // Self-exec carries the validated replacement path to the writer.
        assert_eq!(
            PostSend::for_result(&CommandResult::SelfExec {
                path: "/opt/redoor".to_string()
            }),
            Some(PostSend::Reexec {
                path: Some(PathBuf::from("/opt/redoor"))
            })
        );
        // Ordinary results never replace the process.
        assert_eq!(PostSend::for_result(&CommandResult::RawUpload), None);
    }

    /// Verifies the responder queues typed frames with their post-send action.
    #[tokio::test]
    async fn responder_queues_typed_frames() {
        let (responder, mut frames) = Responder::recording(RequestId::new(7), 4);

        responder.transfer_ready().await.unwrap();
        responder.respond(CommandResult::Restart).await.unwrap();

        let ready = frames.recv().await.unwrap();
        // Readiness names the request so the server can release its HTTP body producer.
        assert!(matches!(
            ready.as_message(),
            Some(AgentToServer::TransferReady { request_id, .. }) if *request_id == RequestId::new(7)
        ));
        let response = frames.recv().await.unwrap();
        // The terminal response is typed, so tests and the writer need no JSON parsing.
        assert!(matches!(
            response.as_message(),
            Some(AgentToServer::CommandResponse {
                result: CommandResult::Restart,
                ..
            })
        ));
        // The re-exec travels with the frame that must reach the socket first.
        assert_eq!(response.after_send, Some(PostSend::Reexec { path: None }));
    }
}
