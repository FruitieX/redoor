//! Per-request writer for framed agent-to-server byte streams.

use redoor_protocol::{
    ids::{ChunkIndex, RequestId},
    wire::{
        stream,
        stream::{StreamChunkFrameRequest, StreamPayloadKind},
    },
};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;

/// Message sent as the terminal error frame when the server cancels an output stream.
pub(crate) const CANCELED_MESSAGE: &[u8] = b"Download canceled by server";

/// Frames one request's output onto the shared transfer socket in chunk order.
///
/// Every outbound payload frame of downloads and exec output leaves through
/// [`StreamWriter::send_frames`], which is the single place where per-stream flow control (a
/// credit gate that waits for server-granted frames) can be added without touching producers.
pub(crate) struct StreamWriter {
    transfer: mpsc::Sender<WsMessage>,
    request_id: RequestId,
    payload_kind: StreamPayloadKind,
    next_index: ChunkIndex,
}

impl StreamWriter {
    /// Starts a stream at chunk index zero on the current transfer socket.
    pub(crate) fn new(
        transfer: mpsc::Sender<WsMessage>,
        request_id: RequestId,
        payload_kind: StreamPayloadKind,
    ) -> Self {
        Self {
            transfer,
            request_id,
            payload_kind,
            next_index: ChunkIndex::new(0),
        }
    }

    /// Sends payload bytes; `is_last` marks the successful end of the stream.
    pub(crate) async fn send(&mut self, data: &[u8], is_last: bool) -> bool {
        self.send_frames(
            StreamChunkFrameRequest::new(self.request_id, data)
                .payload_kind(self.payload_kind)
                .is_last(is_last),
        )
        .await
    }

    /// Sends the terminal error frame the server maps to a failed transfer.
    pub(crate) async fn send_error(&mut self, message: &[u8]) -> bool {
        self.send_frames(
            StreamChunkFrameRequest::new(self.request_id, message)
                .payload_kind(self.payload_kind)
                .is_error(true),
        )
        .await
    }

    /// Number of frames written so far, for completion logs.
    pub(crate) fn frames_sent(&self) -> u64 {
        self.next_index.0
    }

    /// Reframes one logical chunk into websocket-sized frames while preserving order.
    ///
    /// Each send waits for the depth-one transfer queue, propagating socket backpressure to the
    /// producer. A future credit gate belongs immediately before each `send`.
    async fn send_frames(&mut self, request: StreamChunkFrameRequest<'_>) -> bool {
        let mut frames =
            stream::StreamChunkFrames::new(request.starting_chunk_index(self.next_index));
        while let Some(chunk) = frames.next() {
            let next_index = frames.next_chunk_index();
            if self
                .transfer
                .send(WsMessage::Binary(chunk.to_bytes().into()))
                .await
                .is_err()
            {
                self.next_index = next_index;
                return false;
            }
            tokio::task::yield_now().await;
        }
        self.next_index = frames.next_chunk_index();
        true
    }
}
