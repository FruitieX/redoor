//! The agent's single event loop: connection lifecycle, frame handling and teardown.

pub(crate) mod cancel;
pub(crate) mod dispatch;
pub(crate) mod links;
pub(crate) mod responder;
pub(crate) mod sessions;

use crate::commands::ctx::AgentServices;
use crate::connection::AgentConnection;
use crate::platform::identity::HostIdentity;
use crate::transfer::{
    TransferConnectContext, schedule_transfer_reconnect, spawn_transfer_connection,
};
use crate::ws::{spawn_read_task, spawn_stdin_task, spawn_write_task};
use dispatch::{DispatchLink, Dispatcher};
use futures_util::StreamExt;
use links::{ControlLink, TransferLink};
use redoor_core::{log, log_error};
use redoor_protocol::wire::registration::{AgentCapabilities, AgentIdentity};
use redoor_protocol::{
    ids::AgentId,
    wire::{
        AgentToServer, ServerToAgent,
        command::{CommandErrorKind, CommandResult},
        log::{Level, LogStreamId},
        terminal::{TerminalId, TerminalSize},
    },
};
use responder::{OutboundFrame, Responder};
use sessions::Sessions;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;

/// Bounded control-lane depth; full queues backpressure responders instead of dropping frames.
const CONTROL_QUEUE_CAPACITY: usize = 32;
/// Number of quick reconnect attempts allowed before switching to the slower window.
const QUICK_RECONNECT_ATTEMPTS: u32 = 10;
/// Inclusive jitter window for quick reconnect attempts.
const QUICK_RECONNECT_DELAY_SECONDS: std::ops::RangeInclusive<u64> = 2..=5;
/// Inclusive jitter window used after quick reconnect attempts are exhausted.
const SLOW_RECONNECT_DELAY_SECONDS: std::ops::RangeInclusive<u64> = 30..=60;

/// Returns the retry window for an attempt so outages back off without synchronizing agents.
fn reconnect_delay_range(attempt: u32) -> std::ops::RangeInclusive<u64> {
    if attempt <= QUICK_RECONNECT_ATTEMPTS {
        QUICK_RECONNECT_DELAY_SECONDS
    } else {
        SLOW_RECONNECT_DELAY_SECONDS
    }
}

/// Selects one jittered reconnect delay from the window assigned to an attempt.
fn reconnect_delay(attempt: u32) -> tokio::time::Duration {
    tokio::time::Duration::from_secs(fastrand::u64(reconnect_delay_range(attempt)))
}

/// Internal events consumed by the single agent runtime actor.
pub(crate) enum AgentMsg {
    Connect,
    /// One text frame read from the control socket of `connection_generation`.
    WebSocketMessage {
        connection_generation: u64,
        text: String,
    },
    /// Carries a oneshot so setup can wait until the transfer writer is installed before
    /// authenticating, which prevents the server from admitting transfers too early.
    TransferConnected {
        control_generation: u64,
        transfer_generation: u64,
        token: String,
        sender: mpsc::Sender<WsMessage>,
        shutdown: watch::Sender<bool>,
        /// Reports whether the actor accepted this generation so auth cannot race readiness.
        installed: oneshot::Sender<bool>,
    },
    TransferConnectionLost {
        control_generation: u64,
        transfer_generation: u64,
        reason: String,
    },
    ReconnectTransfer {
        control_generation: u64,
        transfer_generation: u64,
        token: String,
    },
    /// Delays desktop UI work and carries socket generations so stale timers are harmless.
    StartupNotificationDue {
        connection_generation: u64,
        transfer_generation: u64,
    },
    /// Operator text from stdin forwarded verbatim for manual protocol debugging.
    SendRawText {
        text: String,
    },
    /// Stops the runtime cleanly when its owning transport has ended.
    Shutdown,
    ExitWithError,
}

/// Lifecycle events bypass ordinary message backpressure so transport recovery cannot be starved.
pub(crate) enum AgentLifecycleMsg {
    /// Discards a failed control transport only while its generation remains authoritative.
    ConnectionLost {
        connection_generation: u64,
        reason: String,
    },
}

/// Runtime handle used to send control events into the agent task.
#[derive(Clone)]
pub(crate) struct AgentHandle {
    sender: mpsc::Sender<AgentMsg>,
    lifecycle_sender: mpsc::UnboundedSender<AgentLifecycleMsg>,
}

impl AgentHandle {
    /// Creates the handle and the receivers the runtime loop consumes.
    pub(crate) fn channel(
        capacity: usize,
    ) -> (
        Self,
        mpsc::Receiver<AgentMsg>,
        mpsc::UnboundedReceiver<AgentLifecycleMsg>,
    ) {
        let (sender, receiver) = mpsc::channel(capacity);
        let (lifecycle_sender, lifecycle_receiver) = mpsc::unbounded_channel();
        (
            Self {
                sender,
                lifecycle_sender,
            },
            receiver,
            lifecycle_receiver,
        )
    }

    /// Awaits mailbox capacity so external producers backpressure instead of dropping frames.
    pub(crate) async fn send(
        &self,
        message: AgentMsg,
    ) -> Result<(), mpsc::error::SendError<AgentMsg>> {
        self.sender.send(message).await
    }

    /// Attempts to queue one control event without waiting.
    ///
    /// The agent runtime uses this only for self-scheduled messages so it does
    /// not deadlock on its own bounded mailbox.
    pub(crate) fn try_send(
        &self,
        message: AgentMsg,
    ) -> Result<(), mpsc::error::TrySendError<AgentMsg>> {
        self.sender.try_send(message)
    }

    /// Reports transport lifecycle without competing with bounded application traffic.
    pub(crate) fn report_connection_lost(&self, connection_generation: u64, reason: String) {
        let _ = self
            .lifecycle_sender
            .send(AgentLifecycleMsg::ConnectionLost {
                connection_generation,
                reason,
            });
    }
}

/// Startup identity that stays stable across reconnects.
pub(crate) struct AgentProfile {
    pub(crate) agent_id: AgentId,
    pub(crate) agent_name: String,
    /// Immutable absolute directory used when the UI opens this agent.
    pub(crate) default_directory: String,
    /// Shared secret presented during registration so the server can reject impostors.
    pub(crate) token: String,
    /// Keepalive and stale-detection timings chosen at process start.
    pub(crate) ws_timeouts: redoor_core::websocket::WebSocketTimeouts,
}

/// One-shot desktop notification shown after the first fully connected session.
struct StartupNotification {
    /// Desktop detected at process startup, if this agent can plausibly reach a GUI session.
    desktop_environment: Option<redoor_core::desktop::DesktopEnvironment>,
    /// User-selected wait after a successful connection, or `None` when disabled.
    delay: Option<tokio::time::Duration>,
    /// Identifies the socket pair whose delayed notification is currently authoritative.
    generation: Option<(u64, u64)>,
    /// Prevents reconnects from repeating the process-start notification.
    sent: bool,
}

/// Why the agent event loop ended, so the binary can choose the process exit status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentExit {
    /// Requested shutdown (stdin EOF of an SSH-launched agent, closed mailboxes).
    Shutdown,
    /// The server rejected this agent; a failure status lets supervisors notice.
    Rejected,
}

/// Long-lived agent runtime that owns connection lifecycle and running work.
pub(crate) struct AgentRuntime {
    profile: AgentProfile,
    /// Shared connection policy so every agent websocket uses the same tunnel and TLS identity.
    connection: AgentConnection,
    control: ControlLink,
    transfer: TransferLink,
    sessions: Sessions,
    dispatcher: Dispatcher,
    notification: StartupNotification,
    /// Counts consecutive reconnect attempts so prolonged outages use a slower retry window.
    reconnect_attempts: u32,
}

impl AgentRuntime {
    /// Creates the runtime before any websocket connection exists.
    pub(crate) fn new(
        profile: AgentProfile,
        connection: AgentConnection,
        services: AgentServices,
        startup_notification_delay: Option<tokio::time::Duration>,
    ) -> Self {
        Self {
            profile,
            connection,
            control: ControlLink::new(),
            transfer: TransferLink::new(),
            sessions: Sessions::new(services.upload_routes.clone()),
            dispatcher: Dispatcher::new(services),
            notification: StartupNotification {
                desktop_environment: redoor_core::desktop::detect_desktop_environment(),
                delay: startup_notification_delay,
                generation: None,
                sent: false,
            },
            reconnect_attempts: 0,
        }
    }

    /// Runs the agent event loop until shutdown or fatal error.
    pub(crate) async fn run(
        mut self,
        mut receiver: mpsc::Receiver<AgentMsg>,
        mut lifecycle_receiver: mpsc::UnboundedReceiver<AgentLifecycleMsg>,
        handle: AgentHandle,
        exit_on_stdin_eof: bool,
    ) -> AgentExit {
        spawn_stdin_task(handle.clone(), exit_on_stdin_eof).await;
        let _ = handle.try_send(AgentMsg::Connect);

        log!(
            Level::Info,
            "Agent task started: agent_id={}, agent_name={}",
            self.profile.agent_id,
            self.profile.agent_name
        );

        let exit = loop {
            tokio::select! {
                biased;
                lifecycle = lifecycle_receiver.recv() => match lifecycle {
                    Some(message) => self.handle_lifecycle_message(handle.clone(), message).await,
                    None => break AgentExit::Shutdown,
                },
                message = receiver.recv() => match message {
                    Some(message) => {
                        if let Some(exit) = self.handle_message(handle.clone(), message).await {
                            break exit;
                        }
                    }
                    None => break AgentExit::Shutdown,
                },
            }
        };

        self.invalidate_connection().await;
        log!(
            Level::Info,
            "Agent stopped: agent_id={}",
            self.profile.agent_id
        );
        exit
    }

    /// Stops both sockets, cancels all connection-owned work and joins command tasks.
    async fn invalidate_connection(&mut self) {
        self.control.invalidate();
        self.transfer.invalidate();
        self.sessions.invalidate();
        self.dispatcher.stop().await;
    }

    /// Handles one agent control message and returns how the event loop ends, if it should.
    async fn handle_message(
        &mut self,
        handle: AgentHandle,
        message: AgentMsg,
    ) -> Option<AgentExit> {
        match message {
            AgentMsg::Connect => {
                if self.control.sender().is_none() {
                    self.connect(handle).await;
                }
            }
            AgentMsg::WebSocketMessage {
                connection_generation,
                text,
            } => {
                if self.control.is_current(connection_generation) && self.control.sender().is_some()
                {
                    self.handle_server_text(handle, text).await;
                }
            }
            AgentMsg::TransferConnected {
                control_generation,
                transfer_generation,
                token,
                sender,
                shutdown,
                installed,
            } => {
                let is_current = self.control.is_current(control_generation)
                    && self.transfer.is_current(transfer_generation, &token);
                if is_current {
                    // Install before acknowledging so transfer auth cannot expose the server first.
                    self.transfer.install(sender, shutdown);
                    let _ = installed.send(true);
                    self.schedule_startup_notification(handle);
                } else {
                    let _ = shutdown.send(true);
                    let _ = installed.send(false);
                }
            }
            AgentMsg::TransferConnectionLost {
                control_generation,
                transfer_generation,
                reason,
            } => self.handle_transfer_loss(handle, control_generation, transfer_generation, reason),
            AgentMsg::ReconnectTransfer {
                control_generation,
                transfer_generation,
                token,
            } => {
                if self.control.is_current(control_generation)
                    && self.transfer.is_current(transfer_generation, &token)
                    && self.control.sender().is_some()
                    && self.transfer.sender().is_none()
                {
                    self.begin_transfer_connection(handle, token);
                }
            }
            AgentMsg::StartupNotificationDue {
                connection_generation,
                transfer_generation,
            } => {
                self.show_startup_notification_if_current(
                    connection_generation,
                    transfer_generation,
                );
            }
            AgentMsg::SendRawText { text } => {
                if let Some(control) = self.control.sender()
                    && control.send(OutboundFrame::raw_text(text)).await.is_err()
                {
                    let error = anyhow::anyhow!("agent websocket channel is closed");
                    log_error!(error, "Failed to send message; connection may be lost");
                }
            }
            AgentMsg::Shutdown => return Some(AgentExit::Shutdown),
            AgentMsg::ExitWithError => {
                let error = anyhow::anyhow!("agent received fatal exit signal");
                log_error!(error, "Exiting agent due to error");
                return Some(AgentExit::Rejected);
            }
        }
        None
    }

    /// Detaches a failed payload socket and schedules a reconnect with the same credentials.
    fn handle_transfer_loss(
        &mut self,
        handle: AgentHandle,
        control_generation: u64,
        transfer_generation: u64,
        reason: String,
    ) {
        if !self.control.is_current(control_generation)
            || self.transfer.generation() != transfer_generation
        {
            log!(
                Level::Debug,
                "Ignoring stale transfer loss: generation={}",
                transfer_generation
            );
            return;
        }
        log!(
            Level::Warning,
            "Transfer connection lost: agent_id={}, reason={}",
            self.profile.agent_id,
            reason
        );
        let next_generation = self.transfer.lose();
        self.sessions.invalidate_transfers();
        if let Some(token) = self.transfer.token() {
            log!(
                Level::Info,
                "Transfer reconnect scheduled: agent_id={}, generation={}",
                self.profile.agent_id,
                next_generation
            );
            schedule_transfer_reconnect(
                handle,
                control_generation,
                next_generation,
                token.to_string(),
            );
        }
    }

    /// Applies transport lifecycle ahead of bounded application traffic.
    async fn handle_lifecycle_message(&mut self, handle: AgentHandle, message: AgentLifecycleMsg) {
        match message {
            AgentLifecycleMsg::ConnectionLost {
                connection_generation,
                reason,
            } => {
                if !self.control.is_current(connection_generation) {
                    log!(
                        Level::Debug,
                        "Ignoring stale connection loss from generation {}: {}",
                        connection_generation,
                        reason
                    );
                    return;
                }
                if self.control.sender().is_none() {
                    log!(
                        Level::Debug,
                        "Ignoring duplicate connection loss: {}",
                        reason
                    );
                    return;
                }
                self.invalidate_connection().await;
                self.schedule_reconnect(handle, &format!("Connection lost: {reason}"));
            }
        }
    }

    /// Parses one server frame; unparsable commands are answered so callers do not hang.
    async fn handle_server_text(&mut self, handle: AgentHandle, text: String) {
        match serde_json::from_str::<ServerToAgent>(&text) {
            Ok(frame) => self.handle_server_frame(handle, frame).await,
            Err(error) => {
                log!(
                    Level::Warning,
                    "Ignoring unparsable server frame: error={}",
                    error
                );
                let (Some(request_id), Some(control)) = (
                    ServerToAgent::unparsed_command_request_id(&text),
                    self.control.sender(),
                ) else {
                    return;
                };
                let _ = Responder::new(control.clone(), self.profile.agent_id.clone(), request_id)
                    .respond(CommandResult::error(
                        CommandErrorKind::InvalidInput,
                        format!("Agent could not parse command: {error}"),
                    ))
                    .await;
            }
        }
    }

    /// Handles one typed server frame; the match is exhaustive so new frames cannot be dropped.
    async fn handle_server_frame(&mut self, handle: AgentHandle, frame: ServerToAgent) {
        match frame {
            ServerToAgent::TransferSocketOpen {
                token,
                stream_credit,
            } => {
                self.control.confirm_stream_credit(stream_credit);
                self.begin_transfer_connection(handle, token);
            }
            ServerToAgent::StreamCredit { request_id, frames } => self
                .dispatcher
                .services()
                .output_credits
                .grant(request_id, frames),
            ServerToAgent::Command {
                request_id,
                command,
                ..
            } => {
                let Some(control) = self.control.sender() else {
                    return;
                };
                let link = DispatchLink {
                    agent_id: &self.profile.agent_id,
                    control,
                    transfer: self.transfer.sender(),
                    scope: self.control.scope(),
                    stream_credit: self.control.stream_credit(),
                };
                self.dispatcher
                    .dispatch(request_id, command, link, &mut self.sessions)
                    .await;
            }
            ServerToAgent::UploadPublicationDecision {
                request_id,
                allowed,
            } => self
                .dispatcher
                .services()
                .upload_routes
                .resolve_publication(request_id, allowed),
            ServerToAgent::CancelTransfer { request_id } => {
                self.sessions.cancel_request(request_id);
            }
            ServerToAgent::TerminalOpen {
                terminal_id,
                token,
                size,
                cwd,
            } => self.start_terminal(terminal_id, token, size, cwd),
            ServerToAgent::LogStreamOpen {
                log_stream_id,
                token,
            } => self.start_log_stream(log_stream_id, token),
            ServerToAgent::Error { message } => {
                log_error!(anyhow::Error::msg(message), "Server reported an error");
                let _ = handle.try_send(AgentMsg::ExitWithError);
            }
        }
    }

    /// Replaces transfer credentials and starts non-blocking setup for the current control link.
    fn begin_transfer_connection(&mut self, handle: AgentHandle, token: String) {
        let Some(control_sender) = self.control.sender().cloned() else {
            return;
        };
        let transfer_generation = self.transfer.begin(token.clone());
        spawn_transfer_connection(TransferConnectContext {
            connection: self.connection.clone(),
            agent_id: self.profile.agent_id.clone(),
            token,
            control_generation: self.control.generation(),
            transfer_generation,
            control_sender,
            upload_routes: self.dispatcher.services().upload_routes.clone(),
            handle,
        });
    }

    /// Starts one dedicated terminal socket whose registration lives as long as its task.
    fn start_terminal(
        &mut self,
        terminal_id: TerminalId,
        token: String,
        size: TerminalSize,
        cwd: String,
    ) {
        if size.validate().is_err() {
            log!(
                Level::Warning,
                "Rejected terminal with invalid dimensions: terminal_id={}",
                terminal_id.0
            );
            return;
        }
        let registration = match self.sessions.terminals.register(terminal_id.clone()) {
            Ok(registration) => registration,
            Err(error) => {
                log!(
                    Level::Warning,
                    "Rejected terminal bootstrap ({}): terminal_id={}",
                    error.reason(),
                    terminal_id.0
                );
                return;
            }
        };
        let connection = self.connection.clone();
        tokio::spawn(async move {
            let cancel = registration.token().clone();
            if let Err(error) = crate::terminal::connect_and_run(
                &connection,
                terminal_id.clone(),
                token,
                size,
                cwd,
                cancel,
            )
            .await
            {
                log!(
                    Level::Warning,
                    "Terminal session ended with an error: terminal_id={}, error={}",
                    terminal_id.0,
                    error
                );
            }
            drop(registration);
        });
    }

    /// Starts one dedicated log-stream socket whose registration lives as long as its task.
    fn start_log_stream(&mut self, log_stream_id: LogStreamId, token: String) {
        let registration = match self.sessions.log_streams.register(log_stream_id.clone()) {
            Ok(registration) => registration,
            Err(error) => {
                log!(
                    Level::Warning,
                    "Rejected log stream bootstrap ({}): log_stream_id={}",
                    error.reason(),
                    log_stream_id.0
                );
                return;
            }
        };
        let connection = self.connection.clone();
        tokio::spawn(async move {
            let cancel = registration.token().clone();
            if let Err(error) =
                crate::logs::connect_and_run(&connection, log_stream_id.clone(), token, cancel)
                    .await
            {
                log!(
                    Level::Warning,
                    "Log stream ended with an error: log_stream_id={}, error={}",
                    log_stream_id.0,
                    error
                );
            }
            drop(registration);
        });
    }

    /// Schedules the next connection attempt with jitter and escalates after repeated failures.
    fn schedule_reconnect(&mut self, handle: AgentHandle, reason: &str) {
        self.reconnect_attempts = self.reconnect_attempts.saturating_add(1);
        let attempt = self.reconnect_attempts;
        let delay = reconnect_delay(attempt);
        log!(
            Level::Warning,
            "{}, scheduling reconnect attempt {} in {}s",
            reason,
            attempt,
            delay.as_secs()
        );
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            // Awaits bounded mailbox capacity so a due reconnect cannot be silently discarded.
            let _ = handle.send(AgentMsg::Connect).await;
        });
    }

    /// Starts a replaceable timer only after both authenticated websocket lanes are ready.
    fn schedule_startup_notification(&mut self, handle: AgentHandle) {
        if self.notification.desktop_environment.is_none() || self.notification.sent {
            return;
        }
        let Some(delay) = self.notification.delay else {
            return;
        };
        let connection_generation = self.control.generation();
        let transfer_generation = self.transfer.generation();
        self.notification.generation = Some((connection_generation, transfer_generation));
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = handle
                .send(AgentMsg::StartupNotificationDue {
                    connection_generation,
                    transfer_generation,
                })
                .await;
        });
    }

    /// Shows the delayed notification only when its fully connected socket pair is still current.
    fn show_startup_notification_if_current(
        &mut self,
        connection_generation: u64,
        transfer_generation: u64,
    ) {
        let is_current = self.notification.generation
            == Some((connection_generation, transfer_generation))
            && self.control.is_current(connection_generation)
            && self.transfer.generation() == transfer_generation
            && self.control.sender().is_some()
            && self.transfer.sender().is_some();
        if !is_current {
            return;
        }

        self.notification.generation = None;
        self.notification.sent = true;
        let Some(desktop_environment) = self.notification.desktop_environment else {
            return;
        };
        let agent_name = self.profile.agent_name.clone();
        tokio::spawn(async move {
            if crate::notification::show_agent_started(desktop_environment, &agent_name).await {
                log!(Level::Debug, "Displayed agent startup desktop notification");
            } else {
                log!(
                    Level::Debug,
                    "No working desktop notification tool was found"
                );
            }
        });
    }

    /// Opens a websocket connection and wires the connection tasks into the runtime channels.
    async fn connect(&mut self, handle: AgentHandle) {
        log!(
            Level::Info,
            "Attempting to connect to {} as agent '{}'",
            self.connection.server_url(),
            self.profile.agent_name
        );

        let stale_timeout = self.profile.ws_timeouts.stale_timeout;
        let connection_result = tokio::time::timeout(
            stale_timeout,
            self.connection.connect(self.connection.server_url()),
        )
        .await;
        let (ws_stream, _response) = match connection_result {
            Ok(Ok(connected)) => connected,
            Ok(Err(error)) => {
                self.schedule_reconnect(handle, &format!("Connection failed: {error:#}"));
                return;
            }
            Err(_) => {
                self.schedule_reconnect(
                    handle,
                    &format!(
                        "Connection failed: WebSocket connection attempt timed out after {stale_timeout:?}"
                    ),
                );
                return;
            }
        };
        self.reconnect_attempts = 0;
        log!(Level::Info, "Connected to {}", self.connection.server_url());
        log!(
            Level::Info,
            "Agent connected: agent_id={}, agent_name={}, server={}",
            self.profile.agent_id,
            self.profile.agent_name,
            self.connection.server_url()
        );

        let (write, read) = ws_stream.split();
        let (control_tx, control_rx) = mpsc::channel::<OutboundFrame>(CONTROL_QUEUE_CAPACITY);
        let (control_shutdown, control_shutdown_rx) = watch::channel(false);
        let connection_generation = self
            .control
            .attach(control_tx.clone(), control_shutdown.clone());
        self.sessions.rebind(self.control.scope());

        if let Err(error) = redoor_core::systemd_notify::ready().await {
            log!(
                Level::Warning,
                "Failed to notify systemd readiness: {error}"
            );
        }

        spawn_read_task(
            read,
            handle.clone(),
            connection_generation,
            control_tx.clone(),
            control_shutdown.clone(),
            control_shutdown_rx.clone(),
            self.profile.ws_timeouts,
        )
        .await;
        spawn_write_task(
            write,
            control_rx,
            control_shutdown,
            control_shutdown_rx,
            handle.clone(),
            connection_generation,
        );

        let host = HostIdentity::current();
        let trash = &self.dispatcher.services().trash;
        let register = AgentToServer::AgentRegister {
            agent_id: self.profile.agent_id.clone(),
            agent_name: self.profile.agent_name.clone(),
            token: self.profile.token.clone(),
            identity: AgentIdentity {
                os: host.os,
                arch: host.arch,
                hostname: host.hostname,
                username: host.username,
                default_directory: self.profile.default_directory.clone(),
                // Lets the server compare agent builds against itself without a round-trip command.
                binary: redoor_core::identity::current_binary_identity(),
                uid: Some(host.uid),
                is_root: host.is_root,
            },
            capabilities: AgentCapabilities {
                // Upgrade safety depends on the server knowing this command is implemented.
                supports_self_exec: true,
                // Reuse startup GUI detection so headless agents never advertise the action.
                supports_native_open: self.notification.desktop_environment.is_some(),
                // Prevents newer servers from dispatching unknown commands to unsupported agents.
                supports_trash: trash.supports_inventory,
                // macOS supports the native move without Redoor inventory or restore support.
                supports_move_to_trash: trash.supports_move,
                // Lets the server isolate streams sharing the transfer socket with per-stream credit.
                supports_stream_credit: true,
            },
        };
        // Do not log the message body: it includes the agent token secret.
        log!(
            Level::Info,
            "Sending agent registration message: agent_id={}, agent_name={}",
            self.profile.agent_id,
            self.profile.agent_name
        );
        if let Err(error) = control_tx.send(OutboundFrame::message(register)).await {
            log_error!(
                anyhow::anyhow!("control lane closed: {error}"),
                "Failed to send agent registration"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trash::Trash;

    /// Builds a runtime without network side effects for state-transition tests.
    fn runtime() -> AgentRuntime {
        AgentRuntime::new(
            AgentProfile {
                agent_id: AgentId::from("agent"),
                agent_name: "agent".to_string(),
                default_directory: "/tmp".to_string(),
                token: "test-token".to_string(),
                ws_timeouts: redoor_core::websocket::WebSocketTimeouts::production(),
            },
            AgentConnection::new("ws://localhost".to_string(), None, false).unwrap(),
            AgentServices::new(Trash::unsupported(), None),
            None,
        )
    }

    /// Attaches fake control and transfer writers as if both sockets were connected.
    fn attach_links(
        runtime: &mut AgentRuntime,
    ) -> (mpsc::Receiver<OutboundFrame>, mpsc::Receiver<WsMessage>) {
        let (control_tx, control_rx) = mpsc::channel(8);
        let (shutdown, _) = watch::channel(false);
        runtime.control.attach(control_tx, shutdown);
        runtime.sessions.rebind(runtime.control.scope());
        let (transfer_tx, transfer_rx) = mpsc::channel(1);
        let (transfer_shutdown, _) = watch::channel(false);
        runtime.transfer.begin("session-token".to_string());
        runtime.transfer.install(transfer_tx, transfer_shutdown);
        (control_rx, transfer_rx)
    }

    /// Verifies the first ten reconnect attempts retain the quick jitter window.
    #[test]
    fn first_ten_reconnect_attempts_use_quick_window() {
        // The first retry must not be delayed by the prolonged-outage policy.
        assert_eq!(reconnect_delay_range(1), 2..=5);
        // The tenth retry is the final attempt in the quick window.
        assert_eq!(reconnect_delay_range(10), 2..=5);
    }

    /// Verifies reconnect attempts after the first ten use the slower jitter window.
    #[test]
    fn later_reconnect_attempts_use_slow_window() {
        // The eleventh retry must back off to avoid aggressive reconnect traffic.
        assert_eq!(reconnect_delay_range(11), 30..=60);
        // Saturated counters must remain in the prolonged-outage retry window.
        assert_eq!(reconnect_delay_range(u32::MAX), 30..=60);
    }

    /// Verifies generated delays always remain inside their assigned inclusive windows.
    #[test]
    fn reconnect_delay_is_jittered_within_policy_windows() {
        for attempt in [1, 10, 11, u32::MAX] {
            let expected_range = reconnect_delay_range(attempt);
            for _ in 0..100 {
                let delay = reconnect_delay(attempt).as_secs();
                // Jitter must never reconnect earlier or later than the policy permits.
                assert!(expected_range.contains(&delay));
            }
        }
    }

    /// Verifies a delayed writer failure cannot clear a newer live connection.
    #[tokio::test]
    async fn stale_connection_loss_does_not_clear_replacement_connection() {
        let mut runtime = runtime();
        let (_control_rx, _transfer_rx) = attach_links(&mut runtime);
        let stale_generation = runtime.control.generation();
        let (_control_rx, _transfer_rx) = attach_links(&mut runtime);
        let current_generation = runtime.control.generation();
        let (handle, _receiver, _lifecycle_receiver) = AgentHandle::channel(1);

        runtime
            .handle_lifecycle_message(
                handle,
                AgentLifecycleMsg::ConnectionLost {
                    connection_generation: stale_generation,
                    reason: "old writer failed".to_string(),
                },
            )
            .await;

        // A delayed lifecycle event must leave the replacement generation authoritative.
        assert_eq!(runtime.control.generation(), current_generation);
        // The current control sender must survive a delayed loss from the old generation.
        assert!(runtime.control.sender().is_some());
        // The independently attached transfer sender must also remain available.
        assert!(runtime.transfer.sender().is_some());
    }

    /// Verifies delayed transfer teardown cannot detach a newer payload socket.
    #[tokio::test]
    async fn stale_transfer_loss_does_not_clear_replacement_sender() {
        let mut runtime = runtime();
        let (_control_rx, _transfer_rx) = attach_links(&mut runtime);
        let stale_generation = runtime.transfer.generation();
        let current_generation = runtime.transfer.begin("session-token".to_string());
        let (transfer_tx, _replacement_rx) = mpsc::channel(1);
        let (transfer_shutdown, _) = watch::channel(false);
        runtime.transfer.install(transfer_tx, transfer_shutdown);
        let (handle, _receiver, _lifecycle_receiver) = AgentHandle::channel(1);

        let exit = runtime
            .handle_message(
                handle,
                AgentMsg::TransferConnectionLost {
                    control_generation: runtime.control.generation(),
                    transfer_generation: stale_generation,
                    reason: "old transfer reader ended".to_string(),
                },
            )
            .await;

        // A stale transfer event must not stop the authoritative control actor.
        assert_eq!(exit, None);
        // The current transfer generation must remain unchanged by delayed teardown.
        assert_eq!(runtime.transfer.generation(), current_generation);
        // The replacement payload sender must remain installed and usable.
        assert!(runtime.transfer.sender().is_some());
    }

    /// Verifies a reconnect timer waits for bounded capacity instead of dropping the only retry.
    #[tokio::test]
    async fn reconnect_survives_saturated_actor_mailbox() {
        let (handle, mut receiver, _lifecycle_receiver) = AgentHandle::channel(1);
        handle
            .try_send(AgentMsg::ExitWithError)
            .expect("mailbox saturation setup should fill its only slot");
        let reconnect = handle.send(AgentMsg::Connect);
        tokio::pin!(reconnect);

        // Pending while full proves a due reconnect waits instead of using a lossy try-send.
        assert!(futures_util::poll!(&mut reconnect).is_pending());

        // The existing item must remain first because reconnect scheduling may not evict traffic.
        assert!(matches!(
            receiver.recv().await,
            Some(AgentMsg::ExitWithError)
        ));
        reconnect.await.expect("mailbox stays open");
        // Freeing capacity must release the awaited timer send and preserve the reconnect attempt.
        assert!(matches!(receiver.recv().await, Some(AgentMsg::Connect)));
    }

    /// Verifies awaited sends stay pending while the bounded mailbox is full so websocket
    /// ingress can propagate upload backpressure instead of dropping frames.
    #[tokio::test]
    async fn send_waits_for_channel_capacity() {
        let (handle, mut receiver, _lifecycle_receiver) = AgentHandle::channel(1);
        handle
            .try_send(AgentMsg::Connect)
            .expect("initial enqueue should succeed");

        let pending_send = handle.send(AgentMsg::ExitWithError);
        tokio::pin!(pending_send);

        // A pending send here proves the bounded mailbox is applying backpressure.
        assert!(futures_util::poll!(&mut pending_send).is_pending());
        // Draining one slot lets the send complete.
        assert!(matches!(receiver.recv().await, Some(AgentMsg::Connect)));
        assert!(pending_send.await.is_ok());
        // The queued message survived backpressure.
        assert!(matches!(
            receiver.recv().await,
            Some(AgentMsg::ExitWithError)
        ));
    }

    /// Verifies non-blocking self-sends fail fast when the mailbox is full.
    #[tokio::test]
    async fn try_send_fails_fast_when_channel_is_full() {
        let (handle, _receiver, _lifecycle_receiver) = AgentHandle::channel(1);
        handle
            .try_send(AgentMsg::Connect)
            .expect("initial enqueue should succeed");

        let result = handle.try_send(AgentMsg::ExitWithError);

        // A full error protects in-actor self-sends from deadlocking on the bounded mailbox.
        assert!(matches!(
            result,
            Err(mpsc::error::TrySendError::Full(AgentMsg::ExitWithError))
        ));
    }

    /// Verifies an unknown command variant is answered instead of silently dropped.
    #[tokio::test]
    async fn unparsable_command_frames_are_answered() {
        let mut runtime = runtime();
        let (mut control_rx, _transfer_rx) = attach_links(&mut runtime);
        let (handle, _receiver, _lifecycle_receiver) = AgentHandle::channel(1);

        runtime
            .handle_server_text(
                handle,
                r#"{"type":"command","agent_id":"agent","request_id":11,"command":{"type":"FromTheFuture"}}"#
                    .to_string(),
            )
            .await;

        let frame = control_rx.recv().await.unwrap();
        // The waiting REST caller receives a typed invalid-input error for its request id.
        assert!(matches!(
            frame.as_message(),
            Some(AgentToServer::CommandResponse {
                request_id,
                result: CommandResult::Error { kind: CommandErrorKind::InvalidInput, .. },
                ..
            }) if request_id.0 == 11
        ));
    }

    /// Verifies control loss cancels and joins command workers before reconnecting.
    #[tokio::test]
    async fn connection_loss_cancels_and_joins_commands() {
        let mut runtime = runtime();
        let (_control_rx, transfer_rx) = attach_links(&mut runtime);
        let (handle, _receiver, _lifecycle_receiver) = AgentHandle::channel(1);
        runtime
            .handle_server_frame(
                handle.clone(),
                ServerToAgent::Command {
                    agent_id: AgentId::from("agent"),
                    request_id: redoor_protocol::ids::RequestId::new(1),
                    command: redoor_protocol::wire::command::Command::RawDownload {
                        path: "/dev/zero".to_string(),
                        range_start: None,
                        range_end: None,
                    },
                },
            )
            .await;
        // The transfer writer task exits with its socket, closing the payload queue.
        drop(transfer_rx);

        runtime
            .handle_lifecycle_message(
                handle,
                AgentLifecycleMsg::ConnectionLost {
                    connection_generation: runtime.control.generation(),
                    reason: "test".to_string(),
                },
            )
            .await;

        // No worker may outlive the connection whose resources it used.
        assert!(runtime.dispatcher.is_idle());
        // The control link is detached so a reconnect attaches a fresh generation.
        assert!(runtime.control.sender().is_none());
    }
}
