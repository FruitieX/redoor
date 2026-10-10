//! Per-request writer for framed agent-to-server byte streams.

use redoor_core::stream_credit::CreditGate;
use redoor_protocol::{
    ids::{ChunkIndex, RequestId},
    wire::{
        stream,
        stream::{StreamChunkFrameRequest, StreamPayloadKind},
    },
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;

/// Message sent as the terminal error frame when the server cancels an output stream.
pub(crate) const CANCELED_MESSAGE: &[u8] = b"Download canceled by server";

/// Credit gates of the output streams currently writing, keyed by request id.
///
/// The server returns credit on the control socket while the writer runs in its own task, so
/// grants are delivered through this shared table instead of the runtime mailbox.
#[derive(Clone, Default)]
pub(crate) struct OutputCredits {
    gates: Arc<Mutex<OutputCreditTable>>,
}

/// Gates plus the serial that keeps a finished writer from removing a newer registration.
#[derive(Default)]
struct OutputCreditTable {
    gates: HashMap<RequestId, (u64, CreditGate)>,
    next_serial: u64,
}

impl OutputCredits {
    /// Locks the table, tolerating poison because every operation leaves it consistent.
    fn table(&self) -> std::sync::MutexGuard<'_, OutputCreditTable> {
        self.gates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Registers a fresh full-window gate for one output stream; dropping the result removes it.
    pub(crate) fn register(&self, request_id: RequestId) -> CreditRegistration {
        let gate = CreditGate::default();
        let mut table = self.table();
        table.next_serial = table.next_serial.wrapping_add(1);
        let serial = table.next_serial;
        table.gates.insert(request_id, (serial, gate.clone()));
        CreditRegistration {
            credits: self.clone(),
            request_id,
            serial,
            gate,
        }
    }

    /// Applies a server grant; grants for streams that already ended are ignored.
    pub(crate) fn grant(&self, request_id: RequestId, frames: u32) {
        if let Some((_, gate)) = self.table().gates.get(&request_id) {
            gate.grant(frames);
        }
    }

    /// Reports whether a stream is registered, for lifecycle tests.
    #[cfg(test)]
    pub(crate) fn contains(&self, request_id: RequestId) -> bool {
        self.table().gates.contains_key(&request_id)
    }
}

/// One output stream's credit gate, unregistered (and closed) when the writer is dropped.
pub(crate) struct CreditRegistration {
    credits: OutputCredits,
    request_id: RequestId,
    serial: u64,
    gate: CreditGate,
}

impl Drop for CreditRegistration {
    /// Removes only this registration so a reused request id keeps its own gate.
    fn drop(&mut self) {
        let mut table = self.credits.table();
        if table
            .gates
            .get(&self.request_id)
            .is_some_and(|(serial, _)| *serial == self.serial)
        {
            table.gates.remove(&self.request_id);
        }
        self.gate.close();
    }
}

/// Frames one request's output onto the shared transfer socket in chunk order.
///
/// Every outbound payload frame of downloads and exec output leaves through
/// [`StreamWriter::send_frames`], which is also where the per-stream credit gate sits: a
/// non-terminal frame is sent only after spending one frame of credit, so a slow consumer on
/// the server stalls only this writer.
pub(crate) struct StreamWriter {
    transfer: mpsc::Sender<WsMessage>,
    request_id: RequestId,
    payload_kind: StreamPayloadKind,
    next_index: ChunkIndex,
    /// Credit granted by the server for this stream.
    credit: CreditRegistration,
}

impl StreamWriter {
    /// Starts a stream at chunk index zero on the current transfer socket.
    pub(crate) fn new(
        transfer: mpsc::Sender<WsMessage>,
        request_id: RequestId,
        payload_kind: StreamPayloadKind,
        credit: CreditRegistration,
    ) -> Self {
        Self {
            transfer,
            request_id,
            payload_kind,
            next_index: ChunkIndex::new(0),
            credit,
        }
    }

    /// Builds a raw-file writer with its own full credit window, for tests that do not
    /// exercise grants.
    #[cfg(test)]
    pub(crate) fn for_test(transfer: mpsc::Sender<WsMessage>, request_id: RequestId) -> Self {
        Self::new(
            transfer,
            request_id,
            StreamPayloadKind::RawFile,
            OutputCredits::default().register(request_id),
        )
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
    /// Each send first spends credit (non-terminal frames only) and then
    /// waits for the depth-one transfer queue. Callers race these waits against cancellation,
    /// so neither a stalled consumer nor socket backpressure can hide a cancel request.
    async fn send_frames(&mut self, request: StreamChunkFrameRequest<'_>) -> bool {
        let mut frames =
            stream::StreamChunkFrames::new(request.starting_chunk_index(self.next_index));
        while let Some(chunk) = frames.next() {
            let next_index = frames.next_chunk_index();
            if chunk.consumes_credit() && !self.credit.gate.acquire().await {
                self.next_index = next_index;
                return false;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use redoor_protocol::wire::stream::{STREAM_CREDIT_WINDOW_FRAMES, StreamChunk};

    /// Collects the chunk headers queued on a fake transfer socket.
    fn drain(receiver: &mut mpsc::Receiver<WsMessage>) -> Vec<StreamChunk> {
        let mut chunks = Vec::new();
        while let Ok(WsMessage::Binary(bytes)) = receiver.try_recv() {
            chunks.push(StreamChunk::from_bytes(&bytes).unwrap());
        }
        chunks
    }

    /// A credited writer stops after its window, resumes on a grant and never waits for the
    /// terminal frame, so a stalled server consumer cannot block cancellation acknowledgements.
    #[tokio::test]
    async fn credited_writer_honors_window_and_exempts_terminal_frames() {
        let credits = OutputCredits::default();
        let request_id = RequestId::new(5);
        let (transfer, mut receiver) = mpsc::channel(64);
        let mut writer = StreamWriter::new(
            transfer,
            request_id,
            StreamPayloadKind::RawFile,
            credits.register(request_id),
        );
        for _ in 0..STREAM_CREDIT_WINDOW_FRAMES {
            assert!(writer.send(b"x", false).await);
        }
        let blocked = tokio::spawn(async move {
            let sent = writer.send(b"y", false).await;
            (writer, sent)
        });
        tokio::task::yield_now().await;
        // The window is spent, so the fifth data frame must wait for server credit.
        assert_eq!(
            drain(&mut receiver).len(),
            STREAM_CREDIT_WINDOW_FRAMES as usize
        );
        assert!(!blocked.is_finished());
        credits.grant(request_id, 1);
        let (mut writer, sent) = blocked.await.unwrap();
        // The grant releases exactly the waiting frame.
        assert!(sent);
        assert_eq!(drain(&mut receiver).len(), 1);
        // Error frames are terminal and must go out without any credit left.
        assert!(writer.send_error(b"canceled").await);
        let terminal = drain(&mut receiver);
        assert!(terminal.len() == 1 && terminal[0].is_error);
        drop(writer);
        // Dropping the writer unregisters its gate so late grants are ignored.
        assert!(!credits.contains(request_id));
    }
}
