//! Typed outbound control frames and the per-request responder that produces them.
//!
//! Frames stay typed until the control writer serializes them, so workers and tests exchange
//! `AgentToServer` values instead of hand-serialized JSON, and actions that must happen only
//! after a frame reached the socket (re-exec) travel with the frame instead of being inferred by
//! re-parsing outgoing text.

use redoor_core::log;
use redoor_protocol::{
    ids::RequestId,
    wire::{AgentToServer, log::Level, request::CommandOutcome, stream::StreamChunk},
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;

/// Bounded lane of the connection writer; the control, interactive and bulk lanes share it.
pub(crate) type ControlSender = mpsc::Sender<OutboundFrame>;

/// Action the control writer runs only after the carrying frame was written successfully.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PostSend {
    /// Replaces the process image; `None` re-executes the current binary.
    Reexec { path: Option<PathBuf> },
}

impl PostSend {
    /// Performs the action; re-exec replaces the process (failure exits it), so this does not
    /// return in practice.
    pub(crate) fn run(self) {
        match self {
            Self::Reexec { path: Some(path) } => redoor_core::process::reexec_process(&path),
            Self::Reexec { path: None } => redoor_core::process::reexec_current_process(),
        }
    }
}

/// Payload of one websocket frame.
#[derive(Debug)]
pub(crate) enum ControlFrame {
    /// Protocol message serialized by the writer.
    Message(Box<AgentToServer>),
    /// Binary data frame of one stream.
    Stream(StreamChunk),
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

    /// Wraps one binary stream frame.
    pub(crate) fn stream(chunk: StreamChunk) -> Self {
        Self {
            frame: ControlFrame::Stream(chunk),
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
            ControlFrame::Stream(chunk) => Some(WsMessage::Binary(chunk.to_bytes().into())),
            ControlFrame::Ping => Some(WsMessage::Ping(bytes::Bytes::new())),
            ControlFrame::RawText(text) => Some(WsMessage::text(text.clone())),
        }
    }

    /// Returns the stream frame, if this frame carries one; used by tests.
    #[cfg(test)]
    pub(crate) fn as_stream(&self) -> Option<&StreamChunk> {
        match &self.frame {
            ControlFrame::Stream(chunk) => Some(chunk),
            _ => None,
        }
    }

    /// Returns the protocol message, if this frame carries one; used by tests and diagnostics.
    #[cfg(test)]
    pub(crate) fn as_message(&self) -> Option<&AgentToServer> {
        match &self.frame {
            ControlFrame::Message(message) => Some(message.as_ref()),
            ControlFrame::Stream(_) | ControlFrame::Ping | ControlFrame::RawText(_) => None,
        }
    }
}

/// Error returned when the control lane has closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ControlClosed;

/// Requests of the current connection that are running and still owe their one result.
///
/// The server relies on exactly one result per request: it is what releases the request on
/// the server. Every result goes through [`ResultLedger::close`], so a second result for the
/// same id is caught (a debug assertion) and never sent, and a result for an id that was never
/// started cannot be sent either.
#[derive(Clone, Default)]
pub(crate) struct ResultLedger {
    open: Arc<Mutex<HashSet<RequestId>>>,
}

impl ResultLedger {
    /// Locks the set, tolerating poison because every operation leaves it consistent.
    fn ids(&self) -> std::sync::MutexGuard<'_, HashSet<RequestId>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records that `request_id` started; `false` when it is already running, which only a
    /// misbehaving server can cause.
    pub(crate) fn open(&self, request_id: RequestId) -> bool {
        self.ids().insert(request_id)
    }

    /// Takes the one result slot of `request_id`; `false` when it was already answered.
    fn close(&self, request_id: RequestId) -> bool {
        self.ids().remove(&request_id)
    }

    /// Forgets every request when the connection that would carry their results ends.
    pub(crate) fn clear(&self) {
        self.ids().clear();
    }
}

/// Sends every control-lane message that belongs to one request.
#[derive(Clone)]
pub(crate) struct Responder {
    control: ControlSender,
    results: ResultLedger,
    request_id: RequestId,
}

impl Responder {
    /// Binds a responder to one request on the current connection.
    pub(crate) fn new(
        control: ControlSender,
        results: ResultLedger,
        request_id: RequestId,
    ) -> Self {
        Self {
            control,
            results,
            request_id,
        }
    }

    /// Request this responder answers.
    pub(crate) fn request_id(&self) -> RequestId {
        self.request_id
    }

    /// The control lane, for results that must carry a post-send action.
    pub(crate) fn control(&self) -> &ControlSender {
        &self.control
    }

    /// Whether the control connection that would carry responses has gone away.
    pub(crate) fn is_closed(&self) -> bool {
        self.control.is_closed()
    }

    /// Queues one frame, waiting for capacity so responses are backpressured rather than lost.
    async fn send(&self, frame: OutboundFrame) -> Result<(), ControlClosed> {
        self.control.send(frame).await.map_err(|_| ControlClosed)
    }

    /// Sends the request's one result on the control lane.
    pub(crate) async fn respond(&self, outcome: CommandOutcome) -> Result<(), ControlClosed> {
        self.respond_on(&self.control, outcome, None).await
    }

    /// Sends the request's one result on `lane`, running `after_send` once it was written.
    ///
    /// Streaming commands pass their stream's lane, so the result is written after all of the
    /// stream's data. A second result for the same request is a bug: it trips a debug
    /// assertion and is dropped instead of being sent.
    pub(crate) async fn respond_on(
        &self,
        lane: &ControlSender,
        outcome: CommandOutcome,
        after_send: Option<PostSend>,
    ) -> Result<(), ControlClosed> {
        if !self.results.close(self.request_id) {
            debug_assert!(
                false,
                "second result for request {}: {}",
                self.request_id,
                outcome.summary()
            );
            log!(
                Level::Warning,
                "Dropping a second result for request_id={}: result={}",
                self.request_id,
                outcome.summary()
            );
            return Ok(());
        }
        log!(
            Level::Info,
            "Command complete: request_id={}, result={}",
            self.request_id,
            outcome.summary()
        );
        lane.send(OutboundFrame {
            frame: ControlFrame::Message(Box::new(AgentToServer::Result {
                id: self.request_id,
                outcome,
            })),
            after_send,
        })
        .await
        .map_err(|_| ControlClosed)
    }

    /// Verification progress must not be mistaken for network throughput.
    pub(crate) async fn verification(
        &self,
        bytes: u64,
        verifying: bool,
    ) -> Result<(), ControlClosed> {
        self.send(OutboundFrame::message(
            AgentToServer::TransferVerification {
                id: self.request_id,
                bytes,
                verifying,
            },
        ))
        .await
    }

    /// Reports byte progress (or a late-discovered total) for router-managed transfers.
    pub(crate) async fn progress(
        &self,
        transferred_bytes: u64,
        total_bytes: Option<u64>,
    ) -> Result<(), ControlClosed> {
        self.send(OutboundFrame::message(AgentToServer::TransferProgress {
            id: self.request_id,
            transferred_bytes,
            total_bytes,
        }))
        .await
    }

    /// Asks the router to order publication of staged upload content against cancellation.
    pub(crate) async fn request_publication(&self) -> Result<(), ControlClosed> {
        self.send(OutboundFrame::message(AgentToServer::PublishRequest {
            id: self.request_id,
        }))
        .await
    }

    /// Returns per-stream credit for an upload: the initial window once its worker is ready,
    /// then batches as the worker stages the drained frames.
    pub(crate) async fn stream_credit(&self, frames: u32) -> Result<(), ControlClosed> {
        self.send(OutboundFrame::message(AgentToServer::Credit {
            stream: self.request_id,
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
        let results = ResultLedger::default();
        results.open(request_id);
        (Self::new(control, results, request_id), frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the responder queues typed frames with their post-send action.
    #[tokio::test]
    async fn responder_queues_typed_frames() {
        let (responder, mut frames) = Responder::recording(RequestId::new(7), 4);

        responder.stream_credit(4).await.unwrap();
        responder
            .respond_on(
                &responder.control,
                CommandOutcome::Ok(serde_json::Value::Null),
                Some(PostSend::Reexec { path: None }),
            )
            .await
            .unwrap();

        let grant = frames.recv().await.unwrap();
        // The first credit grant names the request so the server can start its producer.
        assert!(matches!(
            grant.as_message(),
            Some(AgentToServer::Credit { stream, frames: 4 }) if *stream == RequestId::new(7)
        ));
        let response = frames.recv().await.unwrap();
        // The result is typed, so tests and the writer need no JSON parsing.
        assert!(matches!(
            response.as_message(),
            Some(AgentToServer::Result {
                outcome: CommandOutcome::Ok(serde_json::Value::Null),
                ..
            })
        ));
        // The re-exec travels with the frame that must reach the socket first.
        assert_eq!(response.after_send, Some(PostSend::Reexec { path: None }));
    }

    /// Verifies the ledger hands out exactly one result slot per started request.
    #[test]
    fn ledger_allows_exactly_one_result_per_request() {
        let ledger = ResultLedger::default();
        // A started request owes one result.
        assert!(ledger.open(RequestId::new(1)));
        // The same id cannot start twice while it runs.
        assert!(!ledger.open(RequestId::new(1)));
        // The first result takes the slot.
        assert!(ledger.close(RequestId::new(1)));
        // A second result for the same request finds no slot and is never sent.
        assert!(!ledger.close(RequestId::new(1)));
    }

    /// Verifies a second result trips the debug assertion instead of reaching the server.
    #[tokio::test]
    #[should_panic(expected = "second result for request 9")]
    async fn second_result_is_a_bug() {
        let (responder, _frames) = Responder::recording(RequestId::new(9), 4);
        let ok = || CommandOutcome::Ok(serde_json::Value::Null);
        responder.respond(ok()).await.unwrap();
        let _ = responder.respond(ok()).await;
    }
}
