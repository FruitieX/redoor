use crate::actors;
use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State as AxumState},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use headers::HeaderMap;
use redoor_protocol::{
    api::{
        ErrorResponse,
        files::{CreationOwnershipOptions, RawUploadResponse},
    },
    ids::{AgentId, ChunkIndex, RequestId},
    wire::{
        command::{Command, CommandResult},
        stream::{StreamChunkFrameRequest, StreamChunkFrames},
    },
};

use super::super::{
    agent_helpers::{AgentFilePath, absolute_path_from_url},
    responses::{command_error_status, router_error_response},
    state::ServerState,
};

/// Triggers router-side upload cancellation if the HTTP handler exits before the
/// upload reaches a terminal state.
struct UploadCancelGuard {
    router_ref: actors::router::RouterHandle,
    agent_id: AgentId,
    request_id: RequestId,
    active: bool,
}

/// Distinguishes setup failures from an agent response that completed before readiness.
pub(crate) enum AgentUploadStartError {
    /// Router or readiness transport failures already mapped to the standard JSON response.
    Response(Response),
    /// Agent-side setup can finish before the transfer websocket reports ready.
    Finished(Box<Result<CommandResult, actors::router::RouterError>>),
}

/// Selects creation metadata and replacement behavior for one raw upload.
#[derive(serde::Deserialize)]
pub(crate) struct RawUploadQueryParams {
    /// Keeps existing raw PUT clients replacing files unless they opt into strict creation.
    #[serde(default = "redoor_protocol::api::transfers::CopyExistingMode::override_mode")]
    pub(crate) on_existing: redoor_protocol::api::transfers::CopyExistingMode,
    /// Accepts either a user name or numeric UID for the new file.
    pub(crate) owner: Option<String>,
    /// Accepts either a group name or numeric GID for the new file.
    pub(crate) group: Option<String>,
    /// Lets callers explicitly request or disable parent-owner inheritance.
    pub(crate) inherit_owner: Option<bool>,
    /// Lets callers explicitly request or disable parent-group inheritance.
    pub(crate) inherit_group: Option<bool>,
}

impl RawUploadQueryParams {
    /// Restores the shared ownership value after top-level query parsing handles booleans.
    fn ownership(&self) -> CreationOwnershipOptions {
        CreationOwnershipOptions {
            owner: self.owner.clone(),
            group: self.group.clone(),
            inherit_owner: self.inherit_owner,
            inherit_group: self.inherit_group,
        }
    }
}

/// Owns one initialized server-to-agent upload and cancels it if its producer exits early.
pub(crate) struct AgentUpload {
    state: ServerState,
    agent_id: AgentId,
    path: String,
    total_bytes: u64,
    bytes_written: u64,
    /// Lets owning HTTP handlers correlate cancellation without scanning shared progress paths.
    pub(crate) request_id: RequestId,
    /// Writes frames straight onto the agent's transfer lane, spending per-stream credit.
    stream: actors::router::OutboundStream,
    chunk_index: ChunkIndex,
    completion_receiver:
        tokio::sync::oneshot::Receiver<Result<CommandResult, actors::router::RouterError>>,
    cancel_guard: UploadCancelGuard,
    is_edit: bool,
    /// Archive bodies have unknown length and must keep tar framing through the transfer lane.
    is_tar: bool,
}

/// Parses the required byte count shared by streamed write endpoints.
pub(crate) fn required_content_length(
    headers: &HeaderMap,
    operation: &str,
) -> Result<u64, Box<Response>> {
    let Some(header_value) = headers.get(axum::http::header::CONTENT_LENGTH) else {
        return Err(Box::new(
            (
                StatusCode::LENGTH_REQUIRED,
                Json(ErrorResponse {
                    error: format!("Content-Length header is required for {operation}"),
                }),
            )
                .into_response(),
        ));
    };
    let total_bytes = header_value
        .to_str()
        .ok()
        .and_then(|value| value.parse::<u64>().ok());
    total_bytes.ok_or_else(|| {
        Box::new(
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Invalid Content-Length header".to_string(),
                }),
            )
                .into_response(),
        )
    })
}

/// Streams one Axum request body into the bounded router-to-agent transfer path.
pub(crate) async fn forward_request_body(
    body: Body,
    upload: &mut AgentUpload,
) -> Result<(), Box<Response>> {
    let mut body_stream = body.into_data_stream();
    while let Some(next_chunk) = body_stream.next().await {
        let data = next_chunk.map_err(|error| {
            Box::new(
                (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: format!("Failed to read request body: {error}"),
                    }),
                )
                    .into_response(),
            )
        })?;
        upload.send(&data).await.map_err(Box::new)?;
    }
    Ok(())
}

impl AgentUpload {
    /// Starts a transfer and waits until its dedicated websocket is ready for bounded chunks.
    pub(crate) async fn start(
        state: &ServerState,
        agent_id: AgentId,
        command: Command,
        path: String,
        total_bytes: u64,
    ) -> Result<Self, AgentUploadStartError> {
        let upload_kind = command.policy().upload_kind();
        let is_edit = upload_kind == Some(redoor_protocol::wire::policy::UploadKind::EditFile);
        let is_tar = upload_kind == Some(redoor_protocol::wire::policy::UploadKind::TarUpload);
        let (completion_sender, completion_receiver) = tokio::sync::oneshot::channel();
        let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel();
        let stream = match state
            .router_ref
            .request(30000, |reply| {
                actors::router::RouterMsg::StartUploadStreamRest(
                    actors::router::StartUploadRequest {
                        agent_id: agent_id.clone(),
                        command,
                        path: path.clone(),
                        total_bytes,
                        completion_sender,
                        ready_sender,
                        reply,
                    },
                )
            })
            .await
        {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => {
                return Err(AgentUploadStartError::Response(router_error_response(
                    error,
                )));
            }
            Err(error) => {
                return Err(AgentUploadStartError::Response(
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(ErrorResponse {
                            error: format!("Failed to start upload: {error:?}"),
                        }),
                    )
                        .into_response(),
                ));
            }
        };
        let request_id = stream.request_id();
        let mut cancel_guard =
            UploadCancelGuard::new(state.router_ref.clone(), agent_id.clone(), request_id);
        match tokio::time::timeout(std::time::Duration::from_millis(30000), ready_receiver).await {
            Ok(Ok(Ok(actors::router::UploadStartOutcome::Ready))) => {}
            Ok(Ok(Ok(actors::router::UploadStartOutcome::Finished(completion)))) => {
                cancel_guard.disarm();
                return Err(AgentUploadStartError::Finished(completion));
            }
            Ok(Ok(Err(error))) => {
                cancel_guard.disarm();
                return Err(AgentUploadStartError::Response(router_error_response(
                    error,
                )));
            }
            Ok(Err(_)) => {
                return Err(AgentUploadStartError::Response(
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(ErrorResponse {
                            error: "Upload readiness channel closed".to_string(),
                        }),
                    )
                        .into_response(),
                ));
            }
            Err(_) => {
                return Err(AgentUploadStartError::Response(
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(ErrorResponse {
                            error: "Timed out waiting for upload readiness".to_string(),
                        }),
                    )
                        .into_response(),
                ));
            }
        }

        Ok(Self {
            state: state.clone(),
            agent_id,
            path,
            total_bytes,
            bytes_written: 0,
            request_id,
            stream,
            chunk_index: ChunkIndex::new(0),
            completion_receiver,
            cancel_guard,
            is_edit,
            is_tar,
        })
    }

    /// Forwards one producer chunk while enforcing the declared total byte count.
    pub(crate) async fn send(&mut self, data: &[u8]) -> Result<(), Response> {
        self.bytes_written += data.len() as u64;
        if !self.is_tar && self.bytes_written > self.total_bytes {
            let message = format!(
                "{} exceeded Content-Length header: expected {} bytes, received {}",
                self.operation_name(),
                self.total_bytes,
                self.bytes_written
            );
            let result = self
                .forward(
                    StreamChunkFrameRequest::new(self.request_id, message.as_bytes())
                        .is_error(true),
                )
                .await;
            self.cancel_guard.disarm();
            result?;
            // The error frame bypasses the router, so tell it why the transfer failed.
            let _ = self
                .state
                .router_ref
                .send_async(actors::router::RouterMsg::OutboundStreamFailed(
                    actors::router::OutboundFailure {
                        agent_id: self.agent_id.clone(),
                        request_id: self.request_id,
                        reason: actors::router::OutboundFailureReason::ProducerAborted(
                            message.clone(),
                        ),
                    },
                ))
                .await;
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse { error: message }),
            )
                .into_response());
        }
        let payload_kind = self.payload_kind();
        self.forward(
            StreamChunkFrameRequest::new(self.request_id, data)
                .payload_kind(payload_kind)
                .is_last(false),
        )
        .await
    }

    /// Reframes one producer payload and sends it directly to the agent, never through the
    /// router; only a lost lane is reported to the router as a terminal event.
    async fn forward(&mut self, request: StreamChunkFrameRequest<'_>) -> Result<(), Response> {
        match self
            .stream
            .send_frames(&mut self.chunk_index, request)
            .await
        {
            Ok(()) => Ok(()),
            Err(actors::router::OutboundError::Stopped) => Err(router_error_response(
                actors::router::RouterError::ClientCanceledUpload,
            )),
            Err(actors::router::OutboundError::LaneClosed) => {
                let _ = self
                    .state
                    .router_ref
                    .send_async(actors::router::RouterMsg::OutboundStreamFailed(
                        actors::router::OutboundFailure {
                            agent_id: self.agent_id.clone(),
                            request_id: self.request_id,
                            reason: actors::router::OutboundFailureReason::LaneClosed,
                        },
                    ))
                    .await;
                Err(router_error_response(
                    actors::router::RouterError::UploadForwardFailed {
                        agent_id: self.agent_id.to_string(),
                        request_id: self.request_id.to_string(),
                    },
                ))
            }
        }
    }

    /// Hands edit terminal ownership to the router, then awaits destination completion.
    pub(crate) async fn finish(mut self) -> Result<(CommandResult, u64), Response> {
        if !self.is_tar && self.bytes_written != self.total_bytes {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!(
                        "{} stream ended before completion: expected {} bytes, received {}",
                        self.operation_name(),
                        self.total_bytes,
                        self.bytes_written
                    ),
                }),
            )
                .into_response());
        }
        if self.is_edit {
            self.commit_edit().await?;
        } else {
            let payload_kind = self.payload_kind();
            self.forward(
                StreamChunkFrameRequest::new(self.request_id, &[]).payload_kind(payload_kind),
            )
            .await?;
        }
        match self.completion_receiver.await {
            Ok(Ok(completion)) => {
                self.cancel_guard.disarm();
                Ok((completion, self.bytes_written))
            }
            Ok(Err(error)) => Err(router_error_response(error)),
            Err(error) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Failed waiting for upload completion: {error}"),
                }),
            )
                .into_response()),
        }
    }

    /// Returns the destination for response mapping without exposing transfer internals.
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    /// Names the semantic operation in transport errors without changing shared streaming logic.
    fn operation_name(&self) -> &'static str {
        if self.is_edit { "File edit" } else { "Upload" }
    }

    /// Preserves the agent worker's payload contract for data and terminal frames alike.
    fn payload_kind(&self) -> redoor_protocol::wire::stream::StreamPayloadKind {
        if self.is_tar {
            redoor_protocol::wire::stream::StreamPayloadKind::Tar
        } else {
            redoor_protocol::wire::stream::StreamPayloadKind::RawFile
        }
    }

    /// Atomically publishes the edit boundary with router ownership of its terminal frame.
    async fn commit_edit(&mut self) -> Result<(), Response> {
        let mut frames = StreamChunkFrames::new(
            StreamChunkFrameRequest::new(self.request_id, &[])
                .starting_chunk_index(self.chunk_index),
        );
        let terminal = frames
            .next()
            .expect("an empty terminal request always emits one frame");
        self.chunk_index = frames.next_chunk_index();
        let (reply, response) = tokio::sync::oneshot::channel();
        self.state
            .router_ref
            .send_async(actors::router::RouterMsg::CommitDirectUpload(
                actors::router::CommitDirectUploadRequest {
                    agent_id: self.agent_id.clone(),
                    request_id: self.request_id,
                    chunk: terminal,
                    reply,
                },
            ))
            .await
            .map_err(|error| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: format!("Failed to hand off edit commit: {error:?}"),
                    }),
                )
                    .into_response()
            })?;

        // Once queued, router-owned work either delivers the terminal or starts worker cleanup.
        self.cancel_guard.disarm();
        match tokio::time::timeout(std::time::Duration::from_millis(30000), response).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(error))) => Err(router_error_response(error)),
            Ok(Err(_)) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Edit commit handoff channel closed".to_string(),
                }),
            )
                .into_response()),
            Err(_) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Timed out waiting for edit commit handoff".to_string(),
                }),
            )
                .into_response()),
        }
    }
}

impl UploadCancelGuard {
    /// Arms a new guard for one active upload request.
    fn new(
        router_ref: actors::router::RouterHandle,
        agent_id: AgentId,
        request_id: RequestId,
    ) -> Self {
        Self {
            router_ref,
            agent_id,
            request_id,
            active: true,
        }
    }

    /// Disables drop-driven cancellation once the upload has already been
    /// finalized or explicitly aborted through the normal chunk flow.
    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for UploadCancelGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }

        let router_ref = self.router_ref.clone();
        let agent_id = self.agent_id.clone();
        let request_id = self.request_id;
        tokio::spawn(async move {
            if let Err(error) = router_ref
                .send_async(actors::router::RouterMsg::CancelTransfer {
                    agent_id: agent_id.clone(),
                    request_id,
                })
                .await
            {
                redoor_core::log_failure!(
                    Level::Error,
                    "Failed to queue dropped upload cleanup: agent_id={}, request_id={}, error={}",
                    agent_id,
                    request_id,
                    error
                );
            }
        });
    }
}

/// Route: `PUT /api/v1/agents/{agent}/raw/{*path}`
pub(crate) async fn raw_agent_put_handler(
    Path(AgentFilePath { agent, path }): Path<AgentFilePath>,
    Query(params): Query<RawUploadQueryParams>,
    AxumState(state): AxumState<ServerState>,
    headers: HeaderMap,
    body: Body,
) -> impl IntoResponse {
    let path = absolute_path_from_url(path.unwrap_or_default());
    let agent_id = AgentId::from(agent.clone());
    let ownership = params.ownership();
    if let Err(error) = ownership.validate() {
        return (StatusCode::BAD_REQUEST, Json(ErrorResponse { error })).into_response();
    }
    let total_bytes = match required_content_length(&headers, "uploads") {
        Ok(total_bytes) => total_bytes,
        Err(response) => return *response,
    };

    let resolved_path = path;
    let request_guard = match state.upload_requests.reserve(&headers) {
        Ok(guard) => guard,
        Err(response) => return *response,
    };
    let mut upload = match AgentUpload::start(
        &state,
        agent_id,
        Command::RawUpload {
            path: resolved_path.clone(),
            on_existing: params.on_existing,
            ownership,
        },
        resolved_path.clone(),
        total_bytes,
    )
    .await
    {
        Ok(upload) => upload,
        Err(AgentUploadStartError::Response(response)) => return response,
        Err(AgentUploadStartError::Finished(completion)) => {
            return raw_upload_completion_response(*completion, &resolved_path, 0);
        }
    };

    if request_guard
        .as_ref()
        .is_some_and(|guard| !guard.activate(upload.request_id.as_transfer_id()))
    {
        return (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "Upload request canceled during setup".into(),
            }),
        )
            .into_response();
    }
    if let Err(response) = forward_request_body(body, &mut upload).await {
        return *response;
    }
    let response_path = upload.path().to_string();
    match upload.finish().await {
        Ok((completion, bytes_written)) => {
            raw_upload_completion_response(Ok(completion), &response_path, bytes_written)
        }
        Err(response) => response,
    }
}

/// Maps one upload completion (including early init failures) to the established HTTP response.
fn raw_upload_completion_response(
    completion: Result<CommandResult, actors::router::RouterError>,
    path: &str,
    bytes_written: u64,
) -> Response {
    match completion {
        Ok(CommandResult::RawUpload) => (
            StatusCode::OK,
            Json(RawUploadResponse {
                path: path.to_string(),
                bytes_written,
            }),
        )
            .into_response(),
        Ok(CommandResult::Error { kind, message }) => {
            let status = command_error_status(&kind);
            (status, Json(ErrorResponse { error: message })).into_response()
        }
        Ok(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Unexpected upload completion response".to_string(),
            }),
        )
            .into_response(),
        Err(error) => router_error_response(error),
    }
}
