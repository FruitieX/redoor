//! Admission, cancellation, task ownership and responses for every command.
//!
//! The dispatcher reads [`Command::policy`] once and applies it uniformly, so a new command only
//! needs a policy entry and a [`RunCommand`] arm; it cannot forget to register for cancel,
//! respond, or be joined on teardown.

use super::cancel::{RegisterError, Registration};
use super::responder::{ControlSender, Responder};
use super::sessions::Sessions;
use crate::commands::RunCommand;
use crate::commands::ctx::{AgentServices, CommandCtx, Reply};
use redoor_core::{log, log_error};
use redoor_protocol::{
    ids::{AgentId, RequestId},
    wire::{
        command::{Command, CommandErrorKind, CommandResult},
        log::Level,
        policy::{CancelScope, CommandPolicy},
    },
};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tokio_util::sync::CancellationToken;

/// Caps independently executing non-upload commands for one control generation.
const MAX_CONCURRENT_COMMANDS: usize = 32;

/// Connection resources a command may use, captured when it is dispatched.
pub(crate) struct DispatchLink<'a> {
    pub(crate) agent_id: &'a AgentId,
    pub(crate) control: &'a ControlSender,
    pub(crate) transfer: Option<&'a mpsc::Sender<WsMessage>>,
    /// Cancelled when the control connection ends.
    pub(crate) scope: &'a CancellationToken,
}

/// Owns every running command of the current control connection.
pub(crate) struct Dispatcher {
    /// Ordinary commands, bounded by [`MAX_CONCURRENT_COMMANDS`].
    commands: JoinSet<()>,
    /// Upload workers, bounded by the server's transfer admission rather than this cap.
    uploads: JoinSet<()>,
    /// Serializes commands whose policy is `exclusive`.
    exclusive: Arc<Mutex<()>>,
    services: AgentServices,
}

impl Dispatcher {
    /// Creates a dispatcher with no running work.
    pub(crate) fn new(services: AgentServices) -> Self {
        Self {
            commands: JoinSet::new(),
            uploads: JoinSet::new(),
            exclusive: Arc::new(Mutex::new(())),
            services,
        }
    }

    /// Shared facilities, also used by the transfer socket reader.
    pub(crate) fn services(&self) -> &AgentServices {
        &self.services
    }

    /// Admits, registers and spawns one command, answering immediately when it cannot run.
    pub(crate) async fn dispatch(
        &mut self,
        request_id: RequestId,
        command: Command,
        link: DispatchLink<'_>,
        sessions: &mut Sessions,
    ) {
        let canceled_early = sessions.take_pending_cancel(request_id);
        log!(
            Level::Info,
            "Command received: agent_id={}, request_id={}, command={}",
            link.agent_id,
            request_id,
            command.summary()
        );
        let policy = command.policy();
        let responder = Responder::new(link.control.clone(), link.agent_id.clone(), request_id);
        let transfer = link.transfer.cloned();
        if policy.requires_transfer() && transfer.is_none() {
            let _ = responder
                .respond(CommandResult::error(
                    CommandErrorKind::Internal,
                    "Transfer connection unavailable",
                ))
                .await;
            return;
        }
        let is_upload = policy.upload_kind().is_some();
        if !is_upload && !self.command_capacity_available() {
            let _ = responder
                .respond(CommandResult::error(
                    CommandErrorKind::ServiceUnavailable,
                    "Agent command execution limit reached",
                ))
                .await;
            return;
        }
        let (cancel, registration) =
            match Self::cancellation(&policy, request_id, canceled_early, &link, sessions) {
                Ok(cancellation) => cancellation,
                Err(RegisterError::Duplicate | RegisterError::Full) => {
                    let _ = responder
                        .respond(CommandResult::error(
                            CommandErrorKind::AlreadyExists,
                            format!("Request is already running for request_id={request_id}"),
                        ))
                        .await;
                    return;
                }
            };

        let ctx = CommandCtx {
            responder,
            cancel,
            registration,
            transfer,
            services: self.services.clone(),
        };
        let task = Self::execute(command, policy, ctx, self.exclusive.clone());
        if is_upload {
            self.uploads.spawn(task);
        } else {
            self.commands.spawn(task);
        }
    }

    /// Chooses the command's cancellation token, registering request-scoped commands.
    fn cancellation(
        policy: &CommandPolicy,
        request_id: RequestId,
        canceled_early: bool,
        link: &DispatchLink<'_>,
        sessions: &mut Sessions,
    ) -> Result<(CancellationToken, Option<Registration<RequestId>>), RegisterError> {
        match policy.cancel_scope {
            CancelScope::Request => {
                let registration = sessions.register_request(request_id, canceled_early)?;
                Ok((registration.token().clone(), Some(registration)))
            }
            CancelScope::Connection | CancelScope::Uninterruptible => {
                Ok((link.scope.child_token(), None))
            }
        }
    }

    /// Runs one command inside its task and delivers its reply.
    async fn execute(
        command: Command,
        policy: CommandPolicy,
        ctx: CommandCtx,
        exclusive: Arc<Mutex<()>>,
    ) {
        let responder = ctx.responder.clone();
        let abort_on = (policy.cancel_scope == CancelScope::Connection).then(|| ctx.cancel.clone());
        let _exclusive_slot = if policy.exclusive {
            Some(exclusive.lock_owned().await)
        } else {
            None
        };
        let reply = match abort_on {
            // Abortable commands are dropped mid-flight when their connection ends.
            Some(token) => match token.run_until_cancelled(command.run(ctx)).await {
                Some(reply) => reply,
                None => return,
            },
            None => command.run(ctx).await,
        };
        Self::deliver(&responder, reply).await;
    }

    /// Sends the reply on the control lane according to its delivery mode.
    async fn deliver(responder: &Responder, reply: Reply) {
        match reply {
            Reply::Result(result) => {
                let _ = responder.respond(result).await;
            }
            Reply::Streamed => {}
            Reply::Fallback { result, abandon } => {
                tokio::select! {
                    // Request cancellation does not invalidate an already known outcome; only
                    // teardown of the connection that would carry it may abandon delivery.
                    _ = abandon.cancelled() => {}
                    _ = responder.respond(result) => {}
                }
            }
        }
    }

    /// Reaps finished commands and reports whether another bounded command can start.
    fn command_capacity_available(&mut self) -> bool {
        Self::reap(&mut self.commands);
        Self::reap(&mut self.uploads);
        self.commands.len() < MAX_CONCURRENT_COMMANDS
    }

    /// Collects finished tasks so panics are logged and the sets do not grow.
    fn reap(tasks: &mut JoinSet<()>) {
        while let Some(result) = tasks.try_join_next() {
            if let Err(error) = result {
                log_error!(anyhow::Error::new(error), "Agent command task failed");
            }
        }
    }

    /// Joins every command after the connection scope was cancelled.
    pub(crate) async fn stop(&mut self) {
        for tasks in [&mut self.commands, &mut self.uploads] {
            while let Some(result) = tasks.join_next().await {
                if let Err(error) = result {
                    log!(
                        Level::Warning,
                        "Agent command task failed while stopping: {error}"
                    );
                }
            }
        }
    }

    /// Reports whether any command task is still owned, for teardown tests.
    #[cfg(test)]
    pub(crate) fn is_idle(&self) -> bool {
        self.commands.is_empty() && self.uploads.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::responder::{OutboundFrame, PostSend};
    use crate::transfers::upload::UploadRoutes;
    use crate::trash::Trash;
    use redoor_protocol::{
        api::{agents::EchoRequest, transfers::CopyExistingMode},
        wire::AgentToServer,
    };

    /// Owns a dispatcher with recording control and transfer lanes.
    struct Fixture {
        dispatcher: Dispatcher,
        sessions: Sessions,
        agent_id: AgentId,
        control: ControlSender,
        frames: mpsc::Receiver<OutboundFrame>,
        transfer: Option<mpsc::Sender<WsMessage>>,
        scope: CancellationToken,
    }

    impl Fixture {
        /// Builds a dispatcher bound to a live scope; `with_transfer` attaches a payload lane.
        async fn new(with_transfer: bool) -> (Self, Option<mpsc::Receiver<WsMessage>>) {
            redoor_core::logging::init(None).await.unwrap();
            let routes = UploadRoutes::default();
            let services = AgentServices::new(Trash::unsupported());
            let (control, frames) = mpsc::channel(8);
            let (transfer, payload) = if with_transfer {
                let (sender, receiver) = mpsc::channel(8);
                (Some(sender), Some(receiver))
            } else {
                (None, None)
            };
            let scope = CancellationToken::new();
            let mut sessions = Sessions::new(routes);
            sessions.rebind(&scope);
            (
                Self {
                    dispatcher: Dispatcher::new(services),
                    sessions,
                    agent_id: AgentId::from("agent"),
                    control,
                    frames,
                    transfer,
                    scope,
                },
                payload,
            )
        }

        /// Dispatches one command over the fixture's lanes.
        async fn dispatch(&mut self, request_id: u64, command: Command) {
            let link = DispatchLink {
                agent_id: &self.agent_id,
                control: &self.control,
                transfer: self.transfer.as_ref(),
                scope: &self.scope,
            };
            self.dispatcher
                .dispatch(
                    RequestId::new(request_id),
                    command,
                    link,
                    &mut self.sessions,
                )
                .await;
        }

        /// Receives the next command response from the recorded control lane.
        async fn next_response(&mut self) -> (CommandResult, Option<PostSend>) {
            loop {
                let frame = self.frames.recv().await.expect("control lane open");
                if let Some(AgentToServer::CommandResponse { result, .. }) = frame.as_message() {
                    return (result.clone(), frame.after_send.clone());
                }
            }
        }
    }

    /// A download that cannot finish until cancelled, used to occupy a request id.
    fn blocking_download() -> Command {
        Command::RawDownload {
            path: "/dev/zero".to_string(),
            range_start: None,
            range_end: None,
        }
    }

    /// Verifies the dispatcher, not the command, sends the typed response.
    #[tokio::test]
    async fn control_commands_respond_through_the_dispatcher() {
        let (mut fixture, _) = Fixture::new(false).await;

        fixture
            .dispatch(
                1,
                Command::Echo {
                    request: EchoRequest {
                        message: "hello".to_string(),
                        random_sleep: false,
                    },
                },
            )
            .await;
        let (result, after_send) = fixture.next_response().await;

        // The dispatcher, not the command, sends the typed response.
        assert!(matches!(result, CommandResult::Echo(echo) if echo.message == "hello"));
        // Ordinary results never replace the process.
        assert_eq!(after_send, None);
    }

    /// Verifies restart responses carry their post-send re-exec.
    #[tokio::test]
    async fn restart_response_carries_the_reexec_action() {
        let (mut fixture, _) = Fixture::new(false).await;

        fixture.dispatch(2, Command::Restart).await;
        let (result, after_send) = fixture.next_response().await;

        // Restart answers first so the router sees success before the socket drops.
        assert!(matches!(result, CommandResult::Restart));
        // The writer, not a JSON sniffer, performs the re-exec after this frame is written.
        assert_eq!(after_send, Some(PostSend::Reexec { path: None }));
    }

    /// Verifies transfer-lane commands fail fast without a transfer socket.
    #[tokio::test]
    async fn streaming_commands_require_a_transfer_link() {
        let (mut fixture, _) = Fixture::new(false).await;

        fixture.dispatch(3, blocking_download()).await;
        let (result, _) = fixture.next_response().await;

        // Admission fails fast instead of starting a stream with nowhere to write.
        assert!(matches!(
            result,
            CommandResult::Error { kind: CommandErrorKind::Internal, message } if message == "Transfer connection unavailable"
        ));
        // Rejected commands never register for cancellation.
        assert!(!fixture.sessions.requests.contains(&RequestId::new(3)));
    }

    /// Verifies a cancel that overtook its command still stops it.
    #[tokio::test]
    async fn early_cancel_stops_the_command_when_it_registers() {
        let (mut fixture, payload) = Fixture::new(true).await;
        let mut payload = payload.unwrap();

        fixture.sessions.cancel_request(RequestId::new(4));
        fixture.dispatch(4, blocking_download()).await;
        let frame = payload
            .recv()
            .await
            .expect("download reports on its stream");
        let WsMessage::Binary(bytes) = frame else {
            panic!("downloads use binary frames");
        };
        let chunk = redoor_protocol::wire::stream::StreamChunk::from_bytes(&bytes).unwrap();

        // The overtaking cancel ends the stream with the cancellation frame.
        assert!(chunk.is_error);
        assert_eq!(chunk.data, crate::transfers::stream::CANCELED_MESSAGE);
    }

    /// Verifies one request id cannot own two cancellation registrations.
    #[tokio::test]
    async fn duplicate_request_ids_are_rejected() {
        let (mut fixture, payload) = Fixture::new(true).await;

        fixture.dispatch(5, blocking_download()).await;
        fixture
            .dispatch(
                5,
                Command::LocalCopyFile {
                    source_path: "/nonexistent".to_string(),
                    dest_path: "/nonexistent-copy".to_string(),
                    on_existing: CopyExistingMode::Error,
                },
            )
            .await;
        let (result, _) = fixture.next_response().await;

        // One request id may own only one cancellation registration.
        assert!(matches!(
            result,
            CommandResult::Error {
                kind: CommandErrorKind::AlreadyExists,
                ..
            }
        ));
        // Teardown closes the transfer writer, which releases the blocked download.
        drop(payload);
        fixture.scope.cancel();
        fixture.sessions.invalidate();
        fixture.dispatcher.stop().await;
    }

    /// Verifies stalled commands cannot grow the task set without bound.
    #[tokio::test]
    async fn capacity_limit_rejects_excess_commands() {
        let (mut fixture, _) = Fixture::new(false).await;
        for _ in 0..MAX_CONCURRENT_COMMANDS {
            fixture
                .dispatcher
                .commands
                .spawn(std::future::pending::<()>());
        }

        fixture.dispatch(6, Command::GetLoggingLevel).await;
        let (result, _) = fixture.next_response().await;

        // A full set rejects ordinary commands without blocking control handling.
        assert!(matches!(
            result,
            CommandResult::Error {
                kind: CommandErrorKind::ServiceUnavailable,
                ..
            }
        ));
        fixture.dispatcher.commands.abort_all();
    }

    /// Verifies teardown cancels and joins every running command.
    #[tokio::test]
    async fn teardown_cancels_and_joins_running_commands() {
        let (mut fixture, payload) = Fixture::new(true).await;
        fixture.dispatch(7, blocking_download()).await;
        fixture
            .dispatch(
                8,
                Command::FileSearch {
                    path: "/".to_string(),
                    query: "unlikely-redoor-query".to_string(),
                    timeout_seconds: 3600,
                    include_hidden: true,
                    respect_gitignore: false,
                    case_sensitivity: Default::default(),
                },
            )
            .await;

        // Teardown closes the transfer writer before joining, as the runtime does.
        drop(payload);
        fixture.scope.cancel();
        fixture.sessions.invalidate();
        fixture.dispatcher.stop().await;

        // No command task may outlive the connection that admitted it.
        assert!(fixture.dispatcher.is_idle());
    }

    /// Verifies retained fallback acknowledgements are released only by teardown.
    #[tokio::test]
    async fn fallback_survives_request_cancel_but_not_teardown() {
        redoor_core::logging::init(None).await.unwrap();
        let (responder, mut frames) = Responder::recording(RequestId::new(9), 1);
        // Occupy the control lane so delivery has to wait.
        responder.transfer_ready().await.unwrap();
        let abandon = CancellationToken::new();
        let delivery = Dispatcher::deliver(
            &responder,
            Reply::Fallback {
                result: CommandResult::RawUpload,
                abandon: abandon.clone(),
            },
        );
        tokio::pin!(delivery);

        // A blocked control lane retains the acknowledgement.
        assert!(futures_util::poll!(&mut delivery).is_pending());
        frames.recv().await.unwrap();
        delivery.await;
        // Once capacity frees up, the retained acknowledgement is delivered.
        assert!(matches!(
            frames.recv().await.unwrap().as_message(),
            Some(AgentToServer::CommandResponse { .. })
        ));

        responder.transfer_ready().await.unwrap();
        let delivery = Dispatcher::deliver(
            &responder,
            Reply::Fallback {
                result: CommandResult::RawUpload,
                abandon: abandon.clone(),
            },
        );
        tokio::pin!(delivery);
        assert!(futures_util::poll!(&mut delivery).is_pending());
        abandon.cancel();
        // Teardown of the carrying connection releases the worker without delivering.
        assert!(futures_util::poll!(&mut delivery).is_ready());
    }
}
