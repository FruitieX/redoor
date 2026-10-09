//! Websocket entry points for agent control connections and browser UI events.

use crate::actors;
use crate::watchdog::WatchdogRegistry;
use axum::{
    extract::{
        State as AxumState,
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use redoor_core::websocket::WebSocketTimeouts;
use redoor_protocol::api::ui_events::UiEvent;
use redoor_protocol::ids::SocketId;
use uuid::Uuid;

use super::state::ServerState;

/// Route: `GET /ws`
pub(crate) async fn websocket_handler(
    ws: WebSocketUpgrade,
    AxumState(state): AxumState<ServerState>,
) -> impl IntoResponse {
    let router_ref = state.router_ref;
    let watchdog_registry = state.watchdog_registry;
    let agent_token = state.auth.agent_token().to_string();
    let timeouts = state.runtime.ws_timeouts;
    ws.on_upgrade(move |socket| {
        handle_socket(socket, router_ref, watchdog_registry, agent_token, timeouts)
    })
}

/// Route: `GET /api/v1/ui/ws`
pub(crate) async fn ui_websocket_handler(
    ws: WebSocketUpgrade,
    AxumState(state): AxumState<ServerState>,
) -> impl IntoResponse {
    let timeouts = state.runtime.ws_timeouts;
    ws.on_upgrade(move |socket| handle_ui_socket(socket, state.router_ref, timeouts))
}

/// Gives the new agent connection its own socket id and hands it to the session actor.
async fn handle_socket(
    socket: WebSocket,
    router_ref: actors::router::RouterHandle,
    watchdog_registry: WatchdogRegistry,
    agent_token: String,
    timeouts: WebSocketTimeouts,
) {
    let socket_id = SocketId::new();
    actors::session::handle_websocket(
        socket,
        socket_id,
        router_ref,
        watchdog_registry,
        agent_token,
        timeouts,
    )
    .await;
}

/// Relays UI events from the router to the websocket until either side closes.
async fn forward_ui_events(
    sender: &mut futures_util::stream::SplitSink<WebSocket, WsMessage>,
    rx_out: &mut tokio::sync::mpsc::UnboundedReceiver<UiEvent>,
    timeouts: WebSocketTimeouts,
) {
    let mut keepalive = timeouts.keepalive_interval();
    loop {
        let event = tokio::select! {
            _ = keepalive.tick() => {
                if sender.send(WsMessage::Ping(bytes::Bytes::new())).await.is_err() {
                    return;
                }
                continue;
            }
            event = rx_out.recv() => event,
        };
        let Some(event) = event else {
            return;
        };
        let json = match serde_json::to_string(&event) {
            Ok(json) => json,
            Err(error) => {
                redoor_core::log_failure!(
                    Level::Error,
                    "Failed to serialize UI websocket event: {}",
                    error
                );
                continue;
            }
        };

        if sender.send(WsMessage::Text(json.into())).await.is_err() {
            return;
        }
    }
}

/// Waits for the UI websocket receive side to fail or close.
async fn wait_for_ui_disconnect(receiver: &mut futures_util::stream::SplitStream<WebSocket>) {
    while let Some(message_result) = receiver.next().await {
        if message_result.is_err() {
            break;
        }
    }
}

/// Streams UI events to one browser until it disconnects or the router stops.
async fn handle_ui_socket(
    socket: WebSocket,
    router_ref: actors::router::RouterHandle,
    timeouts: WebSocketTimeouts,
) {
    let subscriber_id = Uuid::new_v4().to_string();
    let (mut sender, mut receiver) = socket.split();
    let (tx_out, mut rx_out) = tokio::sync::mpsc::unbounded_channel::<UiEvent>();

    if let Err(error) = router_ref
        .send_async(actors::router::RouterMsg::RegisterUiSubscriber(
            actors::router::RegisterUiSubscriberRequest {
                subscriber_id: subscriber_id.clone(),
                sender: tx_out,
            },
        ))
        .await
    {
        redoor_core::log_failure!(
            Level::Error,
            "Failed to queue UI subscriber registration: subscriber_id={}, error={}",
            subscriber_id,
            error
        );
        return;
    }

    tokio::select! {
        _ = forward_ui_events(&mut sender, &mut rx_out, timeouts) => {}
        _ = wait_for_ui_disconnect(&mut receiver) => {}
    }

    if let Err(error) = router_ref
        .send_async(actors::router::RouterMsg::UnregisterUiSubscriber {
            subscriber_id: subscriber_id.clone(),
        })
        .await
    {
        redoor_core::log_failure!(
            Level::Error,
            "Failed to queue UI subscriber cleanup: subscriber_id={}, error={}",
            subscriber_id,
            error
        );
    }
}
