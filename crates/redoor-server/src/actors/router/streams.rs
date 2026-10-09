//! The router-free data path: stream leases, per-connection stream tables and consumers.
//!
//! The router admits streams, tracks their lifecycle and handles cancellation, but payload
//! frames never pass through its mailbox. They travel directly between the transfer-socket
//! tasks and the HTTP or copy consumers through the types here, so a 64 KiB chunk costs no
//! router messages, and only terminal events (completion, failure, protocol violation) go back
//! to the router. With per-stream credit the transfer reader also never waits for a consumer,
//! so one stalled HTTP client cannot freeze the other streams sharing its agent's socket.

use axum::extract::ws::Message as WsMessage;
use redoor_core::stream_credit::{CreditGate, CreditLedger};
use redoor_protocol::{
    ids::{AgentId, ChunkIndex, RequestId},
    wire::{
        ServerToAgent,
        stream::{
            STREAM_CREDIT_QUEUE_FRAMES, StreamChunk, StreamChunkFrameRequest, StreamChunkFrames,
            StreamPayloadKind,
        },
    },
};
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex, MutexGuard, OnceLock, PoisonError,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Queue depth for streams of agents without credit, matching the old depth-one hand-off so
/// those agents keep their previous backpressure behavior.
const UNCREDITED_QUEUE_FRAMES: usize = 1;

/// What a stream needs to reach its agent without asking the router.
#[derive(Clone, Debug)]
pub struct AgentLink {
    /// Priority control lane used for credit grants, which must not queue behind commands.
    priority: mpsc::Sender<WsMessage>,
    /// Payload lane for server-to-agent frames; absent when the transfer socket is down.
    transfer: Option<mpsc::Sender<WsMessage>>,
    /// Whether this control session negotiated per-stream credit.
    credit: bool,
}

impl AgentLink {
    /// Captures the lanes of one control session and its current transfer socket.
    pub(crate) fn new(
        priority: mpsc::Sender<WsMessage>,
        transfer: Option<mpsc::Sender<WsMessage>>,
        credit: bool,
    ) -> Self {
        Self {
            priority,
            transfer,
            credit,
        }
    }

    /// Returns credit for frames a consumer drained, waiting for priority-lane capacity so a
    /// grant is never lost (a lost grant would shrink the window for the rest of the stream).
    async fn grant(&self, request_id: RequestId, frames: u32) {
        let message = ServerToAgent::StreamCredit { request_id, frames };
        if let Ok(json) = serde_json::to_string(&message) {
            let _ = self.priority.send(WsMessage::Text(json.into())).await;
        }
    }
}

/// Router-issued handle for one admitted stream, shared by the router and the data path.
///
/// The router keeps a clone to cancel the stream and to read its progress; the data path
/// counts bytes into `bytes` and stops when `cancel` fires. No chunk ever has to go back to
/// the router to be accounted for.
#[derive(Clone, Debug)]
pub struct StreamLease {
    /// Request id the agent uses in the stream's frames.
    pub(crate) id: RequestId,
    /// Lanes of the agent at the other end of the stream.
    pub(crate) link: AgentLink,
    /// Stops the data path; frames for a cancelled stream are discarded.
    pub(crate) cancel: CancellationToken,
    /// Payload bytes accepted by the consumer (inbound) or the agent's lane (outbound).
    pub(crate) bytes: Arc<AtomicU64>,
    /// Reason reported to the consumer when the router failed the stream.
    fault: Arc<OnceLock<String>>,
}

impl StreamLease {
    /// Creates a lease whose byte counter starts at `initial_bytes` (resumed downloads).
    pub(crate) fn new(id: RequestId, link: AgentLink, initial_bytes: u64) -> Self {
        Self {
            id,
            link,
            cancel: CancellationToken::new(),
            bytes: Arc::new(AtomicU64::new(initial_bytes)),
            fault: Arc::new(OnceLock::new()),
        }
    }

    /// Stops the data path without reporting an error to the consumer.
    pub(crate) fn stop(&self) {
        self.cancel.cancel();
    }

    /// Stops the data path and makes the consumer observe `reason` as a stream error.
    pub(crate) fn fail(&self, reason: String) {
        let _ = self.fault.set(reason);
        self.cancel.cancel();
    }

    /// Bytes accounted so far, read by the router when reporting progress.
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Failure reason set by [`StreamLease::fail`].
    fn fault(&self) -> Option<&str> {
        self.fault.get().map(String::as_str)
    }
}

/// How one inbound stream ended, reported by the transfer reader to the router.
#[derive(Debug)]
pub enum StreamOutcome {
    /// The agent sent the final data frame; `payload` is that frame's data (the exec terminal
    /// event for executions).
    Completed { payload: Vec<u8> },
    /// The agent sent an error frame.
    Failed { message: String },
    /// A credited agent sent more frames than it had credit for.
    Overflow,
}

/// Terminal event of one inbound stream; the only part of a stream the router sees.
#[derive(Debug)]
pub struct StreamEnd {
    /// Agent whose transfer socket carried the stream.
    pub(crate) agent_id: AgentId,
    /// Stream that ended.
    pub(crate) request_id: RequestId,
    /// Why the stream ended.
    pub(crate) outcome: StreamOutcome,
    /// Whether the terminal frame reached the consumer's queue; when it did not (stopped or
    /// overflowing stream), the router must deliver the outcome another way.
    pub(crate) delivered: bool,
    /// Payload bytes the reader enqueued for this stream, including frames the consumer has
    /// not drained yet, so completion can report the full size.
    pub(crate) received_bytes: u64,
}

/// One inbound stream as seen by the transfer reader.
#[derive(Debug)]
struct InboundSlot {
    sender: mpsc::Sender<StreamChunk>,
    cancel: CancellationToken,
    received: u64,
}

/// The stream tables of one control session, shared by the router, the control session (for
/// credit grants from the agent) and the transfer reader.
#[derive(Debug)]
struct StreamTables {
    agent_id: AgentId,
    credit: bool,
    inbound: Mutex<HashMap<RequestId, InboundSlot>>,
    outbound: Mutex<HashMap<RequestId, CreditGate>>,
}

/// Per-control-session routing for payload streams, used without the router actor.
///
/// Created by the control session at registration with the negotiated credit mode, stored in
/// the router's connection entry, and handed to the transfer socket when it authenticates.
#[derive(Clone, Debug)]
pub struct ConnectionStreams {
    tables: Arc<StreamTables>,
}

impl ConnectionStreams {
    /// Creates empty tables; `credit` is whether the agent advertised per-stream credit.
    pub fn new(agent_id: AgentId, credit: bool) -> Self {
        Self {
            tables: Arc::new(StreamTables {
                agent_id,
                credit,
                inbound: Mutex::new(HashMap::new()),
                outbound: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Whether streams of this session use per-stream credit.
    pub(crate) fn credit(&self) -> bool {
        self.tables.credit
    }

    /// Locks the inbound table, tolerating poison because every operation leaves it valid.
    fn inbound(&self) -> MutexGuard<'_, HashMap<RequestId, InboundSlot>> {
        self.tables
            .inbound
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Locks the outbound table, tolerating poison because every operation leaves it valid.
    fn outbound(&self) -> MutexGuard<'_, HashMap<RequestId, CreditGate>> {
        self.tables
            .outbound
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Opens an inbound stream and returns its consumer.
    ///
    /// Must happen before the command is sent so no frame can arrive unroutable. Credited
    /// streams get a queue of the window plus the terminal frame; others keep depth one.
    pub(crate) fn open_inbound(&self, lease: StreamLease) -> OutputReceiver {
        let capacity = if self.credit() {
            STREAM_CREDIT_QUEUE_FRAMES
        } else {
            UNCREDITED_QUEUE_FRAMES
        };
        let (sender, chunks) = mpsc::channel(capacity);
        self.inbound().insert(
            lease.id,
            InboundSlot {
                sender,
                cancel: lease.cancel.clone(),
                received: 0,
            },
        );
        OutputReceiver {
            chunks,
            lease,
            ledger: CreditLedger::default(),
            owed_credit: 0,
            fault_reported: false,
        }
    }

    /// Removes an inbound stream; its consumer sees the end after draining queued frames.
    pub(crate) fn close_inbound(&self, request_id: RequestId) {
        self.inbound().remove(&request_id);
    }

    /// Registers credit for an outbound stream; `None` when the agent does not grant credit.
    pub(crate) fn open_outbound(&self, request_id: RequestId) -> Option<CreditGate> {
        if !self.credit() {
            return None;
        }
        let gate = CreditGate::default();
        self.outbound().insert(request_id, gate.clone());
        Some(gate)
    }

    /// Removes an outbound stream's credit and releases a producer waiting for it.
    pub(crate) fn close_outbound(&self, request_id: RequestId) {
        if let Some(gate) = self.outbound().remove(&request_id) {
            gate.close();
        }
    }

    /// Applies credit returned by the agent; grants for finished streams are ignored.
    pub fn grant_outbound(&self, request_id: RequestId, frames: u32) {
        if let Some(gate) = self.outbound().get(&request_id) {
            gate.grant(frames);
        }
    }

    /// Ends every stream of this session because its control or transfer link is gone.
    pub(crate) fn clear(&self) {
        self.inbound().clear();
        for (_, gate) in self.outbound().drain() {
            gate.close();
        }
    }

    /// Hands one frame from the transfer socket to its stream's consumer.
    ///
    /// Credited streams are filled with `try_send` and never make the reader wait; a full
    /// queue means the agent ignored its credit, which fails that stream only. Uncredited
    /// streams wait for their consumer like before (raced against cancellation). Frames of
    /// stopped streams are discarded, but their terminal frame is still reported because it
    /// is the agent's acknowledgement that the worker finished. Returns the terminal event the
    /// router must settle, if any.
    pub(crate) async fn deliver(&self, chunk: StreamChunk) -> Option<StreamEnd> {
        let request_id = chunk.request_id;
        let terminal = !chunk.consumes_credit();
        let (sender, cancel, received) = {
            let mut inbound = self.inbound();
            let slot = inbound.get_mut(&request_id)?;
            if !chunk.is_error {
                slot.received = slot.received.saturating_add(chunk.data.len() as u64);
            }
            let route = (slot.sender.clone(), slot.cancel.clone(), slot.received);
            if terminal {
                // Nothing may follow a terminal frame, so the stream stops being routable.
                inbound.remove(&request_id);
            }
            route
        };
        let outcome = terminal.then(|| Self::outcome(&chunk));
        let delivered = if cancel.is_cancelled() {
            false
        } else if self.credit() {
            match sender.try_send(chunk) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.close_inbound(request_id);
                    return Some(self.end(request_id, StreamOutcome::Overflow, false, received));
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            }
        } else {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => false,
                result = sender.send(chunk) => result.is_ok(),
            }
        };
        outcome.map(|outcome| self.end(request_id, outcome, delivered, received))
    }

    /// Describes a terminal frame before it is moved into the consumer's queue.
    fn outcome(chunk: &StreamChunk) -> StreamOutcome {
        if chunk.is_error {
            StreamOutcome::Failed {
                message: String::from_utf8_lossy(&chunk.data).into_owned(),
            }
        } else {
            StreamOutcome::Completed {
                payload: chunk.data.clone(),
            }
        }
    }

    /// Builds the terminal event for one of this session's streams.
    fn end(
        &self,
        request_id: RequestId,
        outcome: StreamOutcome,
        delivered: bool,
        received_bytes: u64,
    ) -> StreamEnd {
        StreamEnd {
            agent_id: self.tables.agent_id.clone(),
            request_id,
            outcome,
            delivered,
            received_bytes,
        }
    }
}

/// Consumer end of one inbound stream (download or exec output, or a copy source).
///
/// Counts drained bytes for progress and, on credited sessions, returns credit as frames are
/// drained, so the agent can only run ahead of this consumer by the credit window.
#[derive(Debug)]
pub struct OutputReceiver {
    chunks: mpsc::Receiver<StreamChunk>,
    lease: StreamLease,
    ledger: CreditLedger,
    /// Credit earned by drained frames but not yet returned to the agent.
    owed_credit: u32,
    fault_reported: bool,
}

impl OutputReceiver {
    /// Request id of the stream, used by consumers to arm cancellation.
    pub fn request_id(&self) -> RequestId {
        self.lease.id
    }

    /// Receives the next frame, or `None` when the stream ended or was stopped.
    ///
    /// Already queued frames are drained before a stop is observed, so output accepted before
    /// a cancellation is not lost. A stream the router failed yields one error frame carrying
    /// the reason before ending.
    ///
    /// Cancel safe: credit owed for drained frames is returned before waiting for the next
    /// frame, and nothing is awaited after a frame was dequeued, so dropping this future (for
    /// example in a `select!`) never loses a frame or a grant.
    pub async fn recv(&mut self) -> Option<StreamChunk> {
        if self.owed_credit > 0 {
            self.lease.link.grant(self.lease.id, self.owed_credit).await;
            self.owed_credit = 0;
        }
        let chunk = tokio::select! {
            biased;
            chunk = self.chunks.recv() => chunk,
            _ = self.lease.cancel.cancelled() => None,
        };
        let Some(chunk) = chunk else {
            return self.fault_chunk();
        };
        if !chunk.is_error {
            self.lease
                .bytes
                .fetch_add(chunk.data.len() as u64, Ordering::Relaxed);
        }
        if self.lease.link.credit
            && let Some(frames) = self.ledger.drained(chunk.consumes_credit())
        {
            self.owed_credit += frames;
        }
        Some(chunk)
    }

    /// Converts a router-reported failure into one terminal error frame.
    fn fault_chunk(&mut self) -> Option<StreamChunk> {
        if self.fault_reported {
            return None;
        }
        let message = self.lease.fault()?.to_string();
        self.fault_reported = true;
        Some(StreamChunk {
            request_id: self.lease.id,
            chunk_index: ChunkIndex::new(0),
            is_last: true,
            is_error: true,
            payload_kind: StreamPayloadKind::RawFile,
            data: message.into_bytes(),
        })
    }
}

#[cfg(test)]
impl ConnectionStreams {
    /// Opens an inbound stream on a detached link so consumer tests need no router.
    pub(crate) fn open_detached(&self, request_id: RequestId) -> OutputReceiver {
        let link = AgentLink::new(mpsc::channel(1).0, None, false);
        self.open_inbound(StreamLease::new(request_id, link, 0))
    }
}

/// Why an outbound frame was not sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboundError {
    /// The router stopped the stream (cancellation, teardown or completion).
    Stopped,
    /// The agent's transfer socket is gone.
    LaneClosed,
}

/// Producer end of one server-to-agent stream (upload, edit or copy destination).
///
/// Sends directly onto the agent's transfer lane, spending per-stream credit on credited
/// sessions so a slow destination worker stalls only this stream.
#[derive(Clone, Debug)]
pub struct OutboundStream {
    lease: StreamLease,
    transfer: Option<mpsc::Sender<WsMessage>>,
    credit: Option<CreditGate>,
}

impl OutboundStream {
    /// Binds a lease to the agent's transfer lane and optional credit gate.
    pub(crate) fn new(lease: StreamLease, credit: Option<CreditGate>) -> Self {
        let transfer = lease.link.transfer.clone();
        Self {
            lease,
            transfer,
            credit,
        }
    }

    /// Request id of the stream, used by producers to arm cancellation.
    pub fn request_id(&self) -> RequestId {
        self.lease.id
    }

    /// The lease shared with the router.
    pub(crate) fn lease(&self) -> &StreamLease {
        &self.lease
    }

    /// Reframes one logical payload and sends its frames in order.
    ///
    /// `chunk_index` is advanced past every frame attempted so a later abort frame never
    /// reuses an index.
    pub(crate) async fn send_frames(
        &self,
        chunk_index: &mut ChunkIndex,
        request: StreamChunkFrameRequest<'_>,
    ) -> Result<(), OutboundError> {
        let mut frames = StreamChunkFrames::new(request.starting_chunk_index(*chunk_index));
        while let Some(chunk) = frames.next() {
            *chunk_index = frames.next_chunk_index();
            self.send(chunk).await?;
        }
        *chunk_index = frames.next_chunk_index();
        Ok(())
    }

    /// Sends one frame, waiting for credit (non-terminal frames) and lane capacity while
    /// staying responsive to the router stopping the stream.
    pub(crate) async fn send(&self, chunk: StreamChunk) -> Result<(), OutboundError> {
        let cancel = &self.lease.cancel;
        if cancel.is_cancelled() {
            return Err(OutboundError::Stopped);
        }
        if let Some(gate) = &self.credit
            && chunk.consumes_credit()
        {
            let acquired = tokio::select! {
                biased;
                _ = cancel.cancelled() => false,
                acquired = gate.acquire() => acquired,
            };
            if !acquired {
                return Err(OutboundError::Stopped);
            }
        }
        let bytes = if chunk.is_error {
            0
        } else {
            chunk.data.len() as u64
        };
        let transfer = self.transfer.as_ref().ok_or(OutboundError::LaneClosed)?;
        let frame = WsMessage::Binary(chunk.to_bytes().into());
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(OutboundError::Stopped),
            result = transfer.send(frame) => result.map_err(|_| OutboundError::LaneClosed)?,
        }
        self.lease.bytes.fetch_add(bytes, Ordering::Relaxed);
        Ok(())
    }

    /// Sends a terminal error frame even after the stream was stopped, so the destination
    /// worker fails promptly instead of waiting for frames that will never come.
    pub(crate) async fn send_abort(
        &self,
        chunk_index: ChunkIndex,
        payload_kind: StreamPayloadKind,
        reason: &str,
    ) {
        let Some(transfer) = &self.transfer else {
            return;
        };
        let request = StreamChunkFrameRequest::new(self.lease.id, reason.as_bytes())
            .starting_chunk_index(chunk_index)
            .payload_kind(payload_kind)
            .is_error(true);
        if let Some(chunk) = StreamChunkFrames::new(request).next() {
            let _ = transfer
                .send(WsMessage::Binary(chunk.to_bytes().into()))
                .await;
        }
    }

    /// Failure reason set by the router, if it failed rather than merely stopped the stream.
    pub(crate) fn fault(&self) -> Option<&str> {
        self.lease.fault()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redoor_protocol::wire::stream::STREAM_CREDIT_WINDOW_FRAMES;

    /// Builds a session's tables plus the priority lane its credit grants use.
    fn session(credit: bool) -> (ConnectionStreams, AgentLink, mpsc::Receiver<WsMessage>) {
        let (priority, grants) = mpsc::channel(16);
        let agent = AgentId::from("agent");
        (
            ConnectionStreams::new(agent.clone(), credit),
            AgentLink::new(priority, None, credit),
            grants,
        )
    }

    /// Builds one data or terminal frame of `request_id`.
    fn frame(request_id: u64, is_last: bool) -> StreamChunk {
        StreamChunk {
            request_id: RequestId::new(request_id),
            chunk_index: ChunkIndex::new(0),
            is_last,
            is_error: false,
            payload_kind: StreamPayloadKind::RawFile,
            data: vec![7; 16],
        }
    }

    /// A stalled consumer must not stall the reader: other streams keep flowing, and a frame
    /// beyond the credit window fails only the violating stream.
    #[tokio::test]
    async fn stalled_consumer_does_not_block_other_streams() {
        let (streams, link, _grants) = session(true);
        let _stalled = streams.open_inbound(StreamLease::new(RequestId::new(1), link.clone(), 0));
        let mut live = streams.open_inbound(StreamLease::new(RequestId::new(2), link, 0));
        for _ in 0..STREAM_CREDIT_WINDOW_FRAMES {
            // Frames within the window are queued without waiting for the consumer.
            assert!(streams.deliver(frame(1, false)).await.is_none());
        }
        // The terminal frame has its reserved slot even with the window full.
        let terminal = streams.deliver(frame(1, true)).await.unwrap();
        assert!(terminal.delivered);
        // The reader returned for every frame above, so another stream is still served.
        assert!(streams.deliver(frame(2, false)).await.is_none());
        assert_eq!(live.recv().await.unwrap().request_id, RequestId::new(2));
    }

    /// An agent ignoring its credit fails only its own stream.
    #[tokio::test]
    async fn credit_overflow_fails_only_the_violating_stream() {
        let (streams, link, _grants) = session(true);
        let _stalled = streams.open_inbound(StreamLease::new(RequestId::new(1), link, 0));
        for _ in 0..STREAM_CREDIT_WINDOW_FRAMES + 1 {
            assert!(streams.deliver(frame(1, false)).await.is_none());
        }
        let end = streams.deliver(frame(1, false)).await.unwrap();
        // The violation is reported to the router as this stream's terminal event.
        assert!(matches!(end.outcome, StreamOutcome::Overflow));
        // The violating stream is no longer routed, so later frames are ignored.
        assert!(streams.deliver(frame(1, true)).await.is_none());
    }

    /// Draining returns credit in batches on the priority lane and counts drained bytes.
    #[tokio::test]
    async fn draining_grants_credit_and_counts_bytes() {
        let (streams, link, mut grants) = session(true);
        let lease = StreamLease::new(RequestId::new(3), link, 0);
        let mut receiver = streams.open_inbound(lease.clone());
        streams.deliver(frame(3, false)).await;
        streams.deliver(frame(3, false)).await;
        receiver.recv().await.unwrap();
        receiver.recv().await.unwrap();
        // The batch is returned when the consumer asks for more, before it waits again.
        assert!(grants.try_recv().is_err());
        let next = tokio::spawn(async move { receiver.recv().await });
        let WsMessage::Text(grant) = grants.recv().await.unwrap() else {
            panic!("credit grants are control text frames");
        };
        // Two drained frames complete one batch, returned for this stream on the control lane.
        assert_eq!(
            grant.as_str(),
            r#"{"type":"stream_credit","request_id":3,"frames":2}"#
        );
        // Progress counts what the consumer drained, not what the reader merely queued.
        assert_eq!(lease.bytes(), 32);
        next.abort();
    }

    /// Stopped streams discard payload but still report the agent's terminal acknowledgement.
    #[tokio::test]
    async fn stopped_stream_discards_payload_and_reports_terminal() {
        let (streams, link, _grants) = session(true);
        let lease = StreamLease::new(RequestId::new(4), link, 0);
        let mut receiver = streams.open_inbound(lease.clone());
        lease.stop();
        assert!(streams.deliver(frame(4, false)).await.is_none());
        let end = streams.deliver(frame(4, true)).await.unwrap();
        // The router learns the worker finished even though nobody consumes the output.
        assert!(!end.delivered);
        // The consumer sees the end instead of the discarded frames.
        assert!(receiver.recv().await.is_none());
    }

    /// A failed stream reports its reason once to the consumer.
    #[tokio::test]
    async fn failed_stream_yields_reason_once() {
        let (streams, link, _grants) = session(false);
        let lease = StreamLease::new(RequestId::new(5), link, 0);
        let mut receiver = streams.open_inbound(lease.clone());
        lease.fail("Agent disconnected".to_string());
        let chunk = receiver.recv().await.unwrap();
        // Consumers need a real error, not a silently truncated body.
        assert!(chunk.is_error && chunk.data == b"Agent disconnected");
        assert!(receiver.recv().await.is_none());
    }

    /// Uncredited agents keep the depth-one hand-off where the reader waits for the consumer.
    #[tokio::test]
    async fn uncredited_stream_waits_for_consumer() {
        let (streams, link, _grants) = session(false);
        let mut receiver = streams.open_inbound(StreamLease::new(RequestId::new(6), link, 0));
        streams.deliver(frame(6, false)).await;
        let waiting = tokio::spawn({
            let streams = streams.clone();
            async move { streams.deliver(frame(6, true)).await }
        });
        tokio::task::yield_now().await;
        // The old behavior is preserved for agents without credit: the reader waits.
        assert!(!waiting.is_finished());
        receiver.recv().await.unwrap();
        let end = waiting.await.unwrap().unwrap();
        assert!(end.delivered);
    }

    /// Outbound frames spend credit, terminal frames do not, and the router can stop a
    /// producer waiting for credit.
    #[tokio::test]
    async fn outbound_stream_spends_credit_and_stops_promptly() {
        let streams = ConnectionStreams::new(AgentId::from("agent"), true);
        let (priority, _priority_rx) = mpsc::channel(1);
        let (transfer, mut lane) = mpsc::channel(64);
        let link = AgentLink::new(priority, Some(transfer), true);
        let lease = StreamLease::new(RequestId::new(7), link, 0);
        let outbound = OutboundStream::new(lease.clone(), streams.open_outbound(lease.id));
        for _ in 0..STREAM_CREDIT_WINDOW_FRAMES {
            outbound.send(frame(7, false)).await.unwrap();
        }
        // The terminal frame never waits for credit.
        outbound.send(frame(7, true)).await.unwrap();
        let blocked = tokio::spawn({
            let outbound = outbound.clone();
            async move { outbound.send(frame(7, false)).await }
        });
        tokio::task::yield_now().await;
        // With the window spent, the producer waits for the agent's grant.
        assert!(!blocked.is_finished());
        streams.grant_outbound(RequestId::new(7), 1);
        assert_eq!(blocked.await.unwrap(), Ok(()));
        let blocked = tokio::spawn({
            let outbound = outbound.clone();
            async move { outbound.send(frame(7, false)).await }
        });
        lease.stop();
        // Cancellation releases a producer stuck on credit instead of leaking it.
        assert_eq!(blocked.await.unwrap(), Err(OutboundError::Stopped));
        let mut sent = 0;
        while lane.try_recv().is_ok() {
            sent += 1;
        }
        assert_eq!(sent, STREAM_CREDIT_WINDOW_FRAMES as usize + 2);
        // Outbound progress counts bytes the lane accepted.
        assert_eq!(lease.bytes(), 16 * (STREAM_CREDIT_WINDOW_FRAMES as u64 + 2));
    }
}
