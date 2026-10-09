use crate::actors;
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
    state::ServerState, streaming::OutputCancelGuard,
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
    let mut chunk_receiver = state
        .router_ref
        .request(|reply| {
            actors::router::RouterMsg::ExecuteStreamCommandRest(
                actors::router::ExecuteStreamRequest {
                    agent_id: endpoint.agent.clone(),
                    command: Command::RawDownload {
                        path: endpoint.path.clone(),
                        range_start: None,
                        range_end: None,
                    },
                    tracking: actors::router::OutputStreamTracking::Download {
                        path: endpoint.path.clone(),
                        total_bytes: expected_size,
                        full_size: Some(expected_size),
                        resume_offset: None,
                    },
                    reply,
                },
            )
        })
        .await??;
    let mut cancel_guard = OutputCancelGuard::new(
        state.router_ref.clone(),
        endpoint.agent.clone(),
        chunk_receiver.request_id(),
    );

    let capacity = usize::try_from(expected_size)
        .map_err(|_| ApiError::bad_request("Editable file size is unsupported"))?;
    let changed = || ApiError::bad_request("File changed while it was being prepared for diff");
    let mut content = Vec::with_capacity(capacity);
    let mut completed = false;
    while let Some(chunk) = chunk_receiver.recv().await {
        if chunk.is_error {
            cancel_guard.disarm();
            let message = if chunk.data.is_empty() {
                format!("Failed to download {}", endpoint.path)
            } else {
                String::from_utf8_lossy(&chunk.data).into_owned()
            };
            return Err(ApiError::bad_request(message));
        }
        if content.len().saturating_add(chunk.data.len()) > capacity {
            return Err(changed());
        }
        content.extend_from_slice(&chunk.data);
        if chunk.is_last {
            cancel_guard.disarm();
            completed = true;
            break;
        }
    }
    if !completed || content.len() != capacity {
        return Err(changed());
    }
    String::from_utf8(content).map_err(|_| {
        ApiError::bad_request("File is no longer editable because it is not valid UTF-8")
    })
}
