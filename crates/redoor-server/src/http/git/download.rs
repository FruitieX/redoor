//! REST endpoint that downloads a committed file or directory from Git history.

use axum::{
    body::Body,
    extract::{Path, Query, State as AxumState},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use redoor_protocol::{
    api::git::revision::GitRevisionQuery,
    wire::{
        command::{Command, GitRevisionStatKind},
        request,
    },
};

use super::{checked_revision, route_path};
use crate::actors::router::OutputStreamTracking;
use crate::http::{
    access::ResolvedDevice,
    agent_client::GitRevisionStat,
    agent_helpers::AgentFilePath,
    error::ApiError,
    raw::{attachment_disposition, ensure_download_started, stream_directory_archive},
    services::output::OutputStream,
    state::ServerState,
};

/// Route: `GET /api/v1/agents/{agent}/git/download[/{*path}]?rev=`.
///
/// Files stream as raw bytes and directories as a gzip-compressed tar, like the raw download,
/// but always as attachments so committed content is never shown inline as if it were the
/// working file. The revision is resolved to a commit once and that commit is streamed, so a
/// branch moving mid-download cannot mix versions. Ranges are not offered: history downloads
/// are small enough to restart and have no source identity to resume against.
pub(crate) async fn git_download_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    Path(params): Path<AgentFilePath>,
    Query(query): Query<GitRevisionQuery>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Response, ApiError> {
    let path = route_path(params.path)?;
    let stat = state
        .agents
        .call(
            &agent_id,
            GitRevisionStat {
                path: path.clone(),
                rev: checked_revision(query.rev)?,
                replace_target: None,
            },
        )
        .await?;
    let size = match stat.kind {
        GitRevisionStatKind::File { size, .. } => size,
        GitRevisionStatKind::Directory => {
            let command = Command::GitDownload(request::GitDownload {
                path: path.clone(),
                rev: stat.commit_id,
                include_root: true,
            });
            return stream_directory_archive(state, agent_id, path, command).await;
        }
        GitRevisionStatKind::Symlink | GitRevisionStatKind::Submodule => {
            return Err(ApiError::bad_request(
                "Only files and directories can be downloaded from a Git revision",
            ));
        }
    };
    let mut output = OutputStream::open(
        &state.router_ref,
        agent_id,
        Command::GitDownload(request::GitDownload {
            path: path.clone(),
            rev: stat.commit_id,
            include_root: false,
        }),
        OutputStreamTracking::Download {
            path: path.clone(),
            total_bytes: size,
            full_size: Some(size),
            resume_offset: None,
        },
    )
    .await?;
    ensure_download_started(&mut output, &path).await?;
    let filename = path
        .split('/')
        .next_back()
        .filter(|name| !name.is_empty())
        .unwrap_or("file");
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/octet-stream")
        .header("Content-Length", size.to_string())
        .header("Content-Disposition", attachment_disposition(filename))
        .body(Body::from_stream(output))
        .unwrap()
        .into_response())
}
