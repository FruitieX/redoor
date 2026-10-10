//! Per-agent websocket session actor: authenticates registration and bridges the control
//! socket to the router through bounded, prioritized lanes.

use crate::actors::lanes::{ControlLaneReceivers, ControlLanes};
use crate::actors::router::{
    ConnectionStreams, RegisterAgentRequest, RegistrationRejection, RouteResponse,
    RouteTransferReadyRequest, RouterHandle, RouterMsg, TransferProgressUpdateRequest,
};
use crate::watchdog::{WatchdogHandle, WatchdogRegistry};
use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures_util::{SinkExt, StreamExt};
use redoor_core::log;
use redoor_protocol::wire::handshake::RejectReason;
use redoor_protocol::wire::log::Level;
use redoor_protocol::wire::registration::{AgentCapabilities, AgentIdentity};
use redoor_protocol::{
    ids::{AgentId, SocketId},
    wire::{AgentToServer, ServerToAgent},
};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// Per-agent command backlog allowed before REST admission fails fast.
pub(crate) const CONTROL_COMMAND_QUEUE_CAPACITY: usize = 64;
/// Lifecycle and cancellation traffic has independent capacity from ordinary commands.
const CONTROL_PRIORITY_QUEUE_CAPACITY: usize = 16;
/// How long an authenticated socket may stay silent before its `hello`.
///
/// The token was already checked during the upgrade, so this only bounds how long a stuck or
/// buggy client can hold a socket that does not speak for any agent.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a refused connection may take to flush its `rejected` frame before it is dropped.
const REJECTION_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// Converts a lane receive result into the next outbound frame while tracking when that lane
/// has closed.
fn take_outbound_frame(
    frame: Option<ServerToAgent>,
    lane_closed: &mut bool,
) -> Option<ServerToAgent> {
    if frame.is_none() {
        *lane_closed = true;
    }
    frame
}

/// Encodes one typed control frame for the socket; this writer is the only place the
/// server-to-agent control protocol becomes JSON.
fn encode_frame(frame: &ServerToAgent) -> Option<WsMessage> {
    match serde_json::to_string(frame) {
        Ok(json) => Some(WsMessage::Text(json.into())),
        Err(error) => {
            redoor_core::log_failure!(
                Level::Error,
                "Failed to serialize control frame for agent: error={}",
                error
            );
            None
        }
    }
}

/// The identity fields of an agent's `hello`, unpacked from the wire frame.
struct Hello {
    agent_id: AgentId,
    agent_name: String,
    identity: AgentIdentity,
    capabilities: AgentCapabilities,
}

/// Whether the read loop keeps serving the socket after one frame.
#[derive(Debug, PartialEq, Eq)]
enum SessionStep {
    Continue,
    /// The peer broke the protocol; drop the socket right away.
    Close,
    /// The `hello` was refused; flush the queued `rejected` frame, then close.
    Rejected,
}

/// Runtime state held for one websocket-backed agent session.
struct SessionRuntime {
    /// Identifies the websocket connection so the router can map routed work back
    /// to this specific session.
    socket_id: SocketId,
    /// Provides the actor handle used to register the session and forward control
    /// and streaming messages into the routing layer.
    router_ref: RouterHandle,
    /// Absent until the router accepted this connection's `hello`; afterwards every frame is
    /// attributed to this agent, because frames themselves carry no identity.
    agent_id: Option<AgentId>,
    /// Typed command and priority lanes this session's writer drains.
    lanes: ControlLanes,

    /// Populated after `hello` if the agent name has a matching watchdog supervisor. The stale
    /// check uses this to signal the supervisor when the WebSocket goes silent. Stays `None`
    /// for agents that aren't supervised (e.g. manually-spawned external agents), whose stale
    /// sockets are closed without restarting a process.
    watchdog: Option<WatchdogHandle>,
    /// Payload stream tables of the registered session; credit the agent returns for
    /// server-to-agent streams is applied here directly, never through the router mailbox.
    streams: Option<ConnectionStreams>,
}

impl SessionRuntime {
    /// Handles one control frame: `hello` before registration, everything else after it.
    async fn handle_control_message(
        &mut self,
        message: AgentToServer,
        watchdog_registry: &WatchdogRegistry,
    ) -> SessionStep {
        match (self.agent_id.clone(), message) {
            (
                None,
                AgentToServer::Hello {
                    agent_id,
                    agent_name,
                    identity,
                    capabilities,
                },
            ) => {
                self.register(
                    Hello {
                        agent_id,
                        agent_name,
                        identity,
                        capabilities,
                    },
                    watchdog_registry,
                )
                .await
            }
            (None, other) => {
                log!(
                    Level::Warning,
                    "Closing agent socket that sent a frame before hello: socket_id={}, frame={}",
                    self.socket_id,
                    other.kind()
                );
                SessionStep::Close
            }
            (Some(_), AgentToServer::Hello { .. }) => {
                log!(
                    Level::Warning,
                    "Closing agent socket that sent a second hello: socket_id={}",
                    self.socket_id
                );
                SessionStep::Close
            }
            (Some(agent_id), other) => {
                self.handle_agent_frame(agent_id, other).await;
                SessionStep::Continue
            }
        }
    }

    /// Links the watchdog supervisor and asks the router to accept the agent.
    async fn register(
        &mut self,
        hello: Hello,
        watchdog_registry: &WatchdogRegistry,
    ) -> SessionStep {
        // Look up the supervisor for this agent name. A `None` result is fine: it just means
        // the agent is not supervised by the watchdog (e.g. an external agent spawned outside
        // the server) and stale cleanup has no process to restart.
        self.watchdog = watchdog_registry.lookup(&hello.agent_name);
        let mut watchdog_attempt_generation = None;
        if let Some(watchdog) = self.watchdog.as_ref() {
            let Some(attempt_generation) = watchdog.mark_connected(self.socket_id.clone()).await
            else {
                log!(
                    Level::Warning,
                    "Rejecting managed agent while stopped: agent_name={}",
                    hello.agent_name
                );
                self.watchdog = None;
                let _ = self.lanes.try_priority(ServerToAgent::Rejected {
                    reason: RejectReason::Stopped,
                    message: "Managed agent is intentionally stopped".to_string(),
                });
                return SessionStep::Rejected;
            };
            watchdog_attempt_generation = Some(attempt_generation);
            log!(
                Level::Debug,
                "Session linked to watchdog supervisor: agent_name={}",
                hello.agent_name
            );
        }
        let agent_id = hello.agent_id.clone();
        let streams = ConnectionStreams::new(agent_id.clone());
        let (socket_id, lanes, watchdog) = (
            self.socket_id.clone(),
            self.lanes.clone(),
            self.watchdog.clone(),
        );
        let registered_streams = streams.clone();
        // Registration installs the routing entry every later frame depends on, so a
        // momentarily full mailbox must delay it rather than silently drop it.
        let decision = self
            .router_ref
            .request(move |reply| {
                RouterMsg::RegisterAgent(RegisterAgentRequest {
                    agent_id: hello.agent_id,
                    agent_name: hello.agent_name,
                    socket_id,
                    lanes,
                    identity: hello.identity,
                    capabilities: hello.capabilities.normalized(),
                    watchdog,
                    watchdog_attempt_generation,
                    streams: registered_streams,
                    reply: Some(reply),
                })
            })
            .await;
        match decision {
            Ok(Ok(())) => {
                self.streams = Some(streams);
                self.agent_id = Some(agent_id);
                SessionStep::Continue
            }
            Ok(Err(RegistrationRejection { reason, message })) => {
                log!(
                    Level::Warning,
                    "Agent hello rejected: agent_id={}, socket_id={}, reason={:?}, message={}",
                    agent_id,
                    self.socket_id,
                    reason,
                    message
                );
                if let Some(watchdog) = self.watchdog.take() {
                    watchdog.mark_disconnected(self.socket_id.clone());
                }
                SessionStep::Rejected
            }
            Err(error) => {
                redoor_core::log_failure!(
                    Level::Error,
                    "Failed to register agent: agent_id={}, socket_id={}, error={}",
                    agent_id,
                    self.socket_id,
                    error
                );
                SessionStep::Close
            }
        }
    }

    /// Handles a frame from a session whose `hello` was accepted.
    ///
    /// Frames carry no agent id: the session's registered id (bound to an authenticated
    /// upgrade) is the only identity the router checks ownership against, so a client cannot
    /// answer or steer another agent's requests.
    async fn handle_agent_frame(&mut self, agent_id: AgentId, message: AgentToServer) {
        match message {
            AgentToServer::Hello { .. } => {}
            AgentToServer::TransferReady { id } => {
                // TransferReady is a one-shot gate for uploads/copies. try_send would
                // permanently drop readiness when the router mailbox is full, leaving
                // the transfer stuck until client timeout with no cleanup path.
                if let Err(error) = self
                    .router_ref
                    .send_async(RouterMsg::RouteTransferReady(RouteTransferReadyRequest {
                        agent_id: agent_id.clone(),
                        socket_id: self.socket_id.clone(),
                        request_id: id,
                    }))
                    .await
                {
                    redoor_core::log_failure!(
                        Level::Error,
                        "Failed to queue transfer readiness: agent_id={}, request_id={}, error={}",
                        agent_id,
                        id,
                        error
                    );
                }
            }
            AgentToServer::CommandResponse { id, result } => {
                // Terminal responses release pending requests and transfer state, so
                // temporary mailbox pressure must never discard their cleanup signal.
                if let Err(error) = self
                    .router_ref
                    .send_async(RouterMsg::RouteResponse(RouteResponse {
                        agent_id: agent_id.clone(),
                        socket_id: self.socket_id.clone(),
                        request_id: id,
                        result,
                    }))
                    .await
                {
                    redoor_core::log_failure!(
                        Level::Error,
                        "Failed to queue command response: agent_id={}, request_id={}, error={}",
                        agent_id,
                        id,
                        error
                    );
                }
            }
            AgentToServer::PublishRequest { id } => {
                let _ = self
                    .router_ref
                    .send_async(RouterMsg::BeginUploadPublication {
                        agent_id,
                        socket_id: self.socket_id.clone(),
                        request_id: id,
                    })
                    .await;
            }
            AgentToServer::Credit { stream, frames } => {
                if let Some(streams) = &self.streams {
                    streams.grant_outbound(stream, frames);
                }
            }
            AgentToServer::TransferProgress {
                id,
                transferred_bytes,
                total_bytes,
            } => {
                // Late archive totals and local copy progress arrive once or rarely, so waiting
                // for mailbox capacity is cheap while dropping them would leave stale progress.
                let _ = self
                    .router_ref
                    .send_async(RouterMsg::TransferProgressUpdate(
                        TransferProgressUpdateRequest {
                            agent_id,
                            socket_id: self.socket_id.clone(),
                            request_id: id,
                            transferred_bytes,
                            total_bytes,
                        },
                    ))
                    .await;
            }
        }
    }

    /// Unregisters the session's agent after the websocket goes away.
    async fn shutdown(self) {
        if let Some(watchdog) = self.watchdog.as_ref() {
            watchdog.mark_disconnected(self.socket_id.clone());
        }
        if let Some(agent_id) = self.agent_id
            && let Err(error) = self
                .router_ref
                .send_async(RouterMsg::UnregisterAgent {
                    agent_id: agent_id.clone(),
                    socket_id: self.socket_id.clone(),
                })
                .await
        {
            redoor_core::log_failure!(
                Level::Error,
                "Failed to queue disconnected agent cleanup: agent_id={}, socket_id={}, error={}",
                agent_id,
                self.socket_id,
                error
            );
        }

        log!(Level::Info, "Session stopped: socket_id={}", self.socket_id);
    }

    /// Dispatches one inbound websocket frame to the appropriate handler.
    async fn handle_ws_message(
        &mut self,
        message: WsMessage,
        watchdog_registry: &WatchdogRegistry,
    ) -> SessionStep {
        match message {
            WsMessage::Text(text) => match serde_json::from_str::<AgentToServer>(&text) {
                Ok(message) => {
                    self.handle_control_message(message, watchdog_registry)
                        .await
                }
                Err(error) if self.agent_id.is_none() => {
                    log!(
                        Level::Warning,
                        "Closing agent socket whose first frame is not a hello: socket_id={}, error={}",
                        self.socket_id,
                        error
                    );
                    SessionStep::Close
                }
                Err(error) => {
                    redoor_core::log_failure!(
                        Level::Error,
                        "Failed to deserialize WebSocket message: {1}, raw text: {0}",
                        text,
                        error
                    );
                    SessionStep::Continue
                }
            },
            WsMessage::Binary(_) => {
                log!(
                    Level::Warning,
                    "Binary frame on control socket: socket_id={}",
                    self.socket_id
                );
                SessionStep::Close
            }
            _ => SessionStep::Continue,
        }
    }
}

/// Entry point for a new, already authenticated agent connection. Splits the socket into
/// send/receive halves and wires them to the router using explicit Tokio channels.
pub async fn handle_websocket(
    socket: WebSocket,
    socket_id: SocketId,
    router_ref: RouterHandle,
    watchdog_registry: WatchdogRegistry,
    timeouts: redoor_core::websocket::WebSocketTimeouts,
) {
    let (mut sender, mut receiver) = socket.split::<WsMessage>();
    let (lanes, receivers) = ControlLanes::bounded(
        CONTROL_COMMAND_QUEUE_CAPACITY,
        CONTROL_PRIORITY_QUEUE_CAPACITY,
    );
    let ControlLaneReceivers {
        commands: mut rx_out_commands,
        priority: mut rx_out_priority,
    } = receivers;

    let mut runtime = SessionRuntime {
        socket_id: socket_id.clone(),
        router_ref,
        agent_id: None,
        lanes,
        watchdog: None,
        streams: None,
    };

    log!(Level::Info, "Session started: socket_id={}", socket_id);

    // The writer task signals through this channel when it ends, so the reader
    // loop can stop promptly instead of waiting for the read side to notice the
    // broken connection.
    let (writer_done_tx, mut writer_done_rx) = oneshot::channel::<()>();

    let mut writer_task = tokio::spawn(async move {
        let mut commands_closed = false;
        let mut priority_closed = false;
        let mut ping_interval = timeouts.keepalive_interval();

        loop {
            // `biased` keeps control-plane text messages responsive while binary streaming is active.
            let next_message = tokio::select! {
                biased;
                _ = ping_interval.tick() => {
                    // A ping forces a write; if the connection is dead the send
                    // fails and the session can tear down instead of lingering.
                    if !matches!(
                        tokio::time::timeout(
                            timeouts.stale_timeout,
                            sender.send(WsMessage::Ping(bytes::Bytes::new())),
                        )
                        .await,
                        Ok(Ok(()))
                    ) {
                        break;
                    }
                    continue;
                }
                frame = rx_out_priority.recv(), if !priority_closed => take_outbound_frame(frame, &mut priority_closed),
                frame = rx_out_commands.recv(), if !commands_closed => take_outbound_frame(frame, &mut commands_closed),
                // Both bounded lanes are closed, so no future application frames can be produced.
                else => {
                    let _ = sender.send(WsMessage::Close(None)).await;
                    break;
                }
            };

            let Some(message) = next_message.as_ref().and_then(encode_frame) else {
                continue;
            };

            if !matches!(
                tokio::time::timeout(timeouts.stale_timeout, sender.send(message)).await,
                Ok(Ok(()))
            ) {
                // A send failure means the websocket is gone, so continuing to pull
                // router output would only accumulate work for a dead session.
                break;
            }
        }

        let _ = writer_done_tx.send(());
    });

    // Track the last time we received any frame on the WebSocket. Any
    // inbound frame (Text, Binary, Ping, Pong, Close) resets the timer.
    // If no frame arrives for the configured stale timeout we assume the
    // connection is half-open (e.g. SSH tunnel died without a TCP
    // close). Managed sessions also ask their watchdog to restart the subprocess.
    let mut last_seen = Instant::now();
    let opened_at = Instant::now();
    let mut stale_check = tokio::time::interval(timeouts.stale_check_interval);
    stale_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Burn the first immediate tick so the first stale check fires one
    // interval from now rather than immediately at session start.
    stale_check.tick().await;

    let mut rejected = false;
    loop {
        tokio::select! {
            biased;
            _ = &mut writer_done_rx => break,
            _ = stale_check.tick() => {
                if runtime.agent_id.is_none() && opened_at.elapsed() > HELLO_TIMEOUT {
                    log!(
                        Level::Warning,
                        "Closing agent socket that sent no hello: socket_id={}",
                        socket_id
                    );
                    break;
                }
                if last_seen.elapsed() > timeouts.stale_timeout {
                    if let Some(watchdog) = runtime.watchdog.as_ref() {
                        log!(
                            Level::Warning,
                            "WebSocket stale for {:?}, requesting restart: agent_name={}, socket_id={}",
                            last_seen.elapsed(),
                            watchdog.key(),
                            socket_id
                        );
                        watchdog.signal_stale(socket_id.clone());
                    } else {
                        log!(
                            Level::Warning,
                            "Unmanaged WebSocket stale for {:?}, closing session: socket_id={}",
                            last_seen.elapsed(),
                            socket_id
                        );
                    }
                    // Break out of the read loop; `runtime.shutdown()`
                    // below unregisters the agent and closes the writer lane.
                    break;
                }
            }
            result = receiver.next() => match result {
                Some(Ok(message)) => {
                    // Any inbound frame (including Pong) resets the
                    // stale timer. The dispatch below happens before
                    // we drop the frame so the runtime can act on it.
                    last_seen = Instant::now();
                    match runtime.handle_ws_message(message, &watchdog_registry).await {
                        SessionStep::Continue => {}
                        SessionStep::Close => break,
                        SessionStep::Rejected => {
                            rejected = true;
                            break;
                        }
                    }
                }
                _ => break,
            },
        }
    }

    runtime.shutdown().await;
    if rejected {
        // The session held the last lane senders, so the writer flushes the queued `rejected`
        // frame, closes the socket and ends; the agent then exits instead of reconnecting.
        if tokio::time::timeout(REJECTION_FLUSH_TIMEOUT, &mut writer_task)
            .await
            .is_err()
        {
            writer_task.abort();
        }
    } else {
        writer_task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redoor_protocol::ids::RequestId;
    use redoor_protocol::wire::command::CommandResult;
    use tokio::sync::mpsc;

    /// The writer's encoding is the wire format the agent parses.
    #[test]
    fn writer_encodes_typed_frames_as_protocol_json() {
        let frame = ServerToAgent::Cancel {
            id: RequestId::new(5),
        };
        let Some(WsMessage::Text(text)) = encode_frame(&frame) else {
            panic!("control frames are text messages");
        };
        // Agents and fake test agents decode exactly this tag and field.
        assert_eq!(text.as_str(), r#"{"type":"cancel","id":5}"#);
    }

    /// Builds a session runtime over a capturing router mailbox.
    fn capture_session(agent_id: Option<AgentId>) -> (SessionRuntime, mpsc::Receiver<RouterMsg>) {
        let (sender, receiver) = mpsc::channel(8);
        let (lanes, _receivers) = ControlLanes::bounded(1, 1);
        let runtime = SessionRuntime {
            socket_id: SocketId::new(),
            router_ref: RouterHandle::new(sender),
            agent_id,
            lanes,
            watchdog: None,
            streams: None,
        };
        (runtime, receiver)
    }

    /// A socket that has not said hello must not reach the router with agent frames, and the
    /// protocol violation closes it: otherwise an authenticated but unidentified socket could
    /// resolve or steer pending requests of whichever agent it guessed.
    #[tokio::test]
    async fn frames_before_hello_close_the_socket() {
        let watchdog_registry = WatchdogRegistry::new();
        let request_id = RequestId::new(1);
        for frame in [
            AgentToServer::CommandResponse {
                id: request_id,
                result: CommandResult::RawUpload,
            },
            AgentToServer::TransferReady { id: request_id },
            AgentToServer::TransferProgress {
                id: request_id,
                transferred_bytes: 1,
                total_bytes: None,
            },
            AgentToServer::PublishRequest { id: request_id },
            AgentToServer::Credit {
                stream: request_id,
                frames: 1,
            },
        ] {
            let (mut runtime, mut receiver) = capture_session(None);
            let step = runtime
                .handle_control_message(frame, &watchdog_registry)
                .await;
            // The socket is dropped instead of waiting for a hello that may never come.
            assert_eq!(step, SessionStep::Close);
            // Nothing may be queued for the router before the hello was accepted.
            assert!(receiver.try_recv().is_err());
        }
    }

    /// A registered session routes frames under its own agent id, because frames carry none.
    #[tokio::test]
    async fn registered_session_routes_frames_under_its_own_agent_id() {
        let own = AgentId::from("own");
        let (mut runtime, mut receiver) = capture_session(Some(own.clone()));
        let socket_id = runtime.socket_id.clone();
        runtime
            .handle_control_message(
                AgentToServer::CommandResponse {
                    id: RequestId::new(3),
                    result: CommandResult::RawUpload,
                },
                &WatchdogRegistry::new(),
            )
            .await;
        // The router sees the session's identity and socket, which it checks for ownership.
        let Ok(RouterMsg::RouteResponse(response)) = receiver.try_recv() else {
            panic!("registered session should forward the response");
        };
        assert_eq!(response.agent_id, own);
        assert_eq!(response.socket_id, socket_id);
    }

    /// A second hello on an accepted connection is a protocol violation, not a re-register.
    #[tokio::test]
    async fn second_hello_closes_the_socket() {
        let (mut runtime, mut receiver) = capture_session(Some(AgentId::from("own")));
        let hello: AgentToServer = serde_json::from_str(
            r#"{"type":"hello","agent_id":"other","agent_name":"other","os":"linux","arch":"x86_64","hostname":"h","username":"u","cwd":"/","binary":{"version":"0.0.0","git_rev":"r","git_dirty":false,"version_dirty":false,"build_mode":"debug","build_date":"d"}}"#,
        )
        .unwrap();
        let step = runtime
            .handle_control_message(hello, &WatchdogRegistry::new())
            .await;
        // Switching identity mid-connection would let one agent speak for another.
        assert_eq!(step, SessionStep::Close);
        assert!(receiver.try_recv().is_err());
    }

    /// Exercises session dispatch under backpressure so completion cleanup cannot be lost.
    #[tokio::test]
    async fn command_response_waits_for_router_capacity_before_routing() {
        let (sender, mut receiver) = mpsc::channel(1);
        let router_ref = RouterHandle::new(sender);
        router_ref
            .send(RouterMsg::CheckPendingUiRefresh)
            .expect("router mailbox filled");
        let agent_id = AgentId::from("agent");
        let request_id = RequestId::new(42);
        let (lanes, _receivers) = ControlLanes::bounded(1, 1);
        let mut runtime = SessionRuntime {
            socket_id: SocketId::new(),
            router_ref,
            agent_id: Some(agent_id.clone()),
            lanes,
            watchdog: None,
            streams: None,
        };
        let watchdog_registry = WatchdogRegistry::new();
        let response = runtime.handle_control_message(
            AgentToServer::CommandResponse {
                id: request_id,
                result: CommandResult::RawUpload,
            },
            &watchdog_registry,
        );
        tokio::pin!(response);

        // Poll the actual handler to distinguish backpressure from a silently dropped response.
        assert!(futures_util::poll!(&mut response).is_pending());
        // Only the preexisting message may occupy the mailbox while completion is waiting.
        assert!(matches!(
            receiver.recv().await,
            Some(RouterMsg::CheckPendingUiRefresh)
        ));
        response.await;
        // The completion payload must survive capacity pressure unchanged for transfer cleanup.
        assert!(matches!(
            receiver.recv().await,
            Some(RouterMsg::RouteResponse(RouteResponse {
                agent_id: received_agent_id,
                request_id: received_request_id,
                socket_id: _,
                result: CommandResult::RawUpload,
            })) if received_agent_id == agent_id && received_request_id == request_id
        ));
    }

    /// Verifies teardown waits for router capacity instead of dropping the unregistration.
    #[tokio::test]
    async fn shutdown_waits_for_router_capacity_before_unregistering_agent() {
        let (sender, mut receiver) = mpsc::channel(1);
        let router_ref = RouterHandle::new(sender);
        router_ref
            .send(RouterMsg::CheckPendingUiRefresh)
            .expect("router mailbox filled");
        let socket_id = SocketId::new();
        let (lanes, _receivers) = ControlLanes::bounded(1, 1);
        let runtime = SessionRuntime {
            socket_id: socket_id.clone(),
            router_ref,
            agent_id: Some(AgentId::from("agent")),
            lanes,
            watchdog: None,
            streams: None,
        };

        let shutdown_task = tokio::spawn(runtime.shutdown());
        tokio::task::yield_now().await;

        // A full mailbox must backpressure teardown instead of completing after dropping cleanup.
        assert!(!shutdown_task.is_finished());
        assert!(matches!(
            receiver.recv().await,
            Some(RouterMsg::CheckPendingUiRefresh)
        ));
        let cleanup = receiver.recv().await;
        // Releasing one slot must deliver the exact session cleanup that was waiting for capacity.
        assert!(matches!(
            cleanup,
            Some(RouterMsg::UnregisterAgent {
                agent_id,
                socket_id: received_socket_id,
            }) if agent_id == AgentId::from("agent") && received_socket_id == socket_id
        ));
        shutdown_task.await.expect("session shutdown task joined");
    }
}
