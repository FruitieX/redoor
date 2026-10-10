//! Agent-to-server output streams (downloads and process output) for REST and MCP consumers.

use crate::actors::router::{
    ExecuteStreamRequest, OutputReceiver, OutputStreamTracking, RouterHandle, RouterMsg,
};
use crate::http::error::ApiError;
use bytes::Bytes;
use futures_util::Stream;
use redoor_protocol::{
    ids::{AgentId, RequestId},
    wire::{
        command::Command,
        exec::{ExecEvent, ExecRequest},
        stream::StreamChunk,
    },
};
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use tokio::sync::watch;
use tokio_util::sync::ReusableBoxFuture;

/// Message for an agent error frame that carried no text.
const UNDESCRIBED_FAILURE: &str = "Agent stream failed";

/// The pending receive of an output stream, owning the receiver so polling can resume across
/// dropped `recv` futures and `Stream` polls alike.
type ReceiveFuture = ReusableBoxFuture<'static, (OutputReceiver, Option<StreamChunk>)>;

/// The agent ended an output stream with an error frame; the text is its (possibly empty) message.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct AgentStreamError(pub(crate) String);

/// One admitted output stream plus the cleanup its consumer owes the agent.
///
/// Dropping the stream before its terminal frame (an HTTP client disconnecting, an MCP read that
/// stopped at its byte limit) cancels the agent worker, so no consumer can forget to arm or
/// disarm cancellation. Frames are read with [`OutputStream::recv`] or, for byte consumers,
/// through the [`Stream`] implementation; both share one cancel-safe receive.
pub(crate) struct OutputStream {
    request_id: RequestId,
    receive: ReceiveFuture,
    /// A frame read ahead by [`OutputStream::peek`], returned before any new frame.
    peeked: Option<StreamChunk>,
    guard: OutputCancelGuard,
    /// Set once the byte stream yielded its final item.
    finished: bool,
}

impl OutputStream {
    /// Admits an output command and returns its armed consumer.
    pub(crate) async fn open(
        router: &RouterHandle,
        agent_id: AgentId,
        command: Command,
        tracking: OutputStreamTracking,
    ) -> Result<Self, ApiError> {
        let receiver = router
            .request(|reply| {
                RouterMsg::ExecuteStreamCommandRest(ExecuteStreamRequest {
                    agent_id: agent_id.clone(),
                    command,
                    tracking,
                    reply,
                })
            })
            .await??;
        let request_id = receiver.request_id();
        Ok(Self {
            request_id,
            receive: ReusableBoxFuture::new(Self::receive(receiver)),
            peeked: None,
            guard: OutputCancelGuard {
                router: router.clone(),
                agent_id,
                request_id,
                armed: true,
            },
            finished: false,
        })
    }

    /// Admits a non-PTY execution and returns its output plus the independent terminal lane,
    /// which reports how the process ended even when the output queue is saturated.
    pub(crate) async fn open_execution(
        router: &RouterHandle,
        agent_id: AgentId,
        request: ExecRequest,
    ) -> Result<(Self, watch::Receiver<Option<ExecEvent>>), ApiError> {
        let (terminal, terminal_receiver) = watch::channel(None);
        let stream = Self::open(
            router,
            agent_id,
            Command::Exec { request },
            OutputStreamTracking::Execution { terminal },
        )
        .await?;
        Ok((stream, terminal_receiver))
    }

    /// Request id of the stream, which executions expose for cancellation.
    pub(crate) fn request_id(&self) -> RequestId {
        self.request_id
    }

    /// Releases the agent from cancellation once a terminal outcome was observed elsewhere
    /// (the execution terminal lane).
    pub(crate) fn disarm(&mut self) {
        self.guard.armed = false;
    }

    /// Receives the next frame, or `None` when the stream ended.
    ///
    /// Cancel safe: the receive in progress lives in the stream, so a `select!` that drops this
    /// future loses no frame. A terminal frame disarms cancellation because the agent worker
    /// already finished.
    pub(crate) async fn recv(&mut self) -> Option<StreamChunk> {
        if let Some(chunk) = self.peeked.take() {
            return Some(chunk);
        }
        std::future::poll_fn(|cx| self.poll_recv(cx)).await
    }

    /// Reads the first frame without consuming it, so callers can still answer with a JSON
    /// error before committing to a streaming response.
    pub(crate) async fn peek(&mut self) -> Option<&StreamChunk> {
        if self.peeked.is_none() {
            self.peeked = std::future::poll_fn(|cx| self.poll_recv(cx)).await;
        }
        self.peeked.as_ref()
    }

    /// Collects at most `limit` payload bytes into memory.
    ///
    /// Reading stops at the final frame or at the first byte beyond `limit`; dropping the
    /// stream afterwards cancels an agent that still has more. An agent error frame becomes
    /// `Err` with its message (possibly empty).
    pub(crate) async fn read_bounded(
        &mut self,
        limit: usize,
    ) -> Result<BoundedRead, AgentStreamError> {
        let mut data = Vec::with_capacity(limit.min(64 * 1024));
        while let Some(chunk) = self.recv().await {
            if chunk.is_error {
                return Err(AgentStreamError(
                    String::from_utf8_lossy(&chunk.data).into_owned(),
                ));
            }
            let room = limit - data.len();
            if chunk.data.len() > room {
                data.extend_from_slice(&chunk.data[..room]);
                return Ok(BoundedRead {
                    data,
                    overflowed: true,
                    completed: false,
                });
            }
            data.extend_from_slice(&chunk.data);
            if chunk.is_last {
                return Ok(BoundedRead {
                    data,
                    overflowed: false,
                    completed: true,
                });
            }
        }
        Ok(BoundedRead {
            data,
            overflowed: false,
            completed: false,
        })
    }

    /// One receive that hands the receiver back with its frame, so it can be restarted.
    async fn receive(mut receiver: OutputReceiver) -> (OutputReceiver, Option<StreamChunk>) {
        let chunk = receiver.recv().await;
        (receiver, chunk)
    }

    /// Polls the stored receive and immediately re-arms it for the next frame.
    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<StreamChunk>> {
        let (receiver, chunk) = ready!(self.receive.poll(cx));
        self.receive.set(Self::receive(receiver));
        if chunk
            .as_ref()
            .is_none_or(|chunk| chunk.is_last || chunk.is_error)
        {
            // The worker finished (or the router ended the stream); nothing is left to cancel.
            self.guard.armed = false;
        }
        Poll::Ready(chunk)
    }
}

/// Bytes collected by [`OutputStream::read_bounded`].
pub(crate) struct BoundedRead {
    /// At most `limit` payload bytes.
    pub(crate) data: Vec<u8>,
    /// Whether the agent sent more than `limit` bytes.
    pub(crate) overflowed: bool,
    /// Whether the agent's final frame was received.
    pub(crate) completed: bool,
}

impl Stream for OutputStream {
    type Item = std::io::Result<Bytes>;

    /// Yields payload bytes; an agent error frame ends the stream with an I/O error so an HTTP
    /// body fails visibly instead of looking like a complete, truncated file.
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        loop {
            if this.finished {
                return Poll::Ready(None);
            }
            let chunk = match this.peeked.take() {
                Some(chunk) => Some(chunk),
                None => ready!(this.poll_recv(cx)),
            };
            let Some(chunk) = chunk else {
                this.finished = true;
                return Poll::Ready(None);
            };
            if chunk.is_error {
                this.finished = true;
                let message = if chunk.data.is_empty() {
                    UNDESCRIBED_FAILURE.to_string()
                } else {
                    String::from_utf8_lossy(&chunk.data).into_owned()
                };
                return Poll::Ready(Some(Err(std::io::Error::other(message))));
            }
            this.finished = chunk.is_last;
            if !chunk.data.is_empty() {
                return Poll::Ready(Some(Ok(Bytes::from(chunk.data))));
            }
        }
    }
}

/// Cancels the agent worker of a stream whose consumer went away before its terminal frame.
struct OutputCancelGuard {
    router: RouterHandle,
    agent_id: AgentId,
    request_id: RequestId,
    armed: bool,
}

impl Drop for OutputCancelGuard {
    /// Waits for mailbox capacity in a task: a lost cancel would leave the agent streaming
    /// into a stream nobody reads.
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let router = self.router.clone();
        let agent_id = self.agent_id.clone();
        let request_id = self.request_id;
        tokio::spawn(async move {
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
impl OutputStream {
    /// Wraps a detached receiver so consumer tests need no admission round trip.
    pub(crate) fn detached(
        router: RouterHandle,
        agent_id: AgentId,
        receiver: OutputReceiver,
    ) -> Self {
        let request_id = receiver.request_id();
        Self {
            request_id,
            receive: ReusableBoxFuture::new(Self::receive(receiver)),
            peeked: None,
            guard: OutputCancelGuard {
                router,
                agent_id,
                request_id,
                armed: true,
            },
            finished: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake_agent::FakeAgent;
    use futures_util::StreamExt;
    use redoor_protocol::wire::ServerToAgent;

    /// Opens a download of `/tmp/file` on the fake agent.
    async fn download(agent: &mut FakeAgent) -> OutputStream {
        let stream = OutputStream::open(
            &agent.router,
            agent.agent_id.clone(),
            Command::RawDownload {
                path: "/tmp/file".to_string(),
                range_start: None,
                range_end: None,
            },
            OutputStreamTracking::Download {
                path: "/tmp/file".to_string(),
                total_bytes: 8,
                full_size: Some(8),
                resume_offset: None,
            },
        )
        .await
        .unwrap();
        let (request_id, _) = agent.next_command().await;
        // The consumer and the agent agree on the stream they talk about.
        assert_eq!(stream.request_id(), request_id);
        stream
    }

    /// One agent frame of the stream.
    fn frame(request_id: RequestId, data: &[u8], is_last: bool, is_error: bool) -> StreamChunk {
        StreamChunk {
            request_id,
            is_last,
            is_error,
            data: data.to_vec(),
        }
    }

    /// Dropping an unfinished stream cancels its agent worker; a finished one does not.
    #[tokio::test]
    async fn drop_before_terminal_cancels_the_agent() {
        let mut agent = FakeAgent::connect().await;
        let stream = download(&mut agent).await;
        let id = stream.request_id();
        drop(stream);
        // A consumer that vanished must not leave the agent streaming into nothing.
        assert!(matches!(
            agent.next_priority().await,
            ServerToAgent::Cancel { id: request_id } if request_id == id
        ));

        let mut stream = download(&mut agent).await;
        let id = stream.request_id();
        agent.deliver(frame(id, b"done", true, false)).await;
        // The terminal frame is still delivered to the consumer.
        assert!(stream.recv().await.is_some_and(|chunk| chunk.is_last));
        // A worker that already finished has nothing left to cancel.
        assert!(!stream.guard.armed);
    }

    /// The byte stream yields payload, then fails visibly on an agent error frame.
    #[tokio::test]
    async fn byte_stream_ends_with_error_on_agent_failure() {
        let mut agent = FakeAgent::connect().await;
        let mut stream = download(&mut agent).await;
        let id = stream.request_id();
        let delivery = tokio::spawn({
            let streams = agent.streams.clone();
            async move {
                streams.deliver(frame(id, b"", false, false));
                streams.deliver(frame(id, b"abc", false, false));
                streams.deliver(frame(id, b"disk gone", false, true))
            }
        });
        // Empty frames carry nothing for the HTTP body and are skipped.
        assert_eq!(stream.next().await.unwrap().unwrap(), Bytes::from("abc"));
        let error = stream.next().await.unwrap().unwrap_err();
        // The body aborts with the agent's message instead of ending like a complete file.
        assert_eq!(error.to_string(), "disk gone");
        assert!(stream.next().await.is_none());
        delivery.await.unwrap();
    }

    /// Bounded reads stop at the limit and report that more data exists.
    #[tokio::test]
    async fn read_bounded_stops_at_the_limit() {
        let mut agent = FakeAgent::connect().await;
        let mut stream = download(&mut agent).await;
        let id = stream.request_id();
        let delivery = tokio::spawn({
            let streams = agent.streams.clone();
            async move {
                streams.deliver(frame(id, b"abcd", false, false));
                streams.deliver(frame(id, b"efgh", false, false));
            }
        });
        let read = stream.read_bounded(6).await.unwrap();
        // Only the requested bytes are kept in memory.
        assert_eq!(read.data, b"abcdef");
        // The caller learns the content continues and that the agent did not finish.
        assert!(read.overflowed && !read.completed);
        delivery.await.unwrap();
        drop(stream);
        // Stopping early cancels the agent, which may still be producing.
        assert!(matches!(
            agent.next_priority().await,
            ServerToAgent::Cancel { id: request_id } if request_id == id
        ));
    }
}
