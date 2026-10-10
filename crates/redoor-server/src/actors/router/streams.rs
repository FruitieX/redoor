//! The router-free data path: stream leases, per-connection stream tables and consumers.
//!
//! The router admits streams, tracks their lifecycle and handles cancellation, but payload
//! frames never pass through its mailbox. They travel directly between the connection's
//! session and the HTTP, copy, terminal or log consumers through the types here, so a 64 KiB
//! chunk costs no router messages, and only terminal events (completion, failure, protocol
//! violation) go back to the router. Every stream is credited, so the connection reader never
//! waits for a consumer and one stalled client cannot freeze the other streams of its agent.

use crate::actors::lanes::ControlLanes;
use redoor_core::stream_credit::{CreditGate, CreditLedger};
use redoor_protocol::{
    ids::{AgentId, RequestId},
    wire::{
        ServerToAgent,
        stream::{StreamChunk, receive_queue_frames},
        terminal::TerminalServerMessage,
    },
};
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex, MutexGuard, OnceLock, PoisonError,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// What a stream needs to reach its agent without asking the router.
#[derive(Clone, Debug)]
pub struct AgentLink {
    /// Lanes of the connection; credit grants use the priority lane so they never queue
    /// behind commands or data.
    control: ControlLanes,
}

impl AgentLink {
    /// Captures the lanes of one connection.
    pub(crate) fn new(control: ControlLanes) -> Self {
        Self { control }
    }

    /// The connection's lanes, for stream-scoped control frames (resizes, cancels).
    pub(crate) fn lanes(&self) -> &ControlLanes {
        &self.control
    }

    /// Returns credit for frames a consumer drained, waiting for priority-lane capacity so a
    /// grant is never lost (a lost grant would shrink the window for the rest of the stream).
    async fn grant(&self, request_id: RequestId, frames: u32) {
        let _ = self
            .control
            .send_priority(ServerToAgent::Credit {
                stream: request_id,
                frames,
            })
            .await;
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
    /// The agent sent more frames than it had credit for.
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

/// One inbound stream as seen by the connection reader.
#[derive(Debug)]
struct InboundSlot {
    sender: mpsc::Sender<StreamChunk>,
    cancel: CancellationToken,
    received: u64,
}

/// The stream tables of one connection, shared by the router and the connection's session.
#[derive(Debug)]
struct StreamTables {
    agent_id: AgentId,
    inbound: Mutex<HashMap<RequestId, InboundSlot>>,
    outbound: Mutex<HashMap<RequestId, CreditGate>>,
    /// Lifecycle events of open terminals, delivered in their stream's frame order.
    terminal_events: Mutex<HashMap<RequestId, mpsc::Sender<TerminalServerMessage>>>,
}

/// Pending terminal lifecycle events per terminal; there are at most three (ready, exit or
/// error), so the queue never fills for an honest agent.
const TERMINAL_EVENT_QUEUE: usize = 4;

/// Per-control-session routing for payload streams, used without the router actor.
///
/// Created by the session at registration and stored in the router's connection entry.
#[derive(Clone, Debug)]
pub struct ConnectionStreams {
    tables: Arc<StreamTables>,
}

impl ConnectionStreams {
    /// Creates empty tables for one agent connection.
    pub fn new(agent_id: AgentId) -> Self {
        Self {
            tables: Arc::new(StreamTables {
                agent_id,
                inbound: Mutex::new(HashMap::new()),
                outbound: Mutex::new(HashMap::new()),
                terminal_events: Mutex::new(HashMap::new()),
            }),
        }
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

    /// Opens an inbound stream with `window` frames of credit and returns its consumer.
    ///
    /// Must happen before the command is sent so no frame can arrive unroutable. The queue
    /// holds the credit window plus the terminal frame, so an honest agent never overflows it.
    pub(crate) fn open_inbound(&self, lease: StreamLease, window: u32) -> OutputReceiver {
        let (sender, chunks) = mpsc::channel(receive_queue_frames(window));
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

    /// Registers credit for an outbound stream with `window` frames.
    pub(crate) fn open_outbound(&self, request_id: RequestId, window: u32) -> CreditGate {
        let gate = CreditGate::with_window(window);
        self.outbound().insert(request_id, gate.clone());
        gate
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

    /// Opens the lifecycle event queue of terminal `request_id`.
    pub(crate) fn open_terminal_events(
        &self,
        request_id: RequestId,
    ) -> mpsc::Receiver<TerminalServerMessage> {
        let (sender, events) = mpsc::channel(TERMINAL_EVENT_QUEUE);
        self.terminal_events().insert(request_id, sender);
        events
    }

    /// Delivers one terminal lifecycle event; events of unknown or closed terminals are
    /// dropped, and a flooding agent cannot make the reader wait.
    pub fn deliver_terminal_event(&self, request_id: RequestId, event: TerminalServerMessage) {
        if let Some(events) = self.terminal_events().get(&request_id) {
            let _ = events.try_send(event);
        }
    }

    /// Forgets every table entry of one stream (terminals and log streams end this way).
    pub(crate) fn close(&self, request_id: RequestId) {
        self.close_inbound(request_id);
        self.close_outbound(request_id);
        self.terminal_events().remove(&request_id);
    }

    /// Locks the terminal event table, tolerating poison because every operation leaves it
    /// valid.
    fn terminal_events(
        &self,
    ) -> MutexGuard<'_, HashMap<RequestId, mpsc::Sender<TerminalServerMessage>>> {
        self.tables
            .terminal_events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Ends every stream of this connection because it is gone.
    pub(crate) fn clear(&self) {
        self.inbound().clear();
        for (_, gate) in self.outbound().drain() {
            gate.close();
        }
        self.terminal_events().clear();
    }

    /// Hands one frame from the connection to its stream's consumer.
    ///
    /// Streams are filled with `try_send` and never make the reader wait; a full queue means
    /// the agent ignored its credit, which fails that stream only. Frames of stopped streams
    /// are discarded, but their terminal frame is still reported because it is the agent's
    /// acknowledgement that the worker finished. Returns the terminal event the router must
    /// settle, if any.
    pub(crate) fn deliver(&self, chunk: StreamChunk) -> Option<StreamEnd> {
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
        } else {
            match sender.try_send(chunk) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.close_inbound(request_id);
                    return Some(self.end(request_id, StreamOutcome::Overflow, false, received));
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
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
/// Counts drained bytes for progress and returns credit as frames are drained, so the agent
/// can only run ahead of this consumer by the credit window.
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
        if let Some(frames) = self.ledger.drained(chunk.consumes_credit()) {
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
        Some(StreamChunk::error(self.lease.id, &message))
    }

    /// Takes one already queued frame without waiting and without returning credit; used to
    /// drain output that precedes a terminal's exit event.
    pub(crate) fn try_recv(&mut self) -> Option<StreamChunk> {
        self.chunks.try_recv().ok()
    }
}

#[cfg(test)]
impl ConnectionStreams {
    /// Opens an inbound stream on a detached link so consumer tests need no router.
    pub(crate) fn open_detached(&self, request_id: RequestId) -> OutputReceiver {
        let link = AgentLink::new(ControlLanes::bounded(1, 1).0);
        self.open_inbound(
            StreamLease::new(request_id, link, 0),
            redoor_protocol::wire::stream::STREAM_CREDIT_WINDOW_FRAMES,
        )
    }
}

/// Why an outbound frame was not sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboundError {
    /// The router stopped the stream (cancellation, teardown or completion).
    Stopped,
    /// The agent's connection is gone.
    LaneClosed,
}

/// Producer end of one server-to-agent stream (upload, edit, copy destination or terminal
/// input).
///
/// Sends directly onto the connection's bulk or interactive lane, spending per-stream credit
/// so a slow destination worker stalls only this stream.
#[derive(Clone, Debug)]
pub struct OutboundStream {
    lease: StreamLease,
    lane: mpsc::Sender<StreamChunk>,
    credit: CreditGate,
}

impl OutboundStream {
    /// Binds a lease to its lane class and the stream's credit gate.
    pub(crate) fn new(lease: StreamLease, credit: CreditGate, interactive: bool) -> Self {
        let lane = lease.link.control.data(interactive).clone();
        Self {
            lease,
            lane,
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

    /// Splits one logical payload into frames and sends them in order.
    pub(crate) async fn send_data(&self, data: &[u8], is_last: bool) -> Result<(), OutboundError> {
        for chunk in StreamChunk::split(self.lease.id, data, is_last) {
            self.send(chunk).await?;
        }
        Ok(())
    }

    /// Sends one frame, waiting for credit (non-terminal frames) and lane capacity while
    /// staying responsive to the router stopping the stream.
    pub(crate) async fn send(&self, chunk: StreamChunk) -> Result<(), OutboundError> {
        let cancel = &self.lease.cancel;
        if cancel.is_cancelled() {
            return Err(OutboundError::Stopped);
        }
        if chunk.consumes_credit() {
            let acquired = tokio::select! {
                biased;
                _ = cancel.cancelled() => false,
                acquired = self.credit.acquire() => acquired,
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
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(OutboundError::Stopped),
            result = self.lane.send(chunk) => result.map_err(|_| OutboundError::LaneClosed)?,
        }
        self.lease.bytes.fetch_add(bytes, Ordering::Relaxed);
        Ok(())
    }

    /// Sends a terminal error frame even after the stream was stopped, so the destination
    /// worker fails promptly instead of waiting for frames that will never come.
    pub(crate) async fn send_abort(&self, reason: &str) {
        let _ = self
            .lane
            .send(StreamChunk::error(self.lease.id, reason))
            .await;
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
    fn session() -> (ConnectionStreams, AgentLink, mpsc::Receiver<ServerToAgent>) {
        let (lanes, receivers) = ControlLanes::bounded(16, 16);
        let agent = AgentId::from("agent");
        (
            ConnectionStreams::new(agent.clone()),
            AgentLink::new(lanes),
            receivers.priority,
        )
    }

    /// Builds one data or terminal frame of `request_id`.
    fn frame(request_id: u64, is_last: bool) -> StreamChunk {
        StreamChunk::data(RequestId::new(request_id), vec![7; 16], is_last)
    }

    /// A stalled consumer must not stall the reader: other streams keep flowing, and a frame
    /// beyond the credit window fails only the violating stream.
    #[tokio::test]
    async fn stalled_consumer_does_not_block_other_streams() {
        let (streams, link, _grants) = session();
        let _stalled = streams.open_inbound(
            StreamLease::new(RequestId::new(1), link.clone(), 0),
            STREAM_CREDIT_WINDOW_FRAMES,
        );
        let mut live = streams.open_inbound(
            StreamLease::new(RequestId::new(2), link, 0),
            STREAM_CREDIT_WINDOW_FRAMES,
        );
        for _ in 0..STREAM_CREDIT_WINDOW_FRAMES {
            // Frames within the window are queued without waiting for the consumer.
            assert!(streams.deliver(frame(1, false)).is_none());
        }
        // The terminal frame has its reserved slot even with the window full.
        let terminal = streams.deliver(frame(1, true)).unwrap();
        assert!(terminal.delivered);
        // The reader returned for every frame above, so another stream is still served.
        assert!(streams.deliver(frame(2, false)).is_none());
        assert_eq!(live.recv().await.unwrap().request_id, RequestId::new(2));
    }

    /// An agent ignoring its credit fails only its own stream.
    #[tokio::test]
    async fn credit_overflow_fails_only_the_violating_stream() {
        let (streams, link, _grants) = session();
        let _stalled = streams.open_inbound(
            StreamLease::new(RequestId::new(1), link, 0),
            STREAM_CREDIT_WINDOW_FRAMES,
        );
        for _ in 0..STREAM_CREDIT_WINDOW_FRAMES + 1 {
            assert!(streams.deliver(frame(1, false)).is_none());
        }
        let end = streams.deliver(frame(1, false)).unwrap();
        // The violation is reported to the router as this stream's terminal event.
        assert!(matches!(end.outcome, StreamOutcome::Overflow));
        // The violating stream is no longer routed, so later frames are ignored.
        assert!(streams.deliver(frame(1, true)).is_none());
    }

    /// Draining returns credit in batches on the priority lane and counts drained bytes.
    #[tokio::test]
    async fn draining_grants_credit_and_counts_bytes() {
        let (streams, link, mut grants) = session();
        let lease = StreamLease::new(RequestId::new(3), link, 0);
        let mut receiver = streams.open_inbound(lease.clone(), STREAM_CREDIT_WINDOW_FRAMES);
        streams.deliver(frame(3, false));
        streams.deliver(frame(3, false));
        receiver.recv().await.unwrap();
        receiver.recv().await.unwrap();
        // The batch is returned when the consumer asks for more, before it waits again.
        assert!(grants.try_recv().is_err());
        let next = tokio::spawn(async move { receiver.recv().await });
        // Two drained frames complete one batch, returned for this stream on the control lane.
        assert!(matches!(
            grants.recv().await,
            Some(ServerToAgent::Credit { stream, frames: 2 }) if stream == RequestId::new(3)
        ));
        // Progress counts what the consumer drained, not what the reader merely queued.
        assert_eq!(lease.bytes(), 32);
        next.abort();
    }

    /// Stopped streams discard payload but still report the agent's terminal acknowledgement.
    #[tokio::test]
    async fn stopped_stream_discards_payload_and_reports_terminal() {
        let (streams, link, _grants) = session();
        let lease = StreamLease::new(RequestId::new(4), link, 0);
        let mut receiver = streams.open_inbound(lease.clone(), STREAM_CREDIT_WINDOW_FRAMES);
        lease.stop();
        assert!(streams.deliver(frame(4, false)).is_none());
        let end = streams.deliver(frame(4, true)).unwrap();
        // The router learns the worker finished even though nobody consumes the output.
        assert!(!end.delivered);
        // The consumer sees the end instead of the discarded frames.
        assert!(receiver.recv().await.is_none());
    }

    /// A failed stream reports its reason once to the consumer.
    #[tokio::test]
    async fn failed_stream_yields_reason_once() {
        let (streams, link, _grants) = session();
        let lease = StreamLease::new(RequestId::new(5), link, 0);
        let mut receiver = streams.open_inbound(lease.clone(), STREAM_CREDIT_WINDOW_FRAMES);
        lease.fail("Agent disconnected".to_string());
        let chunk = receiver.recv().await.unwrap();
        // Consumers need a real error, not a silently truncated body.
        assert!(chunk.is_error && chunk.data == b"Agent disconnected");
        assert!(receiver.recv().await.is_none());
    }

    /// Outbound frames spend credit, terminal frames do not, and the router can stop a
    /// producer waiting for credit.
    #[tokio::test]
    async fn outbound_stream_spends_credit_and_stops_promptly() {
        let streams = ConnectionStreams::new(AgentId::from("agent"));
        let (lanes, mut receivers) = ControlLanes::bounded(1, 1);
        let lease = StreamLease::new(RequestId::new(7), AgentLink::new(lanes), 0);
        let outbound = OutboundStream::new(
            lease.clone(),
            streams.open_outbound(lease.id, STREAM_CREDIT_WINDOW_FRAMES),
            false,
        );
        // Drains the one-slot bulk lane in the background so only credit limits the producer.
        let drain = tokio::spawn(async move { while receivers.bulk.recv().await.is_some() {} });
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
        // Outbound progress counts bytes the lane accepted: the window, FIN and the granted
        // frame, but not the canceled one.
        assert_eq!(lease.bytes(), 16 * (STREAM_CREDIT_WINDOW_FRAMES as u64 + 2));
        drain.abort();
    }
}
