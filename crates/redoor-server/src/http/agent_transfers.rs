//! Agent endpoint for the persistent payload-only transfer socket that carries binary
//! stream frames apart from control traffic.
//!
//! The socket authenticates during the upgrade with the per-connection token from
//! `transfer_socket_open`, so an unauthenticated socket never exists.

use crate::actors::router::{
    ConnectionStreams, RegisterTransferConnectionRequest, RouterHandle, RouterMsg,
};
use axum::{
    extract::{
        State as AxumState,
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use redoor_core::log;
use redoor_protocol::{
    ids::SocketId,
    wire::{
        handshake::PROTOCOL,
        log::Level,
        stream::StreamChunk,
        transfer::{MAX_TRANSFER_WEBSOCKET_MESSAGE_BYTES, TRANSFER_OUTBOUND_QUEUE_CAPACITY},
    },
};
use tokio::sync::{mpsc, watch};

use super::agent_ws::{bearer_token, offers_protocol};
use super::state::ServerState;

/// Upgrades the exact public agent payload route with transfer-sized frame limits.
pub(crate) async fn agent_transfer_websocket_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    AxumState(state): AxumState<ServerState>,
) -> Response {
    let Some(token) = bearer_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if !offers_protocol(&headers) {
        return StatusCode::UPGRADE_REQUIRED.into_response();
    }
    let authorized = state
        .router_ref
        .request(|reply| RouterMsg::AuthorizeTransferToken {
            token: token.clone(),
            reply,
        })
        .await;
    if !matches!(authorized, Ok(true)) {
        log!(Level::Warning, "Transfer socket upgrade rejected");
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.protocols([PROTOCOL])
        .max_message_size(MAX_TRANSFER_WEBSOCKET_MESSAGE_BYTES)
        .max_frame_size(MAX_TRANSFER_WEBSOCKET_MESSAGE_BYTES)
        .on_upgrade(move |socket| {
            run_transfer_socket(socket, token, state.router_ref, state.runtime.ws_timeouts)
        })
}

/// Registers and runs one persistent payload socket.
///
/// The token was checked before the upgrade, but the connection it belonged to may have been
/// replaced since, so registration validates it once more and closes the socket otherwise.
async fn run_transfer_socket(
    mut socket: WebSocket,
    token: String,
    router: RouterHandle,
    timeouts: redoor_core::websocket::WebSocketTimeouts,
) {
    let socket_id = SocketId::new();
    let (outgoing_binary, mut binary_receiver) =
        mpsc::channel::<StreamChunk>(TRANSFER_OUTBOUND_QUEUE_CAPACITY);
    let (shutdown, shutdown_receiver) = watch::channel(false);
    let registration = router
        .request(|reply| {
            RouterMsg::RegisterTransferConnection(RegisterTransferConnectionRequest {
                token,
                socket_id: socket_id.clone(),
                outgoing_binary,
                shutdown,
                reply,
            })
        })
        .await;
    let Ok(Ok((agent_id, streams))) = registration else {
        log!(
            Level::Warning,
            "Transfer socket registration rejected: socket_id={}",
            socket_id
        );
        let _ = socket.send(WsMessage::Close(None)).await;
        return;
    };

    log!(
        Level::Info,
        "Transfer socket accepted: agent_id={}, socket_id={}",
        agent_id,
        socket_id
    );
    let (mut writer, mut reader) = socket.split();
    let mut reader_shutdown = shutdown_receiver.clone();
    let mut writer_shutdown = shutdown_receiver;
    tokio::select! {
        _ = read_transfer_frames(&mut reader, &router, &streams, &socket_id, &mut reader_shutdown) => {}
        _ = write_transfer_frames(&mut writer, &mut binary_receiver, &mut writer_shutdown, timeouts) => {}
    }

    let _ = router
        .send_async(RouterMsg::UnregisterTransferConnection {
            agent_id,
            socket_id,
        })
        .await;
}

/// Hands binary frames straight to their streams' consumers.
///
/// Payload never passes through the router: only a stream's terminal frame is reported to
/// it. Every stream is credited, so the reader never waits on a consumer and a stalled HTTP
/// client or copy destination cannot freeze the other streams sharing this socket.
async fn read_transfer_frames(
    reader: &mut futures_util::stream::SplitStream<WebSocket>,
    router: &RouterHandle,
    streams: &ConnectionStreams,
    socket_id: &SocketId,
    shutdown: &mut watch::Receiver<bool>,
) {
    loop {
        let frame = tokio::select! {
            _ = shutdown.changed() => break,
            frame = reader.next() => frame,
        };
        let Some(Ok(frame)) = frame else {
            break;
        };
        match frame {
            WsMessage::Binary(bytes) => {
                let Ok(chunk) = StreamChunk::from_bytes(&bytes) else {
                    break;
                };
                if let Some(end) = streams.deliver(chunk)
                    && router
                        .send_async(RouterMsg::InboundStreamEnded(end))
                        .await
                        .is_err()
                {
                    break;
                }
            }
            WsMessage::Ping(_) | WsMessage::Pong(_) => {}
            WsMessage::Close(_) => break,
            WsMessage::Text(_) => {
                log!(
                    Level::Warning,
                    "Text frame on transfer socket: socket_id={}",
                    socket_id
                );
                break;
            }
        }
    }
}

/// Drains payload frames and pings independently of the control writer; the only place
/// server-to-agent stream frames are encoded for the socket.
async fn write_transfer_frames(
    writer: &mut futures_util::stream::SplitSink<WebSocket, WsMessage>,
    receiver: &mut mpsc::Receiver<StreamChunk>,
    shutdown: &mut watch::Receiver<bool>,
    timeouts: redoor_core::websocket::WebSocketTimeouts,
) {
    let mut ping = timeouts.keepalive_interval();
    loop {
        let message = tokio::select! {
            _ = shutdown.changed() => break,
            _ = ping.tick() => Some(WsMessage::Ping(bytes::Bytes::new())),
            chunk = receiver.recv() => chunk.map(|chunk| WsMessage::Binary(chunk.to_bytes().into())),
        };
        let Some(message) = message else {
            break;
        };
        if writer.send(message).await.is_err() {
            break;
        }
    }
}
