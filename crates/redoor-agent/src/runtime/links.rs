//! The two websocket links the agent keeps to its server, each with its own generation.
//!
//! Generations let delayed events from a replaced socket be recognized and ignored, so a stale
//! "connection lost" can never tear down the connection that replaced it.

use super::responder::ControlSender;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tokio_util::sync::CancellationToken;

/// The control socket carrying commands, responses and lifecycle traffic.
pub(crate) struct ControlLink {
    generation: u64,
    sender: Option<ControlSender>,
    /// Stops both socket halves when liveness checks or teardown discard the connection.
    shutdown: Option<watch::Sender<bool>>,
    /// Parent of every cancellation token handed out while this connection is current.
    scope: CancellationToken,
    /// Whether the server confirmed per-stream credit for this connection.
    ///
    /// Negotiated per control session because a reconnect may reach a different server build.
    stream_credit: bool,
}

impl ControlLink {
    /// Starts disconnected with an already-cancelled scope so nothing can run before connect.
    pub(crate) fn new() -> Self {
        let scope = CancellationToken::new();
        scope.cancel();
        Self {
            generation: 0,
            sender: None,
            shutdown: None,
            scope,
            stream_credit: false,
        }
    }

    /// Installs a freshly connected socket and returns its generation.
    pub(crate) fn attach(&mut self, sender: ControlSender, shutdown: watch::Sender<bool>) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.sender = Some(sender);
        self.shutdown = Some(shutdown);
        self.scope = CancellationToken::new();
        self.stream_credit = false;
        self.generation
    }

    /// Records the server's credit confirmation for the current connection.
    pub(crate) fn confirm_stream_credit(&mut self, enabled: bool) {
        self.stream_credit = enabled;
    }

    /// Whether streams of the current connection use per-stream credit.
    pub(crate) fn stream_credit(&self) -> bool {
        self.stream_credit
    }

    /// Stops the socket and cancels everything scoped to this connection.
    pub(crate) fn invalidate(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(true);
        }
        self.sender = None;
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

    /// Writer for the current connection, if connected.
    pub(crate) fn sender(&self) -> Option<&ControlSender> {
        self.sender.as_ref()
    }

    /// Cancellation scope of the current connection.
    pub(crate) fn scope(&self) -> &CancellationToken {
        &self.scope
    }
}

/// The payload-only transfer socket that carries framed stream bytes in both directions.
pub(crate) struct TransferLink {
    generation: u64,
    sender: Option<mpsc::Sender<WsMessage>>,
    /// Session secret issued by the current control connection; reused for reconnects.
    token: Option<String>,
    shutdown: Option<watch::Sender<bool>>,
}

impl TransferLink {
    /// Starts without a socket or credentials.
    pub(crate) fn new() -> Self {
        Self {
            generation: 0,
            sender: None,
            token: None,
            shutdown: None,
        }
    }

    /// Replaces credentials and stops any current socket; returns the new attempt's generation.
    pub(crate) fn begin(&mut self, token: String) -> u64 {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(true);
        }
        self.sender = None;
        self.token = Some(token);
        self.advance()
    }

    /// Whether a connect attempt is still the authoritative one for these credentials.
    pub(crate) fn is_current(&self, generation: u64, token: &str) -> bool {
        self.generation == generation && self.token.as_deref() == Some(token)
    }

    /// Installs the writer of an authenticated attempt.
    pub(crate) fn install(
        &mut self,
        sender: mpsc::Sender<WsMessage>,
        shutdown: watch::Sender<bool>,
    ) {
        self.sender = Some(sender);
        self.shutdown = Some(shutdown);
    }

    /// Forgets a socket that has already failed, keeping credentials for a reconnect, and
    /// returns the generation the reconnect must use.
    pub(crate) fn lose(&mut self) -> u64 {
        self.sender = None;
        self.shutdown = None;
        self.advance()
    }

    /// Stops the socket and drops credentials because the control session that issued them ended.
    pub(crate) fn invalidate(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(true);
        }
        self.sender = None;
        self.token = None;
        self.advance();
    }

    /// Moves to a new generation so delayed events from older sockets are ignored.
    fn advance(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    /// Current generation.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Credentials for reconnecting after a payload-only failure.
    pub(crate) fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Writer for the current socket, if one is installed.
    pub(crate) fn sender(&self) -> Option<&mpsc::Sender<WsMessage>> {
        self.sender.as_ref()
    }
}
