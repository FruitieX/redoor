//! The agent's single multiplexed connection to its server, with a generation number.
//!
//! Generations let delayed events from a replaced socket be recognized and ignored, so a stale
//! "connection lost" can never tear down the connection that replaced it.

use super::responder::ControlSender;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Queue depth of the control lane; full queues backpressure responders instead of dropping.
pub(crate) const CONTROL_QUEUE_CAPACITY: usize = 32;
/// Queue depth of the interactive (terminal) lane.
pub(crate) const INTERACTIVE_QUEUE_CAPACITY: usize = 8;
/// Queue depth of the bulk lane.
///
/// One slot means producers wait in the lane's fair queue, so streams with data and credit
/// take turns frame by frame, and a control frame waits behind at most one bulk frame.
pub(crate) const BULK_QUEUE_CAPACITY: usize = 1;

/// The three outbound classes of one connection, drained by one writer in strict order.
///
/// Control frames (results, credit, progress) go first, then interactive terminal frames,
/// then bulk stream data. Within a lane frames keep their order, so a frame queued after a
/// stream's data on the same lane is always written after that data.
#[derive(Clone)]
pub(crate) struct ConnectionLanes {
    pub(crate) control: ControlSender,
    pub(crate) interactive: ControlSender,
    pub(crate) bulk: ControlSender,
}

/// The connection carrying every control frame and stream of this agent.
pub(crate) struct ControlLink {
    generation: u64,
    lanes: Option<ConnectionLanes>,
    /// Stops both socket halves when liveness checks or teardown discard the connection.
    shutdown: Option<watch::Sender<bool>>,
    /// Parent of every cancellation token handed out while this connection is current.
    scope: CancellationToken,
}

impl ControlLink {
    /// Starts disconnected with an already-cancelled scope so nothing can run before connect.
    pub(crate) fn new() -> Self {
        let scope = CancellationToken::new();
        scope.cancel();
        Self {
            generation: 0,
            lanes: None,
            shutdown: None,
            scope,
        }
    }

    /// Installs a freshly connected socket and returns its generation.
    pub(crate) fn attach(&mut self, lanes: ConnectionLanes, shutdown: watch::Sender<bool>) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.lanes = Some(lanes);
        self.shutdown = Some(shutdown);
        self.scope = CancellationToken::new();
        self.generation
    }

    /// Stops the socket and cancels everything scoped to this connection.
    pub(crate) fn invalidate(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(true);
        }
        self.lanes = None;
        self.scope.cancel();
    }

    /// Whether an event from `generation` still refers to this connection.
    pub(crate) fn is_current(&self, generation: u64) -> bool {
        self.generation == generation
    }

    /// Current generation, captured by tasks that report back later.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Lanes of the current connection, if connected.
    pub(crate) fn lanes(&self) -> Option<&ConnectionLanes> {
        self.lanes.as_ref()
    }

    /// Control lane of the current connection, if connected.
    pub(crate) fn sender(&self) -> Option<&ControlSender> {
        self.lanes.as_ref().map(|lanes| &lanes.control)
    }

    /// Cancellation scope of the current connection.
    pub(crate) fn scope(&self) -> &CancellationToken {
        &self.scope
    }
}
