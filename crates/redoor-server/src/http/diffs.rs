use crate::actors::router::OutputStreamTracking;
use axum::{Json, extract::State as AxumState};
use redoor_protocol::{
    api::{
        diffs::{DiffEndpoint, DiffFilesRequest, DiffFilesResponse},
        files::MetadataResponse,
    },
    wire::command::Command,
};

use super::{
    agent_client::Metadata, agent_helpers::require_absolute_path, error::ApiError,
    services::output::OutputStream, state::ServerState,
};

/// Route: `POST /api/v1/diff`
pub(crate) async fn diff_files_handler(
    AxumState(state): AxumState<ServerState>,
    Json(payload): Json<DiffFilesRequest>,
) -> Result<Json<DiffFilesResponse>, ApiError> {
    let left = DiffEndpoint {
        path: require_absolute_path(payload.left.path)?,
        agent: payload.left.agent,
    };
    let right = DiffEndpoint {
        path: require_absolute_path(payload.right.path)?,
        agent: payload.right.agent,
    };

    let (left_metadata, right_metadata) = tokio::join!(
        fetch_metadata(&state, &left),
        fetch_metadata(&state, &right)
    );
    let (left_metadata, right_metadata) = (left_metadata?, right_metadata?);
    if !left_metadata.editable || !right_metadata.editable {
        return Err(ApiError::bad_request("Diff supports editable files only"));
    }

    let (left_content, right_content) = tokio::join!(
        download_editable_file(&state, &left, left_metadata.file_size),
        download_editable_file(&state, &right, right_metadata.file_size)
    );
    let (left_content, right_content) = (left_content?, right_content?);
    let unified_diff = similar::TextDiff::from_lines(&left_content, &right_content)
        .unified_diff()
        .header(&left.path, &right.path)
        .to_string();
    Ok(Json(DiffFilesResponse { unified_diff }))
}

/// Reuses agent metadata so diff eligibility cannot drift from the file editor's gate.
async fn fetch_metadata(
    state: &ServerState,
    endpoint: &DiffEndpoint,
) -> Result<MetadataResponse, ApiError> {
    state
        .agents
        .call(
            &endpoint.agent,
            Metadata {
                path: endpoint.path.clone(),
            },
        )
        .await
}

/// Downloads an already size-bounded editable file through the normal agent stream path.
async fn download_editable_file(
    state: &ServerState,
    endpoint: &DiffEndpoint,
    expected_size: u64,
) -> Result<String, ApiError> {
    let capacity = usize::try_from(expected_size)
        .map_err(|_| ApiError::bad_request("Editable file size is unsupported"))?;
    let mut stream = OutputStream::open(
        &state.router_ref,
        endpoint.agent.clone(),
        Command::RawDownload {
            path: endpoint.path.clone(),
            range_start: None,
            range_end: None,
        },
        OutputStreamTracking::Download {
            path: endpoint.path.clone(),
            total_bytes: expected_size,
            full_size: Some(expected_size),
            resume_offset: None,
        },
    )
    .await?;
    let read = stream.read_bounded(capacity).await.map_err(|message| {
        if message.is_empty() {
            ApiError::bad_request(format!("Failed to download {}", endpoint.path))
        } else {
            ApiError::bad_request(message)
        }
    })?;
    // A grown, shrunk or truncated file no longer matches the metadata the diff was gated on.
    if read.overflowed || !read.completed || read.data.len() != capacity {
        return Err(ApiError::bad_request(
            "File changed while it was being prepared for diff",
        ));
    }
    String::from_utf8(read.data).map_err(|_| {
        ApiError::bad_request("File is no longer editable because it is not valid UTF-8")
    })
}
