//! Dedicated non-PTY execution endpoint reuses stream routing and drop-triggered cancellation.

use super::{error::ApiError, services::output::OutputStream, state::ServerState};
use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::StatusCode,
    response::Response,
};
use redoor_protocol::{
    api::exec::CancelExecutionResponse,
    ids::{AgentId, RequestId},
    wire::exec::{ExecEvent, ExecRequest},
};

/// Cancels process output through its own resource while file-transfer history stays file-only.
pub(crate) async fn cancel_execution_handler(
    Path((agent, execution_id)): Path<(String, u64)>,
    State(state): State<ServerState>,
) -> Result<(StatusCode, Json<CancelExecutionResponse>), ApiError> {
    let request_id = RequestId::new(execution_id);
    if !state
        .agents
        .cancel_execution(AgentId::from(agent), request_id)
        .await?
    {
        return Err(ApiError::not_found("Execution not found"));
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(CancelExecutionResponse {
            execution_id: request_id,
        }),
    ))
}

/// Returns NDJSON immediately after admission; dropping the body cancels the owned process.
pub(crate) async fn exec_handler(
    Path(agent): Path<String>,
    State(state): State<ServerState>,
    Json(request): Json<ExecRequest>,
) -> Result<Response, ApiError> {
    request.validate().map_err(ApiError::bad_request)?;
    let (output, terminal) =
        OutputStream::open_execution(&state.router_ref, AgentId::from(agent), request).await?;
    let id = output.request_id();
    Ok(Response::builder()
        .header("Content-Type", "application/x-ndjson")
        .header("X-Redoor-Execution-Id", id.to_string())
        .body(execution_body(output, terminal))
        .unwrap())
}

/// Serializes one terminal event as an NDJSON line.
fn ndjson_line(event: &ExecEvent) -> bytes::Bytes {
    let mut data = serde_json::to_vec(event).expect("execution events always serialize");
    data.push(b'\n');
    bytes::Bytes::from(data)
}

/// Drains already accepted payload before fallback completion so timeouts cannot erase trailing output.
fn execution_body(
    mut output: OutputStream,
    mut terminal_receiver: tokio::sync::watch::Receiver<Option<ExecEvent>>,
) -> Body {
    let stream = async_stream::stream! {
        let mut terminal_open = true;
        loop {
            let chunk = tokio::select! {
                biased;
                chunk = output.recv() => Ok(chunk),
                result = terminal_receiver.changed(), if terminal_open => Err(result.is_ok()),
            };
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(false) => { terminal_open = false; continue; }
                Err(true) => {
                    let event = terminal_receiver.borrow_and_update().clone();
                    let Some(event) = event else { break; };
                    // Only agent terminal acknowledgements can describe process termination.
                    output.disarm();
                    yield Ok(ndjson_line(&event));
                    break;
                }
            };
            let Some(chunk) = chunk else {
                // Accepted cancellation drops the payload sink to release backpressure,
                // but termination is still pending on the independent acknowledgement lane.
                if terminal_open && terminal_receiver.borrow().is_none() {
                    let _ = terminal_receiver.changed().await;
                }
                let event = terminal_receiver.borrow_and_update().clone();
                if let Some(event) = event {
                    output.disarm();
                    yield Ok(ndjson_line(&event));
                } else {
                    yield Err(std::io::Error::other("Execution stream ended before completion"));
                }
                break;
            };
            if chunk.is_error {
                yield Err(std::io::Error::other(String::from_utf8_lossy(&chunk.data).to_string()));
                break;
            }
            yield Ok(bytes::Bytes::from(chunk.data));
            if chunk.is_last { break; }
        }
    };
    Body::from_stream(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actors::router::{ConnectionStreams, spawn_router};
    use redoor_protocol::wire::{exec::ExecEvent, stream::StreamChunk};

    /// A slow HTTP consumer can have buffered output when the independent fallback arrives.
    #[tokio::test]
    async fn fallback_drains_buffered_output_before_single_terminal_event() {
        let streams = ConnectionStreams::new(AgentId::from("test"));
        let receiver = streams.open_detached(RequestId::new(1));
        let output = ExecEvent::Stdout {
            data: b"trailing output".to_vec(),
        };
        let mut data = serde_json::to_vec(&output).unwrap();
        data.push(b'\n');
        streams.deliver(StreamChunk {
            request_id: RequestId::new(1),
            is_last: false,
            is_error: false,
            data,
        });
        let (terminal, terminal_receiver) = tokio::sync::watch::channel(None);
        terminal.send_replace(Some(ExecEvent::TimedOut));
        // The router's fallback closes the stream without a terminal frame.
        streams.close_inbound(RequestId::new(1));
        let (router, task) = spawn_router();
        let output = OutputStream::detached(router, AgentId::from("test"), receiver);
        let bytes = axum::body::to_bytes(execution_body(output, terminal_receiver), 4096)
            .await
            .unwrap();
        let events: Vec<ExecEvent> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        // Independent completion must not skip accepted stdout or emit a duplicate terminal record.
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], ExecEvent::Stdout { data } if data == b"trailing output"));
        assert!(matches!(&events[1], ExecEvent::TimedOut));
        task.abort();
    }
}
