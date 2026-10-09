use crate::actors::lanes::{ControlLaneReceivers, ControlLanes};
use crate::actors::router::{
    ConnectionStreams, RegisterAgentRequest, RouteResponse, RouteTransferReadyRequest,
    RouterHandle, RouterMsg, TransferProgressUpdateRequest,
};
use crate::watchdog::{WatchdogHandle, WatchdogRegistry};
use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures_util::{SinkExt, StreamExt};
use redoor_core::log;
use redoor_protocol::wire::log::Level;
use redoor_protocol::{
    ids::{AgentId, SocketId},
    wire::{AgentToServer, ServerToAgent},
};
use sha2::{Digest, Sha256};
use std::time::Instant;
use subtle::ConstantTimeEq;
use tokio::sync::oneshot;

/// Per-agent command backlog allowed before REST admission fails fast.
pub(crate) const CONTROL_COMMAND_QUEUE_CAPACITY: usize = 64;
/// Lifecycle and cancellation traffic has independent capacity from ordinary commands.
const CONTROL_PRIORITY_QUEUE_CAPACITY: usize = 16;

/// Constant-time string equality so invalid tokens do not leak length via timing.
fn constant_time_eq(left: &str, right: &str) -> bool {
    let left_digest = Sha256::digest(left.as_bytes());
    let right_digest = Sha256::digest(right.as_bytes());
    bool::from(left_digest.ct_eq(&right_digest))
}

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

/// Runtime state held for one websocket-backed agent session.
struct SessionRuntime {
    /// Identifies the websocket connection so the router can map routed work back
    /// to this specific session.
    socket_id: SocketId,
    /// Provides the actor handle used to register the session and forward control
    /// and streaming messages into the routing layer.
    router_ref: RouterHandle,
    /// Remains absent until the websocket sends `AgentToServer::AgentRegister`, because
    /// the socket exists before the remote agent has announced which logical
    /// agent identity it should be associated with.
    agent_id: Option<AgentId>,
    /// Typed command and priority lanes this session's writer drains.
    lanes: ControlLanes,

    /// Populated after `AgentRegister` if the registered agent name has a
    /// matching watchdog supervisor. The stale check uses this to signal
    /// the supervisor when the WebSocket goes silent. Stays `None` for
    /// agents that aren't supervised (e.g. manually-spawned external
    /// agents), whose stale sockets are closed without restarting a process.
    watchdog: Option<WatchdogHandle>,
    /// Expected top-level `agent_token`; registration without this secret is rejected.
    agent_token: String,
    /// Payload stream tables of the registered session; credit the agent returns for
    /// server-to-agent streams is applied here directly, never through the router mailbox.
    streams: Option<ConnectionStreams>,
}

impl SessionRuntime {
    /// Registers the agent with the router once the websocket announces itself.
    async fn handle_control_message(
        &mut self,
        message: AgentToServer,
        watchdog_registry: &WatchdogRegistry,
    ) {
        match message {
            AgentToServer::AgentRegister {
                agent_id,
                agent_name,
                token,
                identity,
                capabilities,
            } => {
                // Reject impostors before name takeover: an unauthenticated client
                // that only knows an agent name must not replace a live connection.
                if !constant_time_eq(&token, &self.agent_token) {
                    log!(
                        Level::Warning,
                        "Rejecting agent registration with invalid token: agent_name={}, socket_id={}",
                        agent_name,
                        self.socket_id
                    );
                    let _ = self.lanes.try_priority(ServerToAgent::Error {
                        message: "Invalid agent token".to_string(),
                    });
                    return;
                }

                // Look up the supervisor for this agent name BEFORE
                // moving `agent_name` into the registration payload so
                // the registry still has access to it. A `None` result
                // is fine: it just means the agent is not supervised by
                // the watchdog (e.g. an external agent spawned outside
                // the server) and stale cleanup has no process to restart.
                self.watchdog = watchdog_registry.lookup(&agent_name);
                let mut watchdog_attempt_generation = None;
                if let Some(watchdog) = self.watchdog.as_ref() {
                    let Some(attempt_generation) =
                        watchdog.mark_connected(self.socket_id.clone()).await
                    else {
                        log!(
                            Level::Warning,
                            "Rejecting managed agent registration while stopped: agent_name={}",
                            agent_name
                        );
                        let _ = self.lanes.try_priority(ServerToAgent::Error {
                            message: "Managed agent is intentionally stopped".to_string(),
                        });
                        self.watchdog = None;
                        return;
                    };
                    watchdog_attempt_generation = Some(attempt_generation);
                    log!(
                        Level::Debug,
                        "Session linked to watchdog supervisor: agent_name={}",
                        agent_name
                    );
                }

                let capabilities = capabilities.normalized();
                let streams =
                    ConnectionStreams::new(agent_id.clone(), capabilities.supports_stream_credit);
                self.streams = Some(streams.clone());
                // Registration installs the routing entry every later frame depends on, so a
                // momentarily full mailbox must delay it rather than silently drop it.
                if let Err(error) = self
                    .router_ref
                    .send_async(RouterMsg::RegisterAgent(RegisterAgentRequest {
                        agent_id: agent_id.clone(),
                        agent_name,
                        socket_id: self.socket_id.clone(),
                        lanes: self.lanes.clone(),
                        identity,
                        capabilities,
                        watchdog: self.watchdog.clone(),
                        watchdog_attempt_generation,
                        streams,
                    }))
                    .await
                {
                    redoor_core::log_failure!(
                        Level::Error,
                        "Failed to queue agent registration: agent_id={}, socket_id={}, error={}",
                        agent_id,
                        self.socket_id,
                        error
                    );
                }
                self.agent_id = Some(agent_id);
            }
            other => match self.agent_id.clone() {
                Some(agent_id) => self.handle_agent_frame(agent_id, other).await,
                None => log!(
                    Level::Warning,
                    "Ignoring agent frame before successful registration: socket_id={}, frame={}",
                    self.socket_id,
                    other.kind()
                ),
            },
        }
    }

    /// Handles a frame from a session whose registration was accepted.
    ///
    /// Frames carry their own `agent_id`, but it is never trusted: the session's registered
    /// id (bound to a valid `agent_token`) is what the router checks ownership against, so a
    /// client cannot answer or steer another agent's requests by naming that agent.
    async fn handle_agent_frame(&mut self, agent_id: AgentId, message: AgentToServer) {
        if let Some(claimed) = message.claimed_agent_id()
            && claimed != &agent_id
        {
            log!(
                Level::Warning,
                "Agent frame names a different agent than its session; using the session's: socket_id={}, session_agent_id={}, frame_agent_id={}",
                self.socket_id,
                agent_id,
                claimed
            );
        }
        match message {
            AgentToServer::AgentRegister { .. } => {}
            AgentToServer::AgentUnregister { .. } => {
                if let Some(watchdog) = self.watchdog.as_ref() {
                    watchdog.mark_disconnected(self.socket_id.clone());
                }
                if let Err(error) = self
                    .router_ref
                    .send_async(RouterMsg::UnregisterAgent {
                        agent_id: agent_id.clone(),
                        socket_id: self.socket_id.clone(),
                    })
                    .await
                {
                    redoor_core::log_failure!(
                        Level::Error,
                        "Failed to queue explicit agent cleanup: agent_id={}, socket_id={}, error={}",
                        agent_id,
                        self.socket_id,
                        error
                    );
                }
            }
            AgentToServer::TransferReady { request_id, .. } => {
                // TransferReady is a one-shot gate for uploads/copies. try_send would
                // permanently drop readiness when the router mailbox is full, leaving
                // the transfer stuck until client timeout with no cleanup path.
                if let Err(error) = self
                    .router_ref
                    .send_async(RouterMsg::RouteTransferReady(RouteTransferReadyRequest {
                        agent_id: agent_id.clone(),
                        socket_id: self.socket_id.clone(),
                        request_id,
                    }))
                    .await
                {
                    redoor_core::log_failure!(
                        Level::Error,
                        "Failed to queue transfer readiness: agent_id={}, request_id={}, error={}",
                        agent_id,
                        request_id,
                        error
                    );
                }
            }
            AgentToServer::CommandResponse {
                request_id, result, ..
            } => {
                // Terminal responses release pending requests and transfer state, so
                // temporary mailbox pressure must never discard their cleanup signal.
                if let Err(error) = self
                    .router_ref
                    .send_async(RouterMsg::RouteResponse(RouteResponse {
                        agent_id: agent_id.clone(),
                        socket_id: self.socket_id.clone(),
                        request_id,
                        result,
                    }))
                    .await
                {
                    redoor_core::log_failure!(
                        Level::Error,
                        "Failed to queue command response: agent_id={}, request_id={}, error={}",
                        agent_id,
                        request_id,
                        error
                    );
                }
            }
            AgentToServer::UploadPublicationRequest { request_id, .. } => {
                let _ = self
                    .router_ref
                    .send_async(RouterMsg::BeginUploadPublication {
                        agent_id,
                        socket_id: self.socket_id.clone(),
                        request_id,
                    })
                    .await;
            }
            AgentToServer::StreamCredit { request_id, frames } => {
                if let Some(streams) = &self.streams {
                    streams.grant_outbound(request_id, frames);
                }
            }
            AgentToServer::TransferProgressUpdate {
                request_id,
                transferred_bytes,
                total_bytes,
                ..
            } => {
                // Late archive totals and local copy progress arrive once or rarely, so waiting
                // for mailbox capacity is cheap while dropping them would leave stale progress.
                let _ = self
                    .router_ref
                    .send_async(RouterMsg::TransferProgressUpdate(
                        TransferProgressUpdateRequest {
                            agent_id,
                            socket_id: self.socket_id.clone(),
                            request_id,
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
    /// Returns `false` when the session should stop (e.g. router rejected
    /// a binary chunk), so the caller can break out of its read loop.
    async fn handle_ws_message(
        &mut self,
        message: WsMessage,
        watchdog_registry: &WatchdogRegistry,
    ) -> bool {
        match message {
            WsMessage::Text(text) => match serde_json::from_str::<AgentToServer>(&text) {
                Ok(message) => {
                    self.handle_control_message(message, watchdog_registry)
                        .await
                }
                Err(error) => {
                    redoor_core::log_failure!(
                        Level::Error,
                        "Failed to deserialize WebSocket message: {1}, raw text: {0}",
                        text,
                        error
                    );
                }
            },
            WsMessage::Binary(_) => {
                log!(
                    Level::Warning,
                    "Binary frame on control socket: socket_id={}",
                    self.socket_id
                );
                return false;
            }
            _ => {}
        }
        true
    }
}

/// Entry point for a new WebSocket connection. Splits the socket into send/receive
/// halves and wires them to the router using explicit Tokio channels.
pub async fn handle_websocket(
    socket: WebSocket,
    socket_id: SocketId,
    router_ref: RouterHandle,
    watchdog_registry: WatchdogRegistry,
    agent_token: String,
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
        agent_token,
        streams: None,
    };

    log!(Level::Info, "Session started: socket_id={}", socket_id);

    // The writer task signals through this channel when it ends, so the reader
    // loop can stop promptly instead of waiting for the read side to notice the
    // broken connection.
    let (writer_done_tx, mut writer_done_rx) = oneshot::channel::<()>();

    let writer_task = tokio::spawn(async move {
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
                else => break,
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
    let mut stale_check = tokio::time::interval(timeouts.stale_check_interval);
    stale_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Burn the first immediate tick so the first stale check fires one
    // interval from now rather than immediately at session start.
    stale_check.tick().await;

    loop {
        tokio::select! {
            biased;
            _ = &mut writer_done_rx => break,
            _ = stale_check.tick() => {
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
                    if !runtime.handle_ws_message(message, &watchdog_registry).await {
                        break;
                    }
                }
                _ => break,
            },
        }
    }

    runtime.shutdown().await;
    writer_task.abort();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// The writer's encoding is the wire format the agent parses.
    #[test]
    fn writer_encodes_typed_frames_as_protocol_json() {
        let frame = ServerToAgent::CancelTransfer {
            request_id: redoor_protocol::ids::RequestId::new(5),
        };
        let Some(WsMessage::Text(text)) = encode_frame(&frame) else {
            panic!("control frames are text messages");
        };
        // Agents and fake test agents decode exactly this tag and field.
        assert_eq!(
            text.as_str(),
            r#"{"type":"cancel_transfer","request_id":5}"#
        );
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
            agent_token: "token".to_string(),
            streams: None,
        };
        (runtime, receiver)
    }

    /// An unauthenticated socket must not reach the router with agent frames: otherwise any
    /// client of the public `/ws` endpoint could resolve or steer pending requests.
    #[tokio::test]
    async fn unregistered_session_forwards_no_agent_frames() {
        let (mut runtime, mut receiver) = capture_session(None);
        let watchdog_registry = WatchdogRegistry::new();
        let request_id = redoor_protocol::ids::RequestId::new(1);
        let agent_id = AgentId::from("victim");
        for frame in [
            AgentToServer::CommandResponse {
                agent_id: agent_id.clone(),
                request_id,
                result: redoor_protocol::wire::command::CommandResult::RawUpload,
            },
            AgentToServer::TransferReady {
                agent_id: agent_id.clone(),
                request_id,
            },
            AgentToServer::TransferProgressUpdate {
                agent_id: agent_id.clone(),
                request_id,
                transferred_bytes: 1,
                total_bytes: None,
            },
            AgentToServer::UploadPublicationRequest {
                agent_id: agent_id.clone(),
                request_id,
            },
            AgentToServer::AgentUnregister {
                agent_id: agent_id.clone(),
            },
        ] {
            runtime
                .handle_control_message(frame, &watchdog_registry)
                .await;
        }
        // Nothing may be queued for the router before a registration with a valid token.
        assert!(receiver.try_recv().is_err());
    }

    /// A registered session routes frames under its own agent id, never the one a frame
    /// claims, so an authenticated agent cannot impersonate another.
    #[tokio::test]
    async fn registered_session_routes_frames_under_its_own_agent_id() {
        let own = AgentId::from("own");
        let (mut runtime, mut receiver) = capture_session(Some(own.clone()));
        let socket_id = runtime.socket_id.clone();
        runtime
            .handle_control_message(
                AgentToServer::CommandResponse {
                    agent_id: AgentId::from("someone-else"),
                    request_id: redoor_protocol::ids::RequestId::new(3),
                    result: redoor_protocol::wire::command::CommandResult::RawUpload,
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

    /// Exercises session dispatch under backpressure so completion cleanup cannot be lost.
    #[tokio::test]
    async fn command_response_waits_for_router_capacity_before_routing() {
        let (sender, mut receiver) = mpsc::channel(1);
        let router_ref = RouterHandle::new(sender);
        router_ref
            .send(RouterMsg::CheckPendingUiRefresh)
            .expect("router mailbox filled");
        let agent_id = AgentId::from("agent");
        let request_id = redoor_protocol::ids::RequestId::new(42);
        let (lanes, _receivers) = ControlLanes::bounded(1, 1);
        let mut runtime = SessionRuntime {
            socket_id: SocketId::new(),
            router_ref,
            agent_id: Some(agent_id.clone()),
            lanes,
            watchdog: None,
            agent_token: "token".to_string(),
            streams: None,
        };
        let watchdog_registry = WatchdogRegistry::new();
        let response = runtime.handle_control_message(
            AgentToServer::CommandResponse {
                agent_id: agent_id.clone(),
                request_id,
                result: redoor_protocol::wire::command::CommandResult::RawUpload,
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
                result: redoor_protocol::wire::command::CommandResult::RawUpload,
            })) if received_agent_id == agent_id && received_request_id == request_id
        ));
    }

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
            agent_token: "token".to_string(),
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
