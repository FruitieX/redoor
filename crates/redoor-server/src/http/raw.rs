//! Streamed raw file downloads (with single-range resume) and one-time download tokens.

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State as AxumState},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use headers::{HeaderMap, HeaderMapExt, Range as RangeHeader};
use redoor_protocol::{
    api::files::CreateOneTimeTokenResponse, ids::AgentId, wire::command::Command,
};
use serde::Deserialize;
use uuid::Uuid;

use super::{
    agent_client::Metadata,
    agent_helpers::{AgentFilePath, absolute_path_from_url},
    error::ApiError,
    services::output::{AgentStreamError, OutputStream},
    state::ServerState,
};
use crate::actors::router::OutputStreamTracking;
use futures_util::StreamExt;

pub(crate) mod upload;

/// Records the bytes one token-authorized HTTP body handed off, including partial bodies on drop.
struct OneTimeDownloadProgress {
    registry: crate::one_time_token_registry::OneTimeTokenRegistry,
    agent_id: AgentId,
    path: String,
    token: Uuid,
    start: u64,
    next_offset: u64,
    file_size: u64,
}

impl OneTimeDownloadProgress {
    /// Advances when Axum accepts an item so browser retry offsets cannot leave one frame uncredited.
    fn record_bytes(&mut self, bytes: u64) {
        self.next_offset = self.next_offset.saturating_add(bytes).min(self.file_size);
    }
}

impl Drop for OneTimeDownloadProgress {
    /// Persists partial coverage on cancellation and consumes the token when merged coverage is complete.
    fn drop(&mut self) {
        self.registry.record_downloaded_range(
            &self.agent_id,
            &self.path,
            &self.token,
            self.start..self.next_offset,
            self.file_size,
        );
    }
}

/// Controls download presentation and optional cookie-free one-time authorization.
#[derive(Deserialize)]
pub(crate) struct RawQueryParams {
    /// Preserves the authenticated API's explicit attachment option.
    pub(crate) download: Option<String>,
    /// Authorizes exactly one download for the token's bound agent and absolute path.
    pub(crate) one_time_token: Option<Uuid>,
}

/// Parse Range header and return (start, end) byte positions (inclusive)
/// Returns None if no valid range can be satisfied
/// Only supports a single range for simplicity
fn parse_range_header(range: &RangeHeader, file_size: u64) -> Option<(u64, u64)> {
    use std::ops::Bound;

    let mut ranges = range.satisfiable_ranges(file_size);
    let (start_bound, end_bound) = ranges.next()?;

    let start = match start_bound {
        Bound::Included(start) => start,
        Bound::Excluded(start) => start + 1,
        Bound::Unbounded => 0,
    };

    let end = match end_bound {
        Bound::Included(end) => std::cmp::min(end, file_size - 1),
        Bound::Excluded(end) => std::cmp::min(end.saturating_sub(1), file_size - 1),
        Bound::Unbounded => file_size - 1,
    };

    if start >= file_size || start > end {
        return None;
    }

    Some((start, end))
}

/// Route: `GET /api/v1/agents/{agent}/raw/{*path}`
///
/// Files stream as raw bytes (optional Range). Directories reuse the agent tar
/// download path so the browser can save a folder as an archive without a
/// separate copy endpoint.
pub(crate) async fn raw_agent_handler(
    Path(AgentFilePath { agent, path }): Path<AgentFilePath>,
    Query(params): Query<RawQueryParams>,
    AxumState(state): AxumState<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let path = absolute_path_from_url(path.unwrap_or_default());
    let agent_id = AgentId::from(agent);
    let token_download = if let Some(one_time_token) = params.one_time_token.as_ref() {
        if !state
            .one_time_token_registry
            .contains(&agent_id, &path, one_time_token)
        {
            return Err(ApiError::status(
                StatusCode::UNAUTHORIZED,
                "Invalid or expired one-time token",
            ));
        }
        true
    } else {
        false
    };
    let metadata = state
        .agents
        .call(&agent_id, Metadata { path: path.clone() })
        .await?;

    if metadata.is_dir {
        return stream_directory_archive(
            state,
            agent_id,
            path,
            headers.typed_get::<RangeHeader>().is_some(),
        )
        .await;
    }

    let range_option = headers.typed_get::<RangeHeader>();
    let (range_start, range_end, status_code, content_length, content_range_header) =
        if let Some(range) = range_option {
            match parse_range_header(&range, metadata.file_size) {
                Some((start, end)) => {
                    let length = end - start + 1;
                    let content_range = format!("bytes {}-{}/{}", start, end, metadata.file_size);
                    (
                        Some(start),
                        Some(end + 1),
                        StatusCode::PARTIAL_CONTENT,
                        length,
                        Some(content_range),
                    )
                }
                None => {
                    // The Content-Range header tells the client the real size to retry with.
                    return Ok((
                        StatusCode::RANGE_NOT_SATISFIABLE,
                        [("Content-Range", format!("bytes */{}", metadata.file_size))],
                        ApiError::status(
                            StatusCode::RANGE_NOT_SATISFIABLE,
                            "Range not satisfiable",
                        ),
                    )
                        .into_response());
                }
            }
        } else {
            (None, None, StatusCode::OK, metadata.file_size, None)
        };

    let mut output = OutputStream::open(
        &state.router_ref,
        agent_id.clone(),
        Command::RawDownload {
            path: path.clone(),
            range_start,
            range_end,
        },
        OutputStreamTracking::Download {
            path: path.clone(),
            total_bytes: content_length,
            full_size: Some(metadata.file_size),
            resume_offset: range_start,
        },
    )
    .await?;
    ensure_download_started(&mut output, &path).await?;
    let mut one_time_progress = params.one_time_token.map(|token| OneTimeDownloadProgress {
        registry: state.one_time_token_registry.clone(),
        agent_id: agent_id.clone(),
        path: path.clone(),
        token,
        start: range_start.unwrap_or(0),
        next_offset: range_start.unwrap_or(0),
        file_size: metadata.file_size,
    });
    // Credits each item as Axum accepts it; the progress records itself when the body drops.
    let body_stream = output.inspect(move |item| {
        if let (Ok(bytes), Some(progress)) = (item, one_time_progress.as_mut()) {
            progress.record_bytes(bytes.len() as u64);
        }
    });

    let mut response_builder = Response::builder()
        .status(status_code)
        .header("Content-Type", metadata.mime_type)
        .header("Content-Length", content_length.to_string())
        .header("Accept-Ranges", "bytes");

    if let Some(content_range) = content_range_header {
        response_builder = response_builder.header("Content-Range", content_range);
    }

    if token_download || params.download.as_deref() == Some("1") {
        let filename = path.split('/').next_back().unwrap_or("file");
        response_builder = response_builder.header(
            "Content-Disposition",
            format!("attachment; filename=\"{}\"", filename),
        );
    }

    Ok(response_builder
        .body(Body::from_stream(body_stream))
        .unwrap()
        .into_response())
}

/// Streams a directory as gzip-compressed tar using the existing agent tar download worker.
///
/// Compression stays on the REST edge so agent↔agent copy can keep piping plain tar without
/// gunzip support or a second transfer payload kind.
async fn stream_directory_archive(
    state: ServerState,
    agent_id: AgentId,
    path: String,
    has_range_header: bool,
) -> Result<Response, ApiError> {
    // Tar size is unknown until the agent finishes walking the tree, so byte ranges
    // cannot be satisfied the way they are for fixed-length file downloads.
    if has_range_header {
        return Err(ApiError::bad_request(
            "Range requests are not supported for directory archive downloads",
        ));
    }

    let mut output = OutputStream::open(
        &state.router_ref,
        agent_id,
        Command::TarDownload {
            path: path.clone(),
            include_root: true,
        },
        OutputStreamTracking::Download {
            path: path.clone(),
            // Archive length is unknown at start so the body can begin immediately without
            // Content-Length. A parallel metadata walk may publish a total later; completion
            // still promotes counted bytes if that walk is late. Counts are plain tar bytes
            // from the agent, before REST-edge gzip.
            total_bytes: 0,
            full_size: None,
            resume_offset: None,
        },
    )
    .await?;
    ensure_download_started(&mut output, &path).await?;

    // Gzip only the HTTP body: agent still emits plain tar for reuse by copy uploads.
    let tar_reader = tokio_util::io::StreamReader::new(output);
    let gzip_reader = async_compression::tokio::bufread::GzipEncoder::new(tar_reader);
    let gzipped_stream = tokio_util::io::ReaderStream::new(gzip_reader);

    let leaf_name = path.split('/').next_back().filter(|name| !name.is_empty());
    let archive_name = match leaf_name {
        Some(name) => format!("{name}.tar.gz"),
        None => "archive.tar.gz".to_string(),
    };

    // Always attachment: directory bytes are an archive, never inline browser content.
    // application/gzip keeps the payload as a downloadable .tar.gz rather than transparent
    // Content-Encoding decompression that would leave clients with raw tar bytes.
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/gzip")
        .header(
            "Content-Disposition",
            format!("attachment; filename=\"{archive_name}\""),
        )
        .body(Body::from_stream(gzipped_stream))
        .unwrap()
        .into_response())
}

/// Waits for the first agent frame so an immediate failure can still be answered with a JSON
/// error and a fitting status; once the body starts, failures can only abort the stream.
async fn ensure_download_started(output: &mut OutputStream, path: &str) -> Result<(), ApiError> {
    let first = output
        .peek()
        .await
        .ok_or_else(|| ApiError::internal("No data received"))?;
    let Err(AgentStreamError(message)) = first else {
        return Ok(());
    };
    let message = if message.is_empty() {
        format!("File error: {path}")
    } else {
        message.clone()
    };
    // Download workers report I/O failures as text, so classify the common cases by message.
    let status = if message.contains("No such file") || message.contains("not found") {
        StatusCode::NOT_FOUND
    } else if message.contains("Permission denied") {
        StatusCode::FORBIDDEN
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    Err(ApiError::status(status, message))
}

/// Route: `POST /api/v1/agents/{agent}/one-time-token/{*path}`
pub(crate) async fn create_one_time_token_handler(
    Path(AgentFilePath { agent, path }): Path<AgentFilePath>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<CreateOneTimeTokenResponse>, ApiError> {
    let path = absolute_path_from_url(path.unwrap_or_default());
    let agent_id = AgentId::from(agent);
    let metadata = state.agents.call(&agent_id, Metadata { path }).await?;
    if !metadata.is_file {
        return Err(ApiError::bad_request(
            "One-time download links can only be created for files",
        ));
    }
    let one_time_token = state
        .one_time_token_registry
        .create(agent_id, metadata.path)
        .to_string();
    Ok(Json(CreateOneTimeTokenResponse { one_time_token }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{actors::router::RouterMsg, http::fake_agent::FakeAgent};
    use redoor_protocol::{
        api::transfers::TransferProgressState,
        wire::{ServerToAgent, stream::StreamChunk},
    };

    /// Dropping an HTTP body must cancel even when the agent never sends another chunk.
    #[tokio::test]
    async fn body_drop_cancels_download_without_subsequent_chunk() {
        let mut agent = FakeAgent::connect().await;
        let path = "/tmp/stalled.bin".to_string();
        let mut body = OutputStream::open(
            &agent.router,
            agent.agent_id.clone(),
            Command::RawDownload {
                path: path.clone(),
                range_start: None,
                range_end: None,
            },
            OutputStreamTracking::Download {
                path: path.clone(),
                total_bytes: 2,
                full_size: Some(2),
                resume_offset: None,
            },
        )
        .await
        .expect("download start accepted");
        let request_id = body.request_id();
        let (command_request_id, _) = agent.next_command().await;
        // Returning the exact command id lets the body cancel the router-owned transfer.
        assert_eq!(request_id, command_request_id);

        // An uncredited agent's reader hands the frame over directly, without the router.
        let deliver = tokio::spawn({
            let streams = agent.streams.clone();
            async move {
                streams.deliver(StreamChunk {
                    request_id,
                    is_last: false,
                    data: vec![b'a'],
                })
            }
        });
        ensure_download_started(&mut body, &path)
            .await
            .expect("first chunk starts body");
        let first_bytes = body
            .next()
            .await
            .expect("body yielded first chunk")
            .expect("first chunk was successful");
        // Receiving one nonterminal byte proves cancellation does not depend on another frame.
        assert_eq!(first_bytes.as_ref(), b"a");
        // A nonterminal frame is not reported to the router.
        assert!(deliver.await.expect("first chunk delivered").is_none());

        drop(body);

        // The agent receives cancellation for the stalled request without a follow-up chunk.
        assert!(matches!(
            agent.next_priority().await,
            ServerToAgent::Cancel { id: canceled ,} if canceled == request_id
        ));
        let progress = agent
            .router
            .request(|reply| RouterMsg::GetTransferProgress { reply })
            .await
            .expect("progress request completed");
        let transfer = progress
            .transfers
            .iter()
            .find(|transfer| transfer.request_id == request_id.as_transfer_id())
            .expect("download progress retained");
        // Terminal progress proves router state no longer remains indefinitely active.
        assert!(matches!(transfer.state, TransferProgressState::Errored));
    }
}
