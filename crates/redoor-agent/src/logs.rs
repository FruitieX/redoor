//! Dedicated log-stream socket that relays this agent's live log entries to the server
//! without competing with control or transfer traffic.

use super::connection::AgentConnection;
use anyhow::{Context, Result};
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use redoor_core::{log, logging, logging::LogSubscription};
use redoor_protocol::wire::log::{Level, LogAgentHandshake, LogEntry, LogEvent, LogStreamId};
use tokio::{net::TcpStream, sync::broadcast};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, tungstenite::protocol::Message as WsMessage,
};
use tokio_util::sync::CancellationToken;

/// Dedicated websocket carrying only this agent's log events.
type LogSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
/// Write half, owned by the event sender so it can be backpressured independently.
type LogSink = SplitSink<LogSocket, WsMessage>;
/// Read half, watched only to detect server-side teardown.
type LogStream = SplitStream<LogSocket>;

/// Builds the dedicated data-plane URL while retaining the configured server authority and scheme.
fn log_stream_url(server_url: &str, log_stream_id: &LogStreamId) -> Result<url::Url> {
    let mut url = url::Url::parse(server_url).context("invalid agent websocket URL")?;
    url.set_path(&format!("/api/v1/log-streams/{}/agent/ws", log_stream_id.0));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

/// Connects one authenticated dedicated socket and owns its logger receiver until teardown.
pub(crate) async fn connect_and_run(
    connection: &AgentConnection,
    log_stream_id: LogStreamId,
    token: String,
    cancel: CancellationToken,
) -> Result<()> {
    let url = log_stream_url(connection.server_url(), &log_stream_id)?;
    if cancel.is_cancelled() {
        return Ok(());
    }
    let connection = tokio::select! {
        _ = cancel.cancelled() => return Ok(()),
        result = connection.connect(url.as_str()) => result,
    };
    let (mut socket, _) = connection.context("failed to connect dedicated log websocket")?;

    let handshake = serde_json::to_string(&LogAgentHandshake::Authenticate { token })
        .context("failed to encode log handshake")?;
    let authenticated = tokio::select! {
        _ = cancel.cancelled() => return Ok(()),
        result = socket.send(WsMessage::text(handshake)) => result,
    };
    authenticated.context("failed to authenticate dedicated log websocket")?;

    let subscription = tokio::select! {
        _ = cancel.cancelled() => return Ok(()),
        result = logging::subscribe() => result,
    }
    .context("logger is unavailable")?;

    log!(
        Level::Info,
        "Agent log stream started: log_stream_id={}",
        log_stream_id.0
    );
    let result = run_log_stream(socket, subscription, cancel).await;
    log!(
        Level::Info,
        "Agent log stream stopped: log_stream_id={}",
        log_stream_id.0
    );
    result
}

/// Detects server/browser teardown so dropping the opposite branch releases backpressure promptly.
async fn wait_for_disconnect(stream: &mut LogStream) {
    while let Some(frame) = stream.next().await {
        match frame {
            Ok(WsMessage::Ping(_) | WsMessage::Pong(_)) => {}
            Ok(WsMessage::Close(_) | WsMessage::Text(_) | WsMessage::Binary(_)) | Err(_) => return,
            Ok(WsMessage::Frame(_)) => return,
        }
    }
}

/// Serializes one event so cancellation can interrupt a backpressured dedicated socket send.
async fn send_event_or_cancel(
    sink: &mut LogSink,
    cancel: &CancellationToken,
    event: &LogEvent,
) -> Result<bool> {
    let json = serde_json::to_string(event).context("failed to encode log event")?;
    let sent = tokio::select! {
        _ = cancel.cancelled() => return Ok(false),
        result = sink.send(WsMessage::text(json)) => result,
    };
    Ok(sent.is_ok())
}

/// Sends the initial snapshot while concurrently observing both cancellation sources.
async fn send_snapshot_or_disconnect(
    sink: &mut LogSink,
    stream: &mut LogStream,
    cancel: &CancellationToken,
    snapshot: &LogEvent,
) -> Result<bool> {
    let json = serde_json::to_string(snapshot).context("failed to encode log snapshot")?;
    tokio::select! {
        _ = cancel.cancelled() => Ok(false),
        _ = wait_for_disconnect(stream) => Ok(false),
        result = sink.send(WsMessage::text(json)) => Ok(result.is_ok()),
    }
}

/// Forwards bounded logger records and terminates after one lag notification.
async fn forward_live_entries(
    sink: &mut LogSink,
    receiver: &mut broadcast::Receiver<LogEntry>,
    cancel: &CancellationToken,
) -> Result<()> {
    loop {
        let entry = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            result = receiver.recv() => result,
        };
        match entry {
            Ok(entry) => {
                if !send_event_or_cancel(sink, cancel, &LogEvent::Entry { entry }).await? {
                    return Ok(());
                }
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                let _ = send_event_or_cancel(sink, cancel, &LogEvent::Lagged { skipped }).await?;
                return Ok(());
            }
        }
    }
}

/// Establishes snapshot/live ordering while making history, sends, and live waits cancellable.
async fn run_log_stream(
    socket: LogSocket,
    mut subscription: LogSubscription,
    cancel: CancellationToken,
) -> Result<()> {
    let (mut sink, mut stream) = socket.split();
    let snapshot =
        LogEvent::bounded_snapshot(subscription.entries, subscription.file_logging_enabled);

    if !send_snapshot_or_disconnect(&mut sink, &mut stream, &cancel, &snapshot).await? {
        return Ok(());
    }

    tokio::select! {
        result = forward_live_entries(&mut sink, &mut subscription.receiver, &cancel) => result,
        _ = wait_for_disconnect(&mut stream) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Protects URL construction from retaining control-socket paths, query data, or fragments.
    #[test]
    fn dedicated_url_replaces_only_the_path() {
        let id = LogStreamId(uuid::Uuid::from_u128(1));
        let url = log_stream_url("wss://example.test/ws?secret=no#old", &id)
            .expect("valid server URL should produce a log URL");
        // The server authority and secure WebSocket scheme must be retained.
        assert_eq!(url.scheme(), "wss");
        // Dedicated log traffic must target the one stream-specific agent endpoint.
        assert_eq!(
            url.path(),
            "/api/v1/log-streams/00000000-0000-0000-0000-000000000001/agent/ws"
        );
        // Bootstrap URLs must not inherit unrelated query data or fragments.
        assert!(url.query().is_none() && url.fragment().is_none());
    }
}
