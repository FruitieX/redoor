use super::RouterError;
use super::cleanup::AgentLoss;
use super::flows::{CommandFlow, Flow};
use super::messages::{
    AgentListEntry, ApplyManagedLifecycleRequest, ExecuteCommandRequest, OpenAgentLogStreamRequest,
    OpenTerminalRequest, RegisterAgentRequest, RegisterManagedAgentRequest,
    RegisterTransferConnectionRequest, RouteResponse, UnregisterManagedAgentRequest,
};
use super::state::{AgentConnection, KnownAgent, RouterState, TransferConnection};
use super::streams::AgentLink;
use redoor_core::log;
use redoor_protocol::wire::log::Level;
use redoor_protocol::wire::policy::CancelScope;
use redoor_protocol::{api::agents::AgentConnectionStatus, wire::command::CommandResult};
use redoor_protocol::{
    ids::{AgentId, SocketId, UnixTimestampSeconds},
    wire::ServerToAgent,
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

impl AgentConnection {
    /// Builds one live router connection entry from the agent registration payload.
    pub(crate) fn from_register_request(request: RegisterAgentRequest) -> Self {
        Self {
            agent_id: request.agent_id,
            agent_name: request.agent_name,
            socket_id: request.socket_id,
            lanes: request.lanes,
            transfer_token: uuid::Uuid::new_v4().to_string(),
            transfer: None,
            connected_at: UnixTimestampSeconds::new(jiff::Timestamp::now().as_second()),
            identity: request.identity,
            capabilities: request.capabilities,
            streams: request.streams,
        }
    }

    /// Lanes a stream of this connection uses without going through the router.
    pub(crate) fn link(&self) -> AgentLink {
        AgentLink::new(
            self.lanes.clone(),
            self.transfer
                .as_ref()
                .map(|transfer| transfer.outgoing_binary.clone()),
            self.streams.credit(),
        )
    }

    /// Admits one ordinary command or bootstrap onto the bounded command lane.
    pub(crate) fn send_message(&self, message: ServerToAgent) -> bool {
        self.report_admission(self.lanes.try_command(message))
    }

    /// Queues lifecycle or cancellation traffic independently of command admission.
    pub(crate) fn send_priority_message(&self, message: ServerToAgent) -> bool {
        self.report_admission(self.lanes.try_priority(message))
    }

    /// Logs a frame a saturated or closed lane refused, so dropped work is visible.
    fn report_admission(&self, admitted: bool) -> bool {
        if !admitted {
            log!(
                Level::Warning,
                "Failed to queue control frame for agent: socket_id={}",
                self.socket_id
            );
        }
        admitted
    }

    /// Returns the payload transport without ever falling back to the control socket.
    pub(crate) fn transfer_connection(&self) -> Result<TransferConnection, RouterError> {
        self.transfer
            .clone()
            .ok_or_else(|| RouterError::TransferConnectionUnavailable {
                agent_id: self.agent_id.to_string(),
            })
    }

    /// Stops the attached payload socket when this control session loses authority.
    pub(crate) fn shutdown_transfer(&self) {
        if let Some(transfer) = &self.transfer {
            let _ = transfer.shutdown.send(true);
        }
    }
}

/// Compares transfer tokens in constant time so rejection timing does not reveal token contents.
fn transfer_tokens_match(left: &str, right: &str) -> bool {
    let left_digest = Sha256::digest(left.as_bytes());
    let right_digest = Sha256::digest(right.as_bytes());
    bool::from(left_digest.ct_eq(&right_digest))
}

/// Current wall-clock second, used for inventory timestamps.
fn now() -> UnixTimestampSeconds {
    UnixTimestampSeconds::new(jiff::Timestamp::now().as_second())
}

impl RouterState {
    /// Commits live routing and retained inventory together after lifecycle admission.
    ///
    /// Public status stays non-Connected until the payload socket attaches so clients
    /// that gate on `Connected` do not start transfers during the normal handshake window.
    fn commit_registration(&mut self, request: RegisterAgentRequest) {
        log!(
            Level::Info,
            "Agent registered: agent_id={}, agent_name={}, socket_id={}",
            request.agent_id,
            request.agent_name,
            request.socket_id
        );
        let agent_id = request.agent_id.clone();
        let connection = AgentConnection::from_register_request(request);
        let transfer_open = ServerToAgent::TransferSocketOpen {
            token: connection.transfer_token.clone(),
            // Confirms credit only to agents that advertised it, so both ends switch together.
            stream_credit: connection.streams.credit(),
        };
        let transfer_open_lanes = connection.lanes.clone();
        let known = self
            .agents
            .known_by_id
            .entry(agent_id.clone())
            .or_insert_with(|| {
                KnownAgent::unregistered(
                    agent_id.clone(),
                    None,
                    AgentConnectionStatus::Disconnected,
                )
            });
        known.name = connection.agent_name.clone();
        known.default_directory = Some(connection.identity.default_directory.clone());
        // Control is live, but transfer usability still depends on the async payload handshake.
        // Leave Starting/Disconnected (or any non-Connected state) until register_transfer promotes.
        if known.status == AgentConnectionStatus::Connected {
            known.status = if known.managed {
                AgentConnectionStatus::Starting
            } else {
                AgentConnectionStatus::Disconnected
            };
        }
        known.connected_at = Some(connection.connected_at);
        known.connection_issue = None;
        known.socket_id = Some(connection.socket_id.clone());
        known.identity = Some(connection.identity.clone());
        known.capabilities = connection.capabilities;
        self.agents.by_id.insert(agent_id, connection);
        self.notify_agents_changed();

        let _ = transfer_open_lanes.try_priority(transfer_open);
    }

    /// Marks inventory fully ready only after both control and payload sockets are live.
    fn mark_agent_transfer_ready(&mut self, agent_id: &AgentId) {
        let Some(known) = self.agents.known_by_id.get_mut(agent_id) else {
            return;
        };
        if known.status == AgentConnectionStatus::Connected {
            return;
        }
        known.status = AgentConnectionStatus::Connected;
        known.connection_issue = None;
        self.notify_agents_changed();
    }

    /// Ends an evicted control session and fails everything that depended on it.
    ///
    /// `notice` is sent on the old session's priority lane so a replaced agent process exits
    /// promptly instead of lingering as a zombie.
    fn release_connection(&mut self, connection: AgentConnection, notice: Option<&str>) {
        connection.shutdown_transfer();
        if let Some(message) = notice {
            let _ = connection.send_priority_message(ServerToAgent::Error {
                message: message.to_string(),
            });
        }
        let agent_id = connection.agent_id.clone();
        self.fail_agent_flows(&agent_id, AgentLoss::Control);
        connection.streams.clear();
        self.terminal_registry.remove_agent_pending(&agent_id);
        self.log_registry.remove_agent(&agent_id);
    }

    /// Records in the inventory that an agent's authoritative connection ended.
    fn forget_connection(&mut self, agent_id: &AgentId) {
        if let Some(known) = self.agents.known_by_id.get_mut(agent_id) {
            known.last_seen_at = Some(now());
            known.connected_at = None;
            known.socket_id = None;
        }
    }

    /// Registers a connected agent, replacing any existing connection with the same name.
    ///
    /// When the SSH tunnel drops without a clean TCP close, the server may not detect the old
    /// session's disconnect for a long time. Rejecting the new connection in that window would
    /// make the agent permanently unreachable until the TCP timeout fires. Instead, the new
    /// connection takes over the name and the old session is cleaned up. The stale session's
    /// later `UnregisterAgent` is ignored because its `socket_id` will not match.
    pub(crate) fn register_agent(&mut self, request: RegisterAgentRequest) {
        // Token validation happens in the session layer before this message is enqueued, so
        // name takeover here is only reachable with a valid agent_token.
        let existing_agent_id = self
            .agents
            .by_id
            .iter()
            .find(|(_, info)| info.agent_name == request.agent_name)
            .map(|(id, _)| id.clone());
        if let Some(old_agent_id) = existing_agent_id {
            log!(
                Level::Warning,
                "Replacing stale agent connection: agent_name={}, old_agent_id={}, new_agent_id={}",
                request.agent_name,
                old_agent_id,
                request.agent_id
            );
            if let Some(old_connection) = self.agents.by_id.remove(&old_agent_id) {
                self.forget_connection(&old_agent_id);
                self.release_connection(
                    old_connection,
                    Some("Connection replaced by a new agent with the same name"),
                );
            }
        }

        let agent_id = request.agent_id.clone();
        let rejection_lanes = request.lanes.clone();
        let accepted = match request.watchdog.clone() {
            Some(watchdog) => {
                request
                    .watchdog_attempt_generation
                    .is_some_and(|attempt_generation| {
                        watchdog
                            .while_current_attempt(attempt_generation, || {
                                self.commit_registration(request)
                            })
                            .is_some()
                    })
            }
            None => {
                self.commit_registration(request);
                true
            }
        };
        if !accepted {
            let _ = rejection_lanes.try_priority(ServerToAgent::Error {
                message: "Managed agent is intentionally stopped".to_string(),
            });
            log!(
                Level::Warning,
                "Ignoring managed registration that lost a shutdown race: agent_id={}",
                agent_id
            );
        }
    }

    /// Removes an agent whose control session ended, unless a newer session replaced it.
    pub(crate) fn unregister_agent(&mut self, agent_id: AgentId, socket_id: SocketId) {
        // A replacement shares the agent id, so only the matching session may evict the entry.
        let is_current = self
            .agents
            .by_id
            .get(&agent_id)
            .is_some_and(|connection| connection.socket_id == socket_id);
        if !is_current {
            log!(
                Level::Debug,
                "Ignoring stale unregister: agent_id={}, socket_id={}",
                agent_id,
                socket_id
            );
            return;
        }
        log!(Level::Info, "Agent unregistered: agent_id={}", agent_id);
        let Some(connection) = self.agents.by_id.remove(&agent_id) else {
            return;
        };
        if let Some(known) = self.agents.known_by_id.get_mut(&agent_id) {
            known.connected_at = None;
            known.last_seen_at = Some(now());
            known.socket_id = None;
            known.status = if known.managed {
                AgentConnectionStatus::Starting
            } else {
                AgentConnectionStatus::Disconnected
            };
        }
        self.notify_agents_changed();
        self.release_connection(connection, None);
    }

    /// Authenticates and installs a payload socket under the current control session.
    ///
    /// When a broken transfer socket reconnects before the server processes the old socket's
    /// unregister, replacement must fail in-flight payload work first. Otherwise the delayed
    /// unregister is ignored as stale while the agent has already dropped the old workers,
    /// leaving HTTP requests stuck forever.
    ///
    /// Registration is cancellation-aware: if the socket handler timed out and dropped the
    /// reply receiver, its already-dead senders must not be installed. A timed-out handler
    /// never reaches unregister, so committing would leave a zombie transfer lane.
    pub(crate) fn register_transfer(&mut self, request: RegisterTransferConnectionRequest) {
        if request.reply.is_closed() {
            log!(
                Level::Warning,
                "Ignoring cancelled transfer registration: agent_id={}, socket_id={}",
                request.agent_id,
                request.socket_id
            );
            let _ = request.shutdown.send(true);
            return;
        }
        let Some(connection) = self.agents.by_id.get_mut(&request.agent_id) else {
            let _ = request
                .reply
                .send(Err(RouterError::TransferAuthenticationFailed));
            return;
        };
        if !transfer_tokens_match(&connection.transfer_token, &request.token) {
            let _ = request
                .reply
                .send(Err(RouterError::TransferAuthenticationFailed));
            return;
        }
        // Evict the old payload lane before installing the replacement so cleanup cannot race
        // with new traffic on the incoming socket.
        if let Some(previous) = connection.transfer.take() {
            let _ = previous.shutdown.send(true);
            log!(
                Level::Info,
                "Transfer socket replaced: agent_id={}, old_socket_id={}, socket_id={}",
                request.agent_id,
                previous.socket_id,
                request.socket_id
            );
            // The agent already cleared workers tied to the old socket, and the old socket's
            // unregister will be ignored as stale, so its flows must fail here.
            self.lose_transfer(
                &request.agent_id,
                format!("Transfer connection replaced: {}", request.agent_id),
            );
        }
        let Some(connection) = self.agents.by_id.get_mut(&request.agent_id) else {
            let _ = request.shutdown.send(true);
            let _ = request
                .reply
                .send(Err(RouterError::TransferAuthenticationFailed));
            return;
        };
        connection.transfer = Some(TransferConnection {
            socket_id: request.socket_id.clone(),
            outgoing_binary: request.outgoing_binary,
            shutdown: request.shutdown,
        });
        let streams = connection.streams.clone();
        log!(
            Level::Info,
            "Transfer socket registered: agent_id={}, socket_id={}",
            request.agent_id,
            request.socket_id
        );
        // Promote only here so Connected means transfers can succeed, not merely control is up.
        self.mark_agent_transfer_ready(&request.agent_id);
        let _ = request.reply.send(Ok(streams));
    }

    /// Fails payload flows of an agent whose transfer socket is gone and resets its streams.
    fn lose_transfer(&mut self, agent_id: &AgentId, reason: String) {
        self.fail_agent_flows(agent_id, AgentLoss::Transfer(reason));
        if let Some(connection) = self.agents.by_id.get(agent_id) {
            connection.streams.clear();
        }
    }

    /// Clears only the matching payload socket so stale teardown cannot evict a replacement.
    pub(crate) fn unregister_transfer(&mut self, agent_id: AgentId, socket_id: SocketId) {
        let Some(connection) = self.agents.by_id.get_mut(&agent_id) else {
            return;
        };
        if connection
            .transfer
            .as_ref()
            .is_none_or(|transfer| transfer.socket_id != socket_id)
        {
            log!(
                Level::Debug,
                "Ignoring stale transfer unregister: agent_id={}, socket_id={}",
                agent_id,
                socket_id
            );
            return;
        }
        connection.transfer = None;
        log!(
            Level::Info,
            "Transfer socket unregistered: agent_id={}, socket_id={}",
            agent_id,
            socket_id
        );
        self.lose_transfer(&agent_id, format!("Transfer connection lost: {agent_id}"));
    }

    /// Adds a configured stopped record before its dormant supervisor can be controlled.
    pub(crate) fn register_managed(&mut self, request: RegisterManagedAgentRequest) {
        let id = request.agent_id;
        let mut known = KnownAgent::unregistered(
            id.clone(),
            request.default_directory,
            AgentConnectionStatus::Stopped,
        );
        known.managed = true;
        known.configuration_editable = request.configuration_editable;
        known.ssh_target = request.ssh_target;
        self.agents.known_by_id.insert(id, known);
        self.notify_agents_changed();
        let _ = request.reply.send(());
    }

    /// Removes all retained state for a managed agent whose supervisor is already dormant.
    pub(crate) fn unregister_managed(&mut self, request: UnregisterManagedAgentRequest) {
        self.agents.known_by_id.remove(&request.agent_id);
        if let Some(connection) = self.agents.by_id.remove(&request.agent_id) {
            self.release_connection(connection, None);
        }
        self.notify_agents_changed();
        let _ = request.reply.send(());
    }

    /// Projects supervisor changes into retained inventory without touching streaming lanes.
    pub(crate) fn apply_managed_lifecycle(&mut self, request: ApplyManagedLifecycleRequest) {
        let ApplyManagedLifecycleRequest {
            agent_id,
            snapshot,
            evict_existing,
            reply,
        } = request;
        let managed = self
            .agents
            .known_by_id
            .get(&agent_id)
            .is_some_and(|known| known.managed);
        if !managed {
            if let Some(reply) = reply {
                let _ = reply.send(());
            }
            return;
        }

        if evict_existing && let Some(connection) = self.agents.by_id.remove(&agent_id) {
            self.forget_connection(&agent_id);
            self.release_connection(connection, Some("Managed agent startup was retried"));
        }

        let live_connection = self.agents.by_id.get(&agent_id);
        let live_socket = live_connection.map(|connection| connection.socket_id.clone());
        let transfer_ready = live_connection
            .and_then(|connection| connection.transfer.as_ref())
            .is_some();
        let known = self
            .agents
            .known_by_id
            .get_mut(&agent_id)
            .expect("managed agent disappeared during lifecycle projection");
        if live_socket.is_some() && snapshot.status != AgentConnectionStatus::Stopped {
            // Watchdog learns about control registration first; do not advertise Connected
            // until the payload socket is installed. Once Connected, transfer loss must not demote.
            if transfer_ready || known.status == AgentConnectionStatus::Connected {
                known.status = AgentConnectionStatus::Connected;
            } else if snapshot.status == AgentConnectionStatus::Connected {
                known.status = AgentConnectionStatus::Starting;
            } else {
                known.status = snapshot.status.clone();
            }
            known.connection_issue = snapshot.connection_issue;
            known.provisioning_status = snapshot.provisioning_status.clone();
        } else {
            known.status = snapshot.status.clone();
            known.connection_issue = snapshot.connection_issue;
            known.provisioning_status = snapshot.provisioning_status;
            known.socket_id = snapshot.socket_id;
            if known.status != AgentConnectionStatus::Connected {
                known.connected_at = None;
            }
        }

        if known.status == AgentConnectionStatus::Stopped
            && let Some(connection) = self.agents.by_id.remove(&agent_id)
        {
            known.last_seen_at = Some(now());
            known.socket_id = None;
            self.release_connection(connection, Some("Managed agent was shut down"));
        }
        self.notify_agents_changed();
        if let Some(reply) = reply {
            let _ = reply.send(());
        }
    }

    /// Projects every retained inventory entry for deterministic REST sorting.
    pub(crate) fn list_agents(&self) -> Vec<AgentListEntry> {
        self.agents
            .known_by_id
            .values()
            .map(|info| AgentListEntry {
                id: info.id.clone(),
                name: info.name.clone(),
                default_directory: info.default_directory.clone(),
                managed: info.managed,
                configuration_editable: info.configuration_editable,
                ssh_target: info.ssh_target.clone(),
                status: info.status.clone(),
                connected_at: info.connected_at,
                connection_id: info.socket_id.clone(),
                last_seen_at: info.last_seen_at,
                connection_issue: info.connection_issue.clone(),
                provisioning_status: info.provisioning_status.clone(),
                identity: info.identity.clone(),
                capabilities: info.capabilities,
            })
            .collect()
    }

    /// Allocates an internal request id and routes a one-shot command to an agent.
    pub(crate) fn execute_command_rest(&mut self, request: ExecuteCommandRequest) {
        if request.reply.is_closed() {
            return;
        }
        let request_id = self.next_id();
        log!(
            Level::Trace,
            "Routing REST command: agent_id={}, request_id={}, command={:?}",
            request.agent_id,
            request_id,
            request.command
        );
        let Some(agent_connection) = self.agents.by_id.get(&request.agent_id) else {
            let _ = request.reply.send(CommandResult::error(
                redoor_protocol::wire::command::CommandErrorKind::NotFound,
                format!("Agent not found: {}", request.agent_id),
            ));
            return;
        };
        let cancelable = request.command.policy().cancel_scope == CancelScope::Request;
        if !agent_connection.send_message(ServerToAgent::Command {
            agent_id: request.agent_id.clone(),
            request_id,
            command: request.command,
        }) {
            let _ = request.reply.send(CommandResult::error(
                redoor_protocol::wire::command::CommandErrorKind::ServiceUnavailable,
                format!("Agent control queue is full: {}", request.agent_id),
            ));
            return;
        }
        self.flows.insert(
            request_id,
            Flow::Command(CommandFlow {
                request_id,
                agent_id: request.agent_id.clone(),
                cancelable,
                reply: request.reply,
            }),
        );
        if request.admitted.send(request_id).is_err() {
            // The caller left between queueing and admission, so nobody can cancel it later.
            self.abandon_command(request_id, &request.agent_id);
        }
    }

    /// Drops a one-shot command whose caller gave up and stops it on the agent when the agent
    /// can stop it (request cancel scope); other commands are short or bounded by their own
    /// agent-side timeouts and simply finish unobserved.
    pub(crate) fn abandon_command(&mut self, flow_id: super::flows::FlowId, agent_id: &AgentId) {
        match self.flows.get(flow_id) {
            Some(Flow::Command(command)) if &command.agent_id == agent_id => {}
            _ => return,
        }
        let Some(Flow::Command(command)) = self.flows.remove(flow_id) else {
            return;
        };
        log!(
            Level::Debug,
            "Abandoned command: agent_id={}, request_id={}, cancelable={}",
            agent_id,
            command.request_id,
            command.cancelable
        );
        if command.cancelable
            && let Some(connection) = self.agents.by_id.get(agent_id)
        {
            // A retained delivery task finishes on its own; a closed lane means the
            // connection is gone and its teardown already stops the command.
            let _ = connection.queue_priority_cancel(command.request_id);
        }
    }

    /// Delivers a one-shot command's result to its REST caller.
    pub(crate) fn finish_command(
        &mut self,
        flow_id: super::flows::FlowId,
        response: RouteResponse,
    ) {
        // Only the agent the command was sent to may answer it; any other agent naming the
        // same request id must not resolve (or drop) the caller's reply.
        let owned = matches!(
            self.flows.get(flow_id),
            Some(Flow::Command(command)) if command.agent_id == response.agent_id
        );
        if !owned {
            log!(
                Level::Warning,
                "Command response agent mismatch: request_id={}, agent_id={}",
                response.request_id,
                response.agent_id
            );
            return;
        }
        if let Some(Flow::Command(command)) = self.flows.remove(flow_id) {
            let _ = command.reply.send(response.result);
        }
    }

    /// Queues only the log bootstrap secret on the existing control connection.
    pub(crate) fn open_log_stream(&self, request: OpenAgentLogStreamRequest) {
        let result = match self.agents.by_id.get(&request.agent_id) {
            Some(connection)
                if connection.send_message(ServerToAgent::LogStreamOpen {
                    log_stream_id: request.log_stream_id.clone(),
                    token: request.token.clone(),
                }) =>
            {
                Ok(())
            }
            Some(_) => Err(RouterError::ControlQueueFull {
                agent_id: request.agent_id.to_string(),
            }),
            None => Err(RouterError::AgentNotFound {
                agent_id: request.agent_id.to_string(),
            }),
        };
        let _ = request.reply.send(result);
    }

    /// Queues only the terminal bootstrap secret on the existing control connection.
    pub(crate) fn open_terminal(&self, request: OpenTerminalRequest) {
        let result = match self.agents.by_id.get(&request.agent_id) {
            Some(connection)
                if connection.send_message(ServerToAgent::TerminalOpen {
                    terminal_id: request.terminal_id.clone(),
                    token: request.token.clone(),
                    size: request.size,
                    cwd: request.cwd.clone(),
                }) =>
            {
                Ok(())
            }
            Some(_) => Err(RouterError::ControlQueueFull {
                agent_id: request.agent_id.to_string(),
            }),
            None => Err(RouterError::AgentNotFound {
                agent_id: request.agent_id.to_string(),
            }),
        };
        let _ = request.reply.send(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actors::lanes::ControlLanes;
    use crate::actors::router::state::RouterState;
    use crate::{log_registry::LogRegistry, terminal_registry::TerminalRegistry};
    use tokio::sync::{mpsc, oneshot, watch};

    /// Builds isolated router state without starting a router mailbox task.
    fn test_state() -> RouterState {
        let refresh_task = tokio::spawn(async {
            std::future::pending::<()>().await;
        });
        RouterState::new(refresh_task, TerminalRegistry::new(), LogRegistry::new())
    }

    /// Inserts one authoritative control connection and returns its issued transfer token.
    fn insert_control(state: &mut RouterState, agent_id: &str) -> String {
        let (lanes, _receivers) = ControlLanes::bounded(64, 16);
        let request = RegisterAgentRequest::for_test(
            &AgentId::from(agent_id),
            lanes,
            super::super::streams::ConnectionStreams::new(AgentId::from(agent_id), false),
        );
        state.commit_registration(request);
        state.agents.by_id[&AgentId::from(agent_id)]
            .transfer_token
            .clone()
    }

    /// Inserts a pending one-shot command owned by `agent_id` and returns its reply port.
    fn insert_command(
        state: &mut RouterState,
        id: u64,
        agent_id: &str,
    ) -> oneshot::Receiver<CommandResult> {
        let (reply, receiver) = oneshot::channel();
        let request_id = redoor_protocol::ids::RequestId::new(id);
        state.flows.insert(
            request_id,
            Flow::Command(CommandFlow {
                request_id,
                agent_id: AgentId::from(agent_id),
                cancelable: false,
                reply,
            }),
        );
        receiver
    }

    /// Builds the router message a session sends for an agent's `command_response`.
    fn response_message(agent_id: &str, socket_id: SocketId, id: u64) -> super::super::RouterMsg {
        super::super::RouterMsg::RouteResponse(RouteResponse {
            agent_id: AgentId::from(agent_id),
            socket_id,
            request_id: redoor_protocol::ids::RequestId::new(id),
            result: CommandResult::RawUpload,
        })
    }

    /// A response may only resolve a command when it arrives on the owning agent's current
    /// session; request ids are sequential, so anything else could forge results.
    #[tokio::test]
    async fn command_response_requires_owning_agent_and_current_session() {
        let mut state = test_state();
        insert_control(&mut state, "owner");
        insert_control(&mut state, "other");
        let owner_socket = state.agents.by_id[&AgentId::from("owner")]
            .socket_id
            .clone();
        let other_socket = state.agents.by_id[&AgentId::from("other")]
            .socket_id
            .clone();
        let mut reply = insert_command(&mut state, 7, "owner");

        // A session that never registered (or was replaced) cannot speak for the owner.
        state.handle(response_message("owner", SocketId::new(), 7));
        assert!(
            reply.try_recv().is_err(),
            "unknown session resolved a command"
        );
        // Another authenticated agent cannot answer a request it was never sent.
        state.handle(response_message("other", other_socket, 7));
        assert!(
            reply.try_recv().is_err(),
            "foreign agent resolved a command"
        );
        // The flow survives both attempts, so the real answer still reaches the caller.
        state.handle(response_message("owner", owner_socket, 7));
        assert!(matches!(reply.try_recv(), Ok(CommandResult::RawUpload)));
        state.ui.refresh_check_task.abort();
    }

    /// Verifies retry projection removes the old socket before reporting a fresh start.
    #[tokio::test]
    async fn retry_lifecycle_evicts_existing_managed_connection() {
        let mut state = test_state();
        let agent_id = AgentId::from("managed-retry");
        let (register_reply, register_receiver) = oneshot::channel();
        state.register_managed(RegisterManagedAgentRequest {
            agent_id: agent_id.clone(),
            default_directory: Some("/tmp".to_string()),
            configuration_editable: true,
            ssh_target: None,
            reply: register_reply,
        });
        register_receiver
            .await
            .expect("managed inventory registration acknowledged");
        insert_control(&mut state, "managed-retry");

        state.apply_managed_lifecycle(ApplyManagedLifecycleRequest {
            agent_id: agent_id.clone(),
            snapshot: crate::watchdog::WatchdogSnapshot {
                attempt_generation: 2,
                desired_running: true,
                status: AgentConnectionStatus::Starting,
                connection_issue: None,
                provisioning_status: Vec::new(),
                socket_id: None,
            },
            evict_existing: true,
            reply: None,
        });

        // REST success must not leave a dead socket available for routing or redirect.
        assert!(!state.agents.by_id.contains_key(&agent_id));
        // The retained managed row remains visible as the replacement attempt starts.
        assert_eq!(
            state.agents.known_by_id[&agent_id].status,
            AgentConnectionStatus::Starting
        );
        assert_eq!(state.agents.known_by_id[&agent_id].socket_id, None);
    }

    /// Creates a transfer registration request and exposes its shutdown observer.
    fn transfer_request(
        agent_id: &str,
        token: String,
        socket_id: redoor_protocol::ids::SocketId,
    ) -> (
        RegisterTransferConnectionRequest,
        oneshot::Receiver<Result<super::super::streams::ConnectionStreams, RouterError>>,
        watch::Receiver<bool>,
    ) {
        let (binary_sender, _binary_receiver) = mpsc::channel(1);
        let (shutdown, shutdown_receiver) = watch::channel(false);
        let (reply, reply_receiver) = oneshot::channel();
        (
            RegisterTransferConnectionRequest {
                agent_id: AgentId::from(agent_id),
                token,
                socket_id,
                outgoing_binary: binary_sender,
                shutdown,
                reply,
            },
            reply_receiver,
            shutdown_receiver,
        )
    }

    /// Verifies clients that wait for Connected never see it before the payload socket is usable.
    #[tokio::test]
    async fn connected_status_waits_for_transfer_socket() {
        let mut state = test_state();
        let token = insert_control(&mut state, "agent");
        // Control registration alone must not advertise full readiness.
        assert_eq!(
            state.agents.known_by_id[&AgentId::from("agent")].status,
            AgentConnectionStatus::Disconnected
        );
        assert!(
            state.agents.by_id[&AgentId::from("agent")]
                .transfer_connection()
                .is_err()
        );

        let socket_id = redoor_protocol::ids::SocketId::new();
        let (request, reply, _shutdown) = transfer_request("agent", token, socket_id);
        state.register_transfer(request);
        assert!(reply.await.expect("transfer reply delivered").is_ok());
        // Connected is reserved for the moment both sockets can serve work.
        assert_eq!(
            state.agents.known_by_id[&AgentId::from("agent")].status,
            AgentConnectionStatus::Connected
        );
        assert!(
            state.agents.by_id[&AgentId::from("agent")]
                .transfer_connection()
                .is_ok()
        );
    }

    /// Verifies only the current session token can attach and replacements shut down old sockets.
    #[tokio::test]
    async fn transfer_registration_authenticates_and_replaces_atomically() {
        let mut state = test_state();
        let token = insert_control(&mut state, "agent");
        let first_socket = redoor_protocol::ids::SocketId::new();
        let (wrong_request, wrong_reply, _wrong_shutdown) =
            transfer_request("agent", "wrong-token".to_string(), first_socket.clone());
        state.register_transfer(wrong_request);
        // Rejecting a wrong token proves stale or unauthenticated processes cannot attach.
        assert!(matches!(
            wrong_reply.await.expect("wrong-token reply delivered"),
            Err(RouterError::TransferAuthenticationFailed)
        ));
        // A failed attempt must leave the valid token usable rather than consuming it.
        assert!(
            state.agents.by_id[&AgentId::from("agent")]
                .transfer
                .is_none()
        );

        let (first_request, first_reply, first_shutdown) =
            transfer_request("agent", token.clone(), first_socket.clone());
        state.register_transfer(first_request);
        // The current session token must install the payload connection successfully.
        assert!(first_reply.await.expect("first reply delivered").is_ok());

        let replacement_socket = redoor_protocol::ids::SocketId::new();
        let (replacement_request, replacement_reply, _replacement_shutdown) =
            transfer_request("agent", token, replacement_socket.clone());
        state.register_transfer(replacement_request);
        // Replacement is acknowledged only after router state owns the new socket.
        assert!(
            replacement_reply
                .await
                .expect("replacement reply delivered")
                .is_ok()
        );
        // Replacing the socket must promptly stop both halves of the old connection.
        assert!(*first_shutdown.borrow());
        // The router must retain the replacement identity for stale teardown checks.
        assert_eq!(
            state.agents.by_id[&AgentId::from("agent")]
                .transfer
                .as_ref()
                .map(|transfer| transfer.socket_id.clone()),
            Some(replacement_socket)
        );
    }

    /// Verifies replacement fails payload-dependent work the delayed unregister would skip.
    #[tokio::test]
    async fn transfer_replacement_cleans_up_payload_requests() {
        let mut state = test_state();
        let token = insert_control(&mut state, "agent");
        let first_socket = redoor_protocol::ids::SocketId::new();
        let (first_request, first_reply, _first_shutdown) =
            transfer_request("agent", token.clone(), first_socket);
        state.register_transfer(first_request);
        assert!(first_reply.await.expect("first reply delivered").is_ok());

        let download_id = state.next_id();
        let upload_id = state.next_id();
        let connection = &state.agents.by_id[&AgentId::from("agent")];
        let link = connection.link();
        let download = super::super::streams::StreamLease::new(download_id, link.clone(), 0);
        let mut download_receiver = connection.streams.open_inbound(download.clone());
        state.flows.insert(
            download_id,
            Flow::Output(super::super::flows::OutputFlow {
                request_id: download_id,
                agent_id: AgentId::from("agent"),
                kind: super::super::flows::OutputKind::Download {
                    progress_id: download_id.as_transfer_id(),
                    base_bytes: 0,
                },
                lease: download,
                phase: super::super::flows::OutputPhase::Streaming,
            }),
        );

        let (completion_sender, completion_receiver) = oneshot::channel();
        state.flows.insert(
            upload_id,
            Flow::Upload(super::super::flows::UploadFlow {
                request_id: upload_id,
                agent_id: AgentId::from("agent"),
                kind: redoor_protocol::wire::policy::UploadKind::RawUpload,
                phase: super::super::flows::UploadPhase::Streaming,
                completion: Some(completion_sender),
                ready: None,
                stream: super::super::streams::OutboundStream::new(
                    super::super::streams::StreamLease::new(upload_id, link, 0),
                    None,
                ),
            }),
        );

        let replacement_socket = redoor_protocol::ids::SocketId::new();
        let (replacement_request, replacement_reply, _replacement_shutdown) =
            transfer_request("agent", token, replacement_socket);
        state.register_transfer(replacement_request);
        assert!(
            replacement_reply
                .await
                .expect("replacement reply delivered")
                .is_ok()
        );

        // Stale unregister is ignored after replacement, so cleanup must happen here.
        assert!(state.flows.id_for(download_id).is_none());
        assert!(state.flows.id_for(upload_id).is_none());
        // The download consumer observes the lost payload path as an error, not a clean end.
        assert!(
            download_receiver
                .recv()
                .await
                .is_some_and(|chunk| chunk.is_error)
        );
        // Waiting HTTP upload clients must observe the lost payload path.
        assert!(matches!(
            completion_receiver
                .await
                .expect("upload completion delivered"),
            Err(RouterError::TransferConnectionUnavailable { .. })
        ));
    }

    /// Verifies a timed-out socket handler cannot install dead channels or evict a live socket.
    #[tokio::test]
    async fn transfer_registration_ignores_cancelled_reply() {
        let mut state = test_state();
        let token = insert_control(&mut state, "agent");
        let live_socket = redoor_protocol::ids::SocketId::new();
        let (live_request, live_reply, _live_shutdown) =
            transfer_request("agent", token.clone(), live_socket.clone());
        state.register_transfer(live_request);
        assert!(live_reply.await.expect("live reply delivered").is_ok());

        let cancelled_socket = redoor_protocol::ids::SocketId::new();
        let (cancelled_request, cancelled_reply, cancelled_shutdown) =
            transfer_request("agent", token, cancelled_socket);
        // Dropping the reply models the socket handler's request timeout path,
        // which exits without ever sending UnregisterTransferConnection.
        drop(cancelled_reply);
        state.register_transfer(cancelled_request);

        // The live payload lane must survive; installing dead senders would
        // admit transfers against a socket task that already exited.
        assert_eq!(
            state.agents.by_id[&AgentId::from("agent")]
                .transfer
                .as_ref()
                .map(|transfer| transfer.socket_id.clone()),
            Some(live_socket)
        );
        // Signal the abandoned registration's half so any lingering task exits.
        assert!(*cancelled_shutdown.borrow());
    }

    /// Verifies unknown agents and stale unregister events cannot affect live payload state.
    #[tokio::test]
    async fn transfer_registration_rejects_unknown_and_ignores_stale_unregister() {
        let mut state = test_state();
        let unknown_socket = redoor_protocol::ids::SocketId::new();
        let (unknown_request, unknown_reply, _unknown_shutdown) =
            transfer_request("missing", "unknown-token".to_string(), unknown_socket);
        state.register_transfer(unknown_request);
        // Unknown identities use the same generic rejection as bad tokens to avoid disclosure.
        assert!(matches!(
            unknown_reply.await.expect("unknown reply delivered"),
            Err(RouterError::TransferAuthenticationFailed)
        ));

        let token = insert_control(&mut state, "agent");
        let current_socket = redoor_protocol::ids::SocketId::new();
        let (request, reply, _shutdown) = transfer_request("agent", token, current_socket.clone());
        state.register_transfer(request);
        // Setup must succeed before exercising stale teardown behavior.
        assert!(reply.await.expect("registration reply delivered").is_ok());
        state.unregister_transfer(
            AgentId::from("agent"),
            redoor_protocol::ids::SocketId::new(),
        );
        // An unrelated socket id must not clear the authoritative transfer connection.
        assert_eq!(
            state.agents.by_id[&AgentId::from("agent")]
                .transfer
                .as_ref()
                .map(|transfer| transfer.socket_id.clone()),
            Some(current_socket)
        );
    }

    /// Verifies command overload is explicit while priority cancellation remains admitted.
    #[tokio::test]
    async fn full_control_queue_rejects_commands_and_preserves_priority_lane() {
        let mut state = test_state();
        let agent_id = AgentId::from("bounded-agent");
        let (lanes, mut receivers) = ControlLanes::bounded(1, 1);
        state.commit_registration(RegisterAgentRequest::for_test(
            &agent_id,
            lanes,
            super::super::streams::ConnectionStreams::new(agent_id.clone(), false),
        ));
        // Remove the lifecycle bootstrap so the reserved lane is available for cancellation.
        assert!(matches!(
            receivers.priority.recv().await,
            Some(ServerToAgent::TransferSocketOpen { .. })
        ));

        let (first_reply, first_receiver) = oneshot::channel();
        let (first_admitted, _first_admitted) = oneshot::channel();
        state.execute_command_rest(ExecuteCommandRequest {
            agent_id: agent_id.clone(),
            command: redoor_protocol::wire::command::Command::GetAgentDetails,
            admitted: first_admitted,
            reply: first_reply,
        });
        // The admitted command owns exactly one pending reply while its frame fills the lane.
        assert_eq!(state.flows.len(), 1);
        drop(first_receiver);

        let (overload_reply, overload_receiver) = oneshot::channel();
        let (overload_admitted, _overload_admitted) = oneshot::channel();
        state.execute_command_rest(ExecuteCommandRequest {
            agent_id: agent_id.clone(),
            command: redoor_protocol::wire::command::Command::GetAgentDetails,
            admitted: overload_admitted,
            reply: overload_reply,
        });
        // Rejected commands never enter pending state, and the periodic tick prunes the
        // timed-out caller's reply port.
        assert_eq!(state.flows.len(), 1);
        state.flows.prune_abandoned_commands();
        assert_eq!(state.flows.len(), 0);
        assert!(matches!(
            overload_receiver.await.expect("overload reply delivered"),
            CommandResult::Error {
                kind: redoor_protocol::wire::command::CommandErrorKind::ServiceUnavailable,
                ..
            }
        ));

        let connection = &state.agents.by_id[&agent_id];
        assert!(
            connection.send_priority_message(ServerToAgent::CancelTransfer {
                request_id: redoor_protocol::ids::RequestId::new(99),
            })
        );
        // Priority admission succeeds despite the unread ordinary command frame.
        assert!(matches!(
            receivers.priority.recv().await,
            Some(ServerToAgent::CancelTransfer { .. })
        ));
        // The ordinary queue remains bounded to its single admitted frame.
        assert!(matches!(
            receivers.commands.recv().await,
            Some(ServerToAgent::Command {
                command: redoor_protocol::wire::command::Command::GetAgentDetails,
                ..
            })
        ));
    }
}
