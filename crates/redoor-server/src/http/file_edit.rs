//! REST endpoint that saves editor content over an existing file.

use axum::{
    Json,
    body::Body,
    extract::{Path, State as AxumState},
};
use headers::HeaderMap;
use redoor_protocol::wire::request;
use redoor_protocol::{api::files::FileEditResponse, ids::AgentId, wire::command::Command};

use super::{
    agent_helpers::{AgentFilePath, absolute_path_from_url},
    error::ApiError,
    raw::upload::required_content_length,
    services::upload::{AgentUpload, UploadStart},
    state::ServerState,
};

/// Route: `PUT /api/v1/agents/{agent}/edit/{*path}`.
pub(crate) async fn file_edit_handler(
    Path(AgentFilePath { agent, path }): Path<AgentFilePath>,
    AxumState(state): AxumState<ServerState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<FileEditResponse>, ApiError> {
    let path = absolute_path_from_url(path.unwrap_or_default());
    let total_bytes = required_content_length(&headers, "file edits")?;
    let upload = AgentUpload::start(
        &state.router_ref,
        AgentId::from(agent),
        Command::EditFile(request::EditFile { path: path.clone() }),
        path.clone(),
        total_bytes,
    )
    .await?;
    let UploadStart::Ready(mut upload) = upload else {
        return Ok(Json(FileEditResponse {
            path,
            bytes_written: 0,
        }));
    };
    upload.forward_body(body).await?;
    // Completion is accepted only as an EditFile result, so a protocol mismatch cannot report
    // editor success.
    let bytes_written = upload.finish().await?;
    Ok(Json(FileEditResponse {
        path,
        bytes_written,
    }))
}
