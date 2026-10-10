//! A scripted agent connected to a real router, for unit tests of the HTTP service layer.

use crate::actors::lanes::ControlLanes;
use crate::actors::router::{
    ConnectionStreams, RegisterAgentRequest, RouteResponse, RouterHandle, RouterMsg, spawn_router,
};
use redoor_protocol::{
    ids::{AgentId, RequestId, SocketId},
    wire::{ServerToAgent, command::Command, command::CommandResult, stream::StreamChunk},
};
use tokio::sync::mpsc;

/// The socket-facing ends of one registered agent: what the server queued for it, and the
/// stream tables its connection reader would deliver frames into.
pub(crate) struct FakeAgent {
    pub(crate) router: RouterHandle,
    pub(crate) agent_id: AgentId,
    /// Control session the agent registered on; the router only accepts its frames from it.
    socket_id: SocketId,
    pub(crate) commands: mpsc::Receiver<ServerToAgent>,
    pub(crate) priority: mpsc::Receiver<ServerToAgent>,
    /// Kept open so server-to-agent payload frames have somewhere to go.
    _bulk: mpsc::Receiver<StreamChunk>,
    pub(crate) streams: ConnectionStreams,
}

impl FakeAgent {
    /// Starts a router and registers one agent.
    pub(crate) async fn connect() -> Self {
        let (router, _task) = spawn_router();
        let agent_id = AgentId::from("fake-agent");
        let (lanes, receivers) = ControlLanes::bounded(16, 16);
        let (commands, mut priority) = (receivers.commands, receivers.priority);
        let streams = ConnectionStreams::new(agent_id.clone());
        let registration = RegisterAgentRequest::for_test(&agent_id, lanes, streams.clone());
        let socket_id = registration.socket_id.clone();
        router
            .send_async(RouterMsg::RegisterAgent(registration))
            .await
            .unwrap();
        let ServerToAgent::Welcome { .. } = Self::next(&mut priority).await else {
            panic!("registration starts with the welcome");
        };
        Self {
            router,
            agent_id,
            socket_id,
            commands,
            priority,
            _bulk: receivers.bulk,
            streams,
        }
    }

    /// Reads the next control frame of one lane.
    pub(crate) async fn next(lane: &mut mpsc::Receiver<ServerToAgent>) -> ServerToAgent {
        lane.recv().await.expect("lane open")
    }

    /// Reads the next command the server sent.
    pub(crate) async fn next_command(&mut self) -> (RequestId, Command) {
        match Self::next(&mut self.commands).await {
            ServerToAgent::Command { id, command } => (id, command),
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
                socket_id: self.socket_id.clone(),
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

    /// Delivers one stream frame through the connection reader's routing.
    pub(crate) async fn deliver(&self, chunk: StreamChunk) {
        if let Some(end) = self.streams.deliver(chunk) {
            self.router
                .send_async(RouterMsg::InboundStreamEnded(end))
                .await
                .unwrap();
        }
    }
}
