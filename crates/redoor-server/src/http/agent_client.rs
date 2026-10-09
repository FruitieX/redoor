//! Typed request/response calls to connected agents.
//!
//! Every one-shot agent command goes through [`AgentClient::call`] with an [`AgentCall`] value
//! that knows its wire command, its deadline and which result variant it expects. Handlers
//! therefore never build router messages, pick timeouts or match "unexpected result" arms
//! themselves, and a caller that gives up always tells the agent to stop.

mod calls;

pub(crate) use calls::*;

use super::error::ApiError;
use crate::actors::router::{AgentListEntry, ExecuteCommandRequest, RouterHandle, RouterMsg};
use redoor_core::log;
use redoor_protocol::{
    ids::{AgentId, RequestId},
    wire::{
        command::{Command, CommandResult},
        log::Level,
    },
};
use std::time::Duration;
use tokio::sync::oneshot;

/// One typed agent command: what to send, how long to wait and how to read the answer.
pub(crate) trait AgentCall {
    /// Successful result of the call.
    type Output;
    /// Operator-facing description used when the agent answers with the wrong result.
    const OPERATION: &'static str;

    /// Deadline for the agent's answer; `None` for work whose duration is unbounded (a
    /// cross-device trash restore), which is still canceled when its caller goes away.
    fn timeout(&self) -> Option<Duration> {
        Some(STANDARD_TIMEOUT)
    }

    /// Builds the wire command.
    fn command(self) -> Command;

    /// Reads the successful result; typed agent failures are mapped before this is called.
    fn extract(result: CommandResult) -> Result<Self::Output, ApiError>;
}

/// Sends typed commands to agents through the router.
#[derive(Clone, Debug)]
pub(crate) struct AgentClient {
    router: RouterHandle,
}

impl AgentClient {
    /// Creates a client on the shared router.
    pub(crate) fn new(router: RouterHandle) -> Self {
        Self { router }
    }

    /// Runs one command on `agent_id` and returns its typed result.
    ///
    /// When the deadline passes (or the caller's future is dropped, e.g. by an HTTP
    /// disconnect) the router abandons the command and cancels it on the agent, so the agent
    /// stops working for a caller that is no longer there.
    pub(crate) async fn call<C: AgentCall>(
        &self,
        agent_id: &AgentId,
        call: C,
    ) -> Result<C::Output, ApiError> {
        let timeout = call.timeout();
        match self.execute(agent_id, call.command(), timeout).await? {
            CommandResult::Error { kind, message } => Err(ApiError::Command { kind, message }),
            result => C::extract(result),
        }
    }

    /// Retained inventory of every agent seen by this server, in router order.
    pub(crate) async fn inventory(&self) -> Result<Vec<AgentListEntry>, ApiError> {
        Ok(self
            .router
            .request(|reply| RouterMsg::GetAgentList { reply })
            .await?)
    }

    /// Cancels an admitted execution; `false` when no such execution belongs to the agent.
    pub(crate) async fn cancel_execution(
        &self,
        agent_id: AgentId,
        request_id: RequestId,
    ) -> Result<bool, ApiError> {
        Ok(self
            .router
            .request(|reply| RouterMsg::CancelExecution {
                agent_id,
                request_id,
                reply,
            })
            .await??)
    }

    /// Queues a command and waits for its final result under `timeout`.
    async fn execute(
        &self,
        agent_id: &AgentId,
        command: Command,
        timeout: Option<Duration>,
    ) -> Result<CommandResult, ApiError> {
        let (admitted, admitted_receiver) = oneshot::channel();
        let (reply, result) = oneshot::channel();
        self.router
            .send_async(RouterMsg::ExecuteCommandRest(ExecuteCommandRequest {
                agent_id: agent_id.clone(),
                command,
                admitted,
                reply,
            }))
            .await
            .map_err(|_| crate::actors::router::RouterCallError::RouterStopped)?;
        let mut pending = PendingCommand {
            router: self.router.clone(),
            agent_id: agent_id.clone(),
            admitted: admitted_receiver,
            armed: true,
        };
        let result = match timeout {
            Some(deadline) => match tokio::time::timeout(deadline, result).await {
                Ok(result) => result,
                Err(_) => {
                    log!(
                        Level::Warning,
                        "Agent command timed out: agent_id={}, timeout_ms={}",
                        agent_id,
                        deadline.as_millis()
                    );
                    return Err(ApiError::Timeout(deadline));
                }
            },
            None => result.await,
        };
        // Any answer, including the router dropping the reply, ends the command's lifetime.
        pending.armed = false;
        result.map_err(|_| crate::actors::router::RouterCallError::RouterStopped.into())
    }
}

/// Cancels a queued command whose caller stopped waiting before it answered.
struct PendingCommand {
    router: RouterHandle,
    agent_id: AgentId,
    /// Carries the request id once the router queued the command.
    admitted: oneshot::Receiver<RequestId>,
    armed: bool,
}

impl Drop for PendingCommand {
    /// Abandons the command in the router, which cancels it on the agent when possible.
    ///
    /// A command the router has not admitted yet needs nothing: the router sees the closed
    /// reply and never sends it, or sees the closed admission channel and abandons it itself.
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(request_id) = self.admitted.try_recv() else {
            return;
        };
        let router = self.router.clone();
        let agent_id = self.agent_id.clone();
        tokio::spawn(async move {
            // Waits for mailbox capacity: losing this would leave the agent working for nobody.
            let _ = router
                .send_async(RouterMsg::CancelTransfer {
                    agent_id,
                    request_id,
                })
                .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actors::router::{ConnectionStreams, RegisterAgentRequest, spawn_router};
    use crate::{log_registry::LogRegistry, terminal_registry::TerminalRegistry};
    use axum::extract::ws::Message as WsMessage;
    use axum::http::StatusCode;
    use redoor_protocol::ids::SocketId;
    use redoor_protocol::wire::ServerToAgent;
    use redoor_protocol::wire::command::CommandErrorKind;
    use tokio::sync::mpsc;

    /// A restore (request cancel scope) with a test-sized deadline.
    struct QuickRestore;

    impl AgentCall for QuickRestore {
        type Output = String;
        const OPERATION: &'static str = "restoring a test item";

        fn timeout(&self) -> Option<Duration> {
            Some(Duration::from_millis(50))
        }

        fn command(self) -> Command {
            Command::RestoreTrash {
                location_id: "l".to_string(),
                item_id: "i".to_string(),
                destination_path: "/tmp/restored".to_string(),
            }
        }

        fn extract(result: CommandResult) -> Result<String, ApiError> {
            RestoreTrash::extract(result)
        }
    }

    /// An echo (connection cancel scope) with a test-sized deadline.
    struct QuickEcho;

    impl AgentCall for QuickEcho {
        type Output = ();
        const OPERATION: &'static str = "echoing";

        fn timeout(&self) -> Option<Duration> {
            Some(Duration::from_millis(50))
        }

        fn command(self) -> Command {
            Command::Echo {
                request: redoor_protocol::api::agents::EchoRequest {
                    message: "hi".to_string(),
                    random_sleep: false,
                },
            }
        }

        fn extract(_result: CommandResult) -> Result<(), ApiError> {
            Ok(())
        }
    }

    /// A fake agent's control lanes, registered with a real router.
    struct FakeAgent {
        client: AgentClient,
        agent_id: AgentId,
        commands: mpsc::Receiver<WsMessage>,
        priority: mpsc::Receiver<WsMessage>,
    }

    impl FakeAgent {
        /// Registers one agent that never answers on its own.
        async fn connect() -> Self {
            let _ = redoor_core::logging::init(None).await;
            let (router, _task) = spawn_router(TerminalRegistry::new(), LogRegistry::new());
            let agent_id = AgentId::from("client-test");
            let (commands_sender, commands) = mpsc::channel(8);
            let (priority_sender, mut priority) = mpsc::channel(8);
            router
                .send_async(RouterMsg::RegisterAgent(RegisterAgentRequest {
                    agent_id: agent_id.clone(),
                    agent_name: "client-test".to_string(),
                    socket_id: SocketId::new(),
                    outgoing_commands: commands_sender,
                    outgoing_priority: priority_sender,
                    os: "linux".to_string(),
                    arch: "x86_64".to_string(),
                    hostname: "host".to_string(),
                    username: "user".to_string(),
                    default_directory: "/tmp".to_string(),
                    binary: redoor_core::identity::current_binary_identity(),
                    supports_self_exec: false,
                    supports_native_open: false,
                    supports_trash: true,
                    supports_move_to_trash: true,
                    uid: None,
                    is_root: false,
                    watchdog: None,
                    watchdog_attempt_generation: None,
                    streams: ConnectionStreams::new(agent_id.clone(), false),
                }))
                .await
                .unwrap();
            // Skip the transfer-socket bootstrap so the lane only carries what the test sends.
            assert!(priority.recv().await.is_some());
            Self {
                client: AgentClient::new(router),
                agent_id,
                commands,
                priority,
            }
        }

        /// Reads the next control frame of one lane.
        async fn next(lane: &mut mpsc::Receiver<WsMessage>) -> ServerToAgent {
            match lane.recv().await.expect("lane open") {
                WsMessage::Text(text) => serde_json::from_str(&text).unwrap(),
                other => panic!("unexpected frame: {other:?}"),
            }
        }

        /// Answers the next queued command with `result`.
        async fn answer(&mut self, result: CommandResult) {
            let ServerToAgent::Command { request_id, .. } = Self::next(&mut self.commands).await
            else {
                panic!("expected a command");
            };
            self.client
                .router
                .send_async(RouterMsg::RouteResponse(
                    crate::actors::router::RouteResponse {
                        agent_id: self.agent_id.clone(),
                        request_id,
                        result,
                    },
                ))
                .await
                .unwrap();
        }
    }

    /// Typed agent failures and wrong result variants become the matching API errors.
    #[tokio::test]
    async fn extract_maps_agent_failures_and_protocol_mismatches() {
        let mut agent = FakeAgent::connect().await;
        let client = agent.client.clone();
        let id = agent.agent_id.clone();
        let call = tokio::spawn(async move {
            client
                .call(
                    &id,
                    Metadata {
                        path: "/missing".to_string(),
                    },
                )
                .await
        });
        agent
            .answer(CommandResult::error(
                CommandErrorKind::NotFound,
                "No such file",
            ))
            .await;
        let error = call.await.unwrap().unwrap_err();
        // The agent's typed failure keeps its status and message without Debug noise.
        assert_eq!(error.status_code(), StatusCode::NOT_FOUND);
        assert_eq!(error.to_string(), "No such file");

        let client = agent.client.clone();
        let id = agent.agent_id.clone();
        let call = tokio::spawn(async move {
            client
                .call(
                    &id,
                    Metadata {
                        path: "/x".to_string(),
                    },
                )
                .await
        });
        agent.answer(CommandResult::RawUpload).await;
        let error = call.await.unwrap().unwrap_err();
        // A result for a different command is a protocol mismatch, reported once, as a 500.
        assert!(
            matches!(error, ApiError::Unexpected(operation) if operation == Metadata::OPERATION)
        );
        assert_eq!(error.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// A cancelable command whose deadline passes is canceled on the agent.
    #[tokio::test]
    async fn timeout_cancels_request_scoped_command() {
        let mut agent = FakeAgent::connect().await;
        let error = agent
            .client
            .call(&agent.agent_id, QuickRestore)
            .await
            .unwrap_err();
        // The caller learns the agent was too slow.
        assert!(matches!(error, ApiError::Timeout(_)));
        let ServerToAgent::Command { request_id, .. } = FakeAgent::next(&mut agent.commands).await
        else {
            panic!("expected the restore command");
        };
        // The agent is told to stop exactly the abandoned request.
        assert!(matches!(
            FakeAgent::next(&mut agent.priority).await,
            ServerToAgent::CancelTransfer { request_id: canceled } if canceled == request_id
        ));
    }

    /// Commands the agent cannot cancel are only forgotten, never sent a pointless cancel.
    #[tokio::test]
    async fn timeout_forgets_connection_scoped_command() {
        let mut agent = FakeAgent::connect().await;
        let error = agent
            .client
            .call(&agent.agent_id, QuickEcho)
            .await
            .unwrap_err();
        // The deadline still bounds the caller.
        assert!(matches!(error, ApiError::Timeout(_)));
        // A follow-up command proves the router processed the abandonment first.
        let client = agent.client.clone();
        let id = agent.agent_id.clone();
        let follow_up = tokio::spawn(async move { client.call(&id, ListAccounts).await });
        let _echo = FakeAgent::next(&mut agent.commands).await;
        agent
            .answer(CommandResult::error(CommandErrorKind::Internal, "done"))
            .await;
        assert!(follow_up.await.unwrap().is_err());
        // The echo was abandoned without a cancel frame on the priority lane.
        assert!(agent.priority.try_recv().is_err());
    }
}
