//! A scripted agent connected to a real router, for unit tests of the HTTP service layer.

use crate::actors::lanes::ControlLanes;
use crate::actors::router::{
    ConnectionStreams, RegisterAgentRequest, RegisterTransferConnectionRequest, RouteResponse,
    RouterHandle, RouterMsg, spawn_router,
};
use crate::{log_registry::LogRegistry, terminal_registry::TerminalRegistry};
use redoor_protocol::{
    ids::{AgentId, RequestId, SocketId},
    wire::{ServerToAgent, command::Command, command::CommandResult, stream::StreamChunk},
};
use tokio::sync::{mpsc, watch};

/// The socket-facing ends of one registered agent: what the server queued for it, and the
/// stream tables its transfer reader would deliver frames into.
pub(crate) struct FakeAgent {
    pub(crate) router: RouterHandle,
    pub(crate) agent_id: AgentId,
    pub(crate) commands: mpsc::Receiver<ServerToAgent>,
    pub(crate) priority: mpsc::Receiver<ServerToAgent>,
    /// Kept open so server-to-agent payload frames have somewhere to go.
    _transfer: mpsc::Receiver<StreamChunk>,
    pub(crate) streams: ConnectionStreams,
    _transfer_shutdown: watch::Receiver<bool>,
}

impl FakeAgent {
    /// Starts a router and registers one uncredited agent with control and transfer sockets.
    pub(crate) async fn connect() -> Self {
        let (router, _task) = spawn_router(TerminalRegistry::new(), LogRegistry::new());
        let agent_id = AgentId::from("fake-agent");
        let (lanes, receivers) = ControlLanes::bounded(16, 16);
        let (commands, mut priority) = (receivers.commands, receivers.priority);
        let (transfer_sender, transfer) = mpsc::channel(16);
        let streams = ConnectionStreams::new(agent_id.clone(), false);
        router
            .send_async(RouterMsg::RegisterAgent(RegisterAgentRequest::for_test(
                &agent_id,
                lanes,
                streams.clone(),
            )))
            .await
            .unwrap();
        let ServerToAgent::TransferSocketOpen { token, .. } = Self::next(&mut priority).await
        else {
            panic!("registration starts with the transfer bootstrap");
        };
        let (shutdown, transfer_shutdown) = watch::channel(false);
        router
            .request(|reply| {
                RouterMsg::RegisterTransferConnection(RegisterTransferConnectionRequest {
                    agent_id: agent_id.clone(),
                    token,
                    socket_id: SocketId::new(),
                    outgoing_binary: transfer_sender,
                    shutdown,
                    reply,
                })
            })
            .await
            .unwrap()
            .unwrap();
        Self {
            router,
            agent_id,
            commands,
            priority,
            _transfer: transfer,
            streams,
            _transfer_shutdown: transfer_shutdown,
        }
    }

    /// Reads the next control frame of one lane.
    pub(crate) async fn next(lane: &mut mpsc::Receiver<ServerToAgent>) -> ServerToAgent {
        lane.recv().await.expect("lane open")
    }

    /// Reads the next command the server sent.
    pub(crate) async fn next_command(&mut self) -> (RequestId, Command) {
        match Self::next(&mut self.commands).await {
            ServerToAgent::Command {
                request_id,
                command,
                ..
            } => (request_id, command),
            other => panic!("expected a command, got {other:?}"),
        }
    }

    /// Reads the next priority frame (cancels, credit grants).
    pub(crate) async fn next_priority(&mut self) -> ServerToAgent {
        Self::next(&mut self.priority).await
    }

    /// Sends the final result of `request_id` as the agent would.
    pub(crate) async fn respond(&self, request_id: RequestId, result: CommandResult) {
        self.router
            .send_async(RouterMsg::RouteResponse(RouteResponse {
                agent_id: self.agent_id.clone(),
                request_id,
                result,
            }))
            .await
            .unwrap();
    }

    /// Answers the next queued command with `result`.
    pub(crate) async fn answer(&mut self, result: CommandResult) {
        let (request_id, _) = self.next_command().await;
        self.respond(request_id, result).await;
    }

    /// Delivers one stream frame through the transfer reader's routing.
    pub(crate) async fn deliver(&self, chunk: StreamChunk) {
        if let Some(end) = self.streams.deliver(chunk).await {
            self.router
                .send_async(RouterMsg::InboundStreamEnded(end))
                .await
                .unwrap();
        }
    }
}
