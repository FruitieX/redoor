//! Browser terminal endpoint: bridges one browser websocket to one terminal stream of the
//! agent's connection.

use super::{
    agent_helpers::require_absolute_path, state::ServerState, websocket_security::is_same_origin,
};
use crate::actors::{
    lanes::ControlLanes,
    router::{
        OpenTerminalRequest, OutboundStream, OutputReceiver, RouterError, RouterMsg,
        StreamCloseGuard, TerminalStream,
    },
};
use crate::http::access::ResolvedDevice;
use axum::{
    extract::{
        Query, State as AxumState,
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use redoor_core::websocket::WebSocketTimeouts;
use redoor_protocol::{
    ids::{AgentId, RequestId},
    wire::{
        ServerToAgent,
        stream::StreamChunk,
        terminal::{TerminalClientMessage, TerminalServerMessage, TerminalSize},
    },
};
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::mpsc;

/// Caps websocket frames on terminal sockets so one message cannot force a large allocation.
const MAX_TERMINAL_MESSAGE_SIZE: usize = 1024 * 1024;
/// How long a browser waits for the agent's PTY to start before setup is abandoned.
const TERMINAL_SETUP_TIMEOUT: Duration = Duration::from_secs(10);
/// Frames of browser input kept until the agent's PTY is ready, so early keystrokes are not
/// lost but cannot accumulate without bound.
const MAX_PRE_READY_FRAMES: usize = 32;
/// Byte cap for the same pre-ready input buffer.
const MAX_PRE_READY_BYTES: usize = 64 * 1024;

/// Parses initial terminal dimensions before the browser upgrade is accepted.
#[derive(Deserialize)]
pub(crate) struct TerminalQuery {
    rows: u16,
    cols: u16,
    cwd: String,
}

/// Upgrades a same-origin browser request and opens one terminal stream for it.
pub(crate) async fn browser_terminal_websocket_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    Query(query): Query<TerminalQuery>,
    headers: HeaderMap,
    AxumState(state): AxumState<ServerState>,
    websocket: WebSocketUpgrade,
) -> Response {
    let cwd = match require_absolute_path(query.cwd) {
        Ok(cwd) => cwd,
        Err(error) => return error.into_response(),
    };
    let size = match TerminalSize::new(query.rows, query.cols) {
        Ok(size) => size,
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };
    if !is_same_origin(&headers) {
        return (
            StatusCode::FORBIDDEN,
            "terminal websocket origin is not allowed",
        )
            .into_response();
    }
    websocket
        .max_message_size(MAX_TERMINAL_MESSAGE_SIZE)
        .max_frame_size(MAX_TERMINAL_MESSAGE_SIZE)
        .on_upgrade(move |socket| run_terminal(socket, agent_id, size, cwd, state))
        .into_response()
}

/// The server's ends of one terminal stream, minus the output kept by the relay.
struct TerminalInput {
    stream: RequestId,
    /// Keystrokes, credited with the terminal window on the interactive lane.
    input: OutboundStream,
    /// Resizes travel as priority control frames, ahead of queued input.
    lanes: ControlLanes,
}

/// How waiting for the agent's PTY ended.
enum Setup {
    /// The PTY runs; carries the browser input received meanwhile.
    Ready(Vec<WsMessage>),
    /// The agent could not start the terminal or went away.
    Failed(String),
    /// The browser left first.
    BrowserGone,
}

/// Opens the terminal stream, waits for the PTY and relays until either side ends.
async fn run_terminal(
    mut browser: WebSocket,
    agent_id: AgentId,
    size: TerminalSize,
    cwd: String,
    state: ServerState,
) {
    let opened = state
        .router_ref
        .request(|reply| {
            RouterMsg::OpenTerminal(OpenTerminalRequest {
                agent_id: agent_id.clone(),
                size,
                cwd,
                reply,
            })
        })
        .await;
    let terminal = match opened {
        Ok(Ok(terminal)) => terminal,
        Ok(Err(RouterError::StreamLimitReached { .. })) => {
            send_terminal_error(&mut browser, "Too many terminals are open on this agent").await;
            return;
        }
        _ => {
            send_terminal_error(&mut browser, "Agent is not connected").await;
            return;
        }
    };
    let _close = StreamCloseGuard::new(state.router_ref.clone(), agent_id, terminal.stream);
    let TerminalStream {
        stream,
        output,
        input,
        mut events,
    } = terminal;
    let input = TerminalInput {
        stream,
        lanes: input.lease().link.lanes().clone(),
        input,
    };
    match wait_for_ready(&mut browser, &mut events).await {
        Setup::Ready(pending) => {
            if send_event(&mut browser, &TerminalServerMessage::Ready).await {
                relay_terminal(
                    browser,
                    input,
                    output,
                    events,
                    pending,
                    state.runtime.ws_timeouts,
                )
                .await;
            }
        }
        Setup::Failed(message) => send_terminal_error(&mut browser, &message).await,
        Setup::BrowserGone => {}
    }
}

/// Waits for the PTY while buffering early browser input and detecting a departing browser.
async fn wait_for_ready(
    browser: &mut WebSocket,
    events: &mut mpsc::Receiver<TerminalServerMessage>,
) -> Setup {
    let setup_timeout = tokio::time::sleep(TERMINAL_SETUP_TIMEOUT);
    tokio::pin!(setup_timeout);
    let mut pending = Vec::new();
    let mut pending_bytes = 0;
    loop {
        tokio::select! {
            event = events.recv() => return setup_outcome(event, pending),
            _ = &mut setup_timeout => {
                return Setup::Failed("Agent did not start the terminal".to_string());
            }
            frame = browser.recv() => {
                if !buffer_setup_frame(browser, frame, &mut pending, &mut pending_bytes).await {
                    return Setup::BrowserGone;
                }
            }
        }
    }
}

/// Interprets the first lifecycle event of a terminal.
fn setup_outcome(event: Option<TerminalServerMessage>, pending: Vec<WsMessage>) -> Setup {
    match event {
        Some(TerminalServerMessage::Ready) => Setup::Ready(pending),
        Some(TerminalServerMessage::Error { message }) => Setup::Failed(message),
        Some(TerminalServerMessage::Exit { .. }) => {
            Setup::Failed("Terminal exited before it was ready".to_string())
        }
        None => Setup::Failed("Agent disconnected".to_string()),
    }
}

/// Preserves bounded early input, answers keepalives, and detects browser departure.
async fn buffer_setup_frame(
    browser: &mut WebSocket,
    frame: Option<Result<WsMessage, axum::Error>>,
    pending: &mut Vec<WsMessage>,
    pending_bytes: &mut usize,
) -> bool {
    let length = match &frame {
        Some(Ok(WsMessage::Binary(bytes))) => bytes.len(),
        Some(Ok(WsMessage::Text(text))) => text.len(),
        Some(Ok(WsMessage::Ping(bytes))) => {
            return browser.send(WsMessage::Pong(bytes.clone())).await.is_ok();
        }
        Some(Ok(WsMessage::Pong(_))) => return true,
        Some(Ok(WsMessage::Close(_))) | Some(Err(_)) | None => return false,
    };
    if pending.len() >= MAX_PRE_READY_FRAMES
        || length > MAX_PRE_READY_BYTES.saturating_sub(*pending_bytes)
    {
        return false;
    }
    *pending_bytes += length;
    pending.extend(frame.and_then(Result::ok));
    true
}

/// Sends a typed error and closes the browser socket without exposing terminal contents.
async fn send_terminal_error(socket: &mut WebSocket, message: &str) {
    let _ = send_event(
        socket,
        &TerminalServerMessage::Error {
            message: message.to_string(),
        },
    )
    .await;
    let _ = socket.close().await;
}

/// Sends one lifecycle event to the browser; returns whether the socket accepted it.
async fn send_event(socket: &mut WebSocket, event: &TerminalServerMessage) -> bool {
    let Ok(json) = serde_json::to_string(event) else {
        return false;
    };
    socket.send(WsMessage::Text(json.into())).await.is_ok()
}

/// Runs both directions; whichever ends first ends the terminal.
async fn relay_terminal(
    browser: WebSocket,
    input: TerminalInput,
    output: OutputReceiver,
    events: mpsc::Receiver<TerminalServerMessage>,
    pending: Vec<WsMessage>,
    timeouts: WebSocketTimeouts,
) {
    let (browser_sink, browser_stream) = browser.split();
    tokio::select! {
        _ = forward_browser_to_agent(browser_stream, &input, pending) => {}
        _ = forward_agent_to_browser(output, events, browser_sink, timeouts) => {}
    }
}

/// Forwards keystrokes and validated resizes, starting with the input buffered during setup.
async fn forward_browser_to_agent(
    mut browser: SplitStream<WebSocket>,
    input: &TerminalInput,
    pending: Vec<WsMessage>,
) -> Result<(), ()> {
    for frame in pending {
        if !forward_browser_frame(frame, input).await? {
            return Ok(());
        }
    }
    while let Some(frame) = browser.next().await {
        if !forward_browser_frame(frame.map_err(|_| ())?, input).await? {
            return Ok(());
        }
    }
    Ok(())
}

/// Forwards one browser frame; returns `false` when the browser closed the terminal.
async fn forward_browser_frame(frame: WsMessage, input: &TerminalInput) -> Result<bool, ()> {
    match frame {
        WsMessage::Binary(bytes) => input
            .input
            .send_data(&bytes, false)
            .await
            .map(|()| true)
            .map_err(|_| ()),
        WsMessage::Text(text) => {
            let message = serde_json::from_str::<TerminalClientMessage>(&text).map_err(|_| ())?;
            let TerminalClientMessage::Resize { size } = message;
            size.validate().map_err(|_| ())?;
            let resize = ServerToAgent::TerminalResize {
                stream: input.stream,
                size,
            };
            Ok(input.lanes.try_priority_or_command(resize))
        }
        WsMessage::Ping(_) | WsMessage::Pong(_) => Ok(true),
        WsMessage::Close(_) => Ok(false),
    }
}

/// One thing the agent side produced for the browser.
enum AgentOutput {
    Event(Option<TerminalServerMessage>),
    Output(Option<Result<StreamChunk, redoor_protocol::wire::request::CommandError>>),
    Keepalive,
}

/// Forwards PTY output and lifecycle events until the terminal ends.
///
/// An exit or error event is forwarded only after the output queued before it, because the
/// agent sends the event after its last output and the reader enqueues in that order.
async fn forward_agent_to_browser(
    mut output: OutputReceiver,
    mut events: mpsc::Receiver<TerminalServerMessage>,
    mut browser: SplitSink<WebSocket, WsMessage>,
    timeouts: WebSocketTimeouts,
) -> Result<(), ()> {
    let mut keepalive = timeouts.keepalive_interval();
    loop {
        let next = tokio::select! {
            biased;
            chunk = output.recv() => AgentOutput::Output(chunk),
            event = events.recv() => AgentOutput::Event(event),
            _ = keepalive.tick() => AgentOutput::Keepalive,
        };
        match next {
            AgentOutput::Output(Some(Err(error))) => {
                return finish(
                    &mut browser,
                    &TerminalServerMessage::Error {
                        message: error.message,
                    },
                )
                .await;
            }
            AgentOutput::Output(Some(Ok(chunk))) if chunk.is_last => {
                send_output(&mut browser, chunk).await?;
                // The exit event precedes FIN on the wire, so it is already queued if it exists.
                let event = events.try_recv().unwrap_or(TerminalServerMessage::Exit {
                    code: None,
                    signal: None,
                });
                return finish(&mut browser, &event).await;
            }
            AgentOutput::Output(Some(Ok(chunk))) => send_output(&mut browser, chunk).await?,
            AgentOutput::Output(None) | AgentOutput::Event(None) => {
                return finish(
                    &mut browser,
                    &TerminalServerMessage::Error {
                        message: "Agent disconnected".to_string(),
                    },
                )
                .await;
            }
            AgentOutput::Event(Some(event)) => {
                while let Some(chunk) = output.try_recv() {
                    send_output(&mut browser, chunk).await?;
                }
                return finish(&mut browser, &event).await;
            }
            AgentOutput::Keepalive => browser
                .send(WsMessage::Ping(bytes::Bytes::new()))
                .await
                .map_err(|_| ())?,
        }
    }
}

/// Forwards one PTY output frame.
async fn send_output(
    browser: &mut SplitSink<WebSocket, WsMessage>,
    chunk: StreamChunk,
) -> Result<(), ()> {
    if chunk.data.is_empty() {
        return Ok(());
    }
    browser
        .send(WsMessage::Binary(chunk.data.into()))
        .await
        .map_err(|_| ())
}

/// Sends the final lifecycle event and closes the browser socket.
async fn finish(
    browser: &mut SplitSink<WebSocket, WsMessage>,
    event: &TerminalServerMessage,
) -> Result<(), ()> {
    if let Ok(json) = serde_json::to_string(event) {
        let _ = browser.send(WsMessage::Text(json.into())).await;
    }
    let _ = browser.send(WsMessage::Close(None)).await;
    Err(())
}
