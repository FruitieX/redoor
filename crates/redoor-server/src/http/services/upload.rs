//! Server-to-agent uploads (raw files, archives, edits) for REST, MCP and agent upgrades.

use crate::actors::router::{
    CommitDirectUploadRequest, OutboundError, OutboundFailure, OutboundFailureReason,
    OutboundStream, RouterError, RouterHandle, RouterMsg, StartUploadRequest, UploadStartOutcome,
};
use crate::http::{agent_client::STANDARD_TIMEOUT, error::ApiError};
use axum::body::Body;
use futures_util::StreamExt;
use redoor_core::log_failure;
use redoor_protocol::{
    ids::{AgentId, ChunkIndex, RequestId},
    wire::{
        command::{Command, CommandResult},
        policy::UploadKind,
        stream::{StreamChunkFrameRequest, StreamChunkFrames, StreamPayloadKind},
    },
};
use tokio::sync::oneshot;

/// Result of starting an upload.
pub(crate) enum UploadStart {
    /// The agent worker is ready for the body.
    Ready(Box<AgentUpload>),
    /// The agent already finished successfully before asking for any byte (an empty file).
    Completed,
}

/// Owns one admitted server-to-agent upload and cancels it if its producer exits early.
pub(crate) struct AgentUpload {
    router: RouterHandle,
    agent_id: AgentId,
    kind: UploadKind,
    total_bytes: u64,
    bytes_written: u64,
    /// Writes frames straight onto the agent's transfer lane, spending per-stream credit.
    stream: OutboundStream,
    chunk_index: ChunkIndex,
    completion: oneshot::Receiver<Result<CommandResult, RouterError>>,
    cancel_guard: UploadCancelGuard,
}

impl AgentUpload {
    /// Admits an upload command and waits until the agent worker is ready for bytes.
    ///
    /// Setup failures the agent reports before readiness (missing parent, permission denied,
    /// existing destination) come back as the matching [`ApiError`].
    pub(crate) async fn start(
        router: &RouterHandle,
        agent_id: AgentId,
        command: Command,
        path: String,
        total_bytes: u64,
    ) -> Result<UploadStart, ApiError> {
        let Some(kind) = command.policy().upload_kind() else {
            return Err(ApiError::internal("Command does not upload a stream"));
        };
        let (completion_sender, completion) = oneshot::channel();
        let (ready_sender, ready) = oneshot::channel();
        let stream = router
            .request(|reply| {
                RouterMsg::StartUploadStreamRest(StartUploadRequest {
                    agent_id: agent_id.clone(),
                    command,
                    path: path.clone(),
                    total_bytes,
                    completion_sender,
                    ready_sender,
                    reply,
                })
            })
            .await??;
        let request_id = stream.request_id();
        let mut cancel_guard = UploadCancelGuard {
            router: router.clone(),
            agent_id: agent_id.clone(),
            request_id,
            armed: true,
        };
        let outcome = tokio::time::timeout(STANDARD_TIMEOUT, ready)
            .await
            .map_err(|_| ApiError::internal("Timed out waiting for upload readiness"))?
            .map_err(|_| ApiError::internal("Upload readiness channel closed"))?;
        match outcome {
            Ok(UploadStartOutcome::Ready) => {}
            Ok(UploadStartOutcome::Finished(result)) => {
                cancel_guard.armed = false;
                Self::check_completion(kind, (*result)?)?;
                return Ok(UploadStart::Completed);
            }
            Err(error) => {
                cancel_guard.armed = false;
                return Err(error.into());
            }
        }
        Ok(UploadStart::Ready(Box::new(Self {
            router: router.clone(),
            agent_id,
            kind,
            total_bytes,
            bytes_written: 0,
            stream,
            chunk_index: ChunkIndex::new(0),
            completion,
            cancel_guard,
        })))
    }

    /// Request id of the upload, which public upload-request tokens map to transfers.
    pub(crate) fn request_id(&self) -> RequestId {
        self.stream.request_id()
    }

    /// Streams an HTTP request body into the upload without buffering it.
    pub(crate) async fn forward_body(&mut self, body: Body) -> Result<(), ApiError> {
        let mut body_stream = body.into_data_stream();
        while let Some(next_chunk) = body_stream.next().await {
            let data = next_chunk.map_err(|error| {
                ApiError::bad_request(format!("Failed to read request body: {error}"))
            })?;
            self.send(&data).await?;
        }
        Ok(())
    }

    /// Forwards one producer chunk while enforcing the declared total byte count.
    pub(crate) async fn send(&mut self, data: &[u8]) -> Result<(), ApiError> {
        self.bytes_written += data.len() as u64;
        if !self.is_tar() && self.bytes_written > self.total_bytes {
            let message = format!(
                "{} exceeded Content-Length header: expected {} bytes, received {}",
                self.operation_name(),
                self.total_bytes,
                self.bytes_written
            );
            let result = self
                .forward(
                    StreamChunkFrameRequest::new(self.request_id(), message.as_bytes())
                        .is_error(true),
                )
                .await;
            self.cancel_guard.armed = false;
            result?;
            // The error frame bypasses the router, so tell it why the transfer failed.
            let _ = self
                .router
                .send_async(RouterMsg::OutboundStreamFailed(OutboundFailure {
                    agent_id: self.agent_id.clone(),
                    request_id: self.request_id(),
                    reason: OutboundFailureReason::ProducerAborted(message.clone()),
                }))
                .await;
            return Err(ApiError::bad_request(message));
        }
        let payload_kind = self.payload_kind();
        self.forward(
            StreamChunkFrameRequest::new(self.request_id(), data)
                .payload_kind(payload_kind)
                .is_last(false),
        )
        .await
    }

    /// Ends the body and waits for the agent to publish it; returns the bytes written.
    ///
    /// Edits hand their terminal frame to the router, which owns the commit boundary.
    pub(crate) async fn finish(mut self) -> Result<u64, ApiError> {
        if !self.is_tar() && self.bytes_written != self.total_bytes {
            return Err(ApiError::bad_request(format!(
                "{} stream ended before completion: expected {} bytes, received {}",
                self.operation_name(),
                self.total_bytes,
                self.bytes_written
            )));
        }
        if self.kind == UploadKind::EditFile {
            self.commit_edit().await?;
        } else {
            let payload_kind = self.payload_kind();
            self.forward(
                StreamChunkFrameRequest::new(self.request_id(), &[]).payload_kind(payload_kind),
            )
            .await?;
        }
        let result = (&mut self.completion).await.map_err(|error| {
            ApiError::internal(format!("Failed waiting for upload completion: {error}"))
        })??;
        self.cancel_guard.armed = false;
        Self::check_completion(self.kind, result)?;
        Ok(self.bytes_written)
    }

    /// Accepts only the success result of this upload kind, so another write operation's
    /// response can never report this upload as done.
    fn check_completion(kind: UploadKind, result: CommandResult) -> Result<(), ApiError> {
        match result {
            CommandResult::Error { kind, message } => Err(ApiError::Command { kind, message }),
            result if kind.completion_matches(&result) => Ok(()),
            _ => Err(ApiError::Unexpected("completing an upload")),
        }
    }

    /// Reframes one producer payload and sends it directly to the agent, never through the
    /// router; only a lost lane is reported to the router as a terminal event.
    async fn forward(&mut self, request: StreamChunkFrameRequest<'_>) -> Result<(), ApiError> {
        match self
            .stream
            .send_frames(&mut self.chunk_index, request)
            .await
        {
            Ok(()) => Ok(()),
            Err(OutboundError::Stopped) => Err(RouterError::ClientCanceledUpload.into()),
            Err(OutboundError::LaneClosed) => {
                let _ = self
                    .router
                    .send_async(RouterMsg::OutboundStreamFailed(OutboundFailure {
                        agent_id: self.agent_id.clone(),
                        request_id: self.request_id(),
                        reason: OutboundFailureReason::LaneClosed,
                    }))
                    .await;
                Err(RouterError::UploadForwardFailed {
                    agent_id: self.agent_id.to_string(),
                    request_id: self.request_id().to_string(),
                }
                .into())
            }
        }
    }

    /// Whether the body is a tar archive of unknown length.
    fn is_tar(&self) -> bool {
        self.kind == UploadKind::TarUpload
    }

    /// Names the semantic operation in transport errors.
    fn operation_name(&self) -> &'static str {
        if self.kind == UploadKind::EditFile {
            "File edit"
        } else {
            "Upload"
        }
    }

    /// Payload framing the agent worker expects for data and terminal frames alike.
    fn payload_kind(&self) -> StreamPayloadKind {
        self.kind.payload_kind()
    }

    /// Atomically publishes the edit boundary with router ownership of its terminal frame.
    async fn commit_edit(&mut self) -> Result<(), ApiError> {
        let mut frames = StreamChunkFrames::new(
            StreamChunkFrameRequest::new(self.request_id(), &[])
                .starting_chunk_index(self.chunk_index),
        );
        let terminal = frames
            .next()
            .expect("an empty terminal request always emits one frame");
        self.chunk_index = frames.next_chunk_index();
        let (reply, response) = oneshot::channel();
        self.router
            .send_async(RouterMsg::CommitDirectUpload(CommitDirectUploadRequest {
                agent_id: self.agent_id.clone(),
                request_id: self.request_id(),
                chunk: terminal,
                reply,
            }))
            .await
            .map_err(|error| {
                ApiError::internal(format!("Failed to hand off edit commit: {error}"))
            })?;
        // Once queued, router-owned work either delivers the terminal or starts worker cleanup.
        self.cancel_guard.armed = false;
        tokio::time::timeout(STANDARD_TIMEOUT, response)
            .await
            .map_err(|_| ApiError::internal("Timed out waiting for edit commit handoff"))?
            .map_err(|_| ApiError::internal("Edit commit handoff channel closed"))??;
        Ok(())
    }
}

/// Cancels an upload whose producer exits before it reaches a terminal state.
struct UploadCancelGuard {
    router: RouterHandle,
    agent_id: AgentId,
    request_id: RequestId,
    armed: bool,
}

impl Drop for UploadCancelGuard {
    /// Waits for mailbox capacity in a task: a lost cancel would leave the agent waiting for
    /// bytes that never come.
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let router = self.router.clone();
        let agent_id = self.agent_id.clone();
        let request_id = self.request_id;
        tokio::spawn(async move {
            if let Err(error) = router
                .send_async(RouterMsg::CancelTransfer {
                    agent_id: agent_id.clone(),
                    request_id,
                })
                .await
            {
                log_failure!(
                    redoor_protocol::wire::log::Level::Error,
                    "Failed to queue dropped upload cleanup: agent_id={}, request_id={}, error={}",
                    agent_id,
                    request_id,
                    error
                );
            }
        });
    }
}
