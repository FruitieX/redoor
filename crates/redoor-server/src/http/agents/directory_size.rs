//! REST endpoint that measures recursive directory size on an agent.

use crate::http::access::ResolvedDevice;
use axum::{
    Json,
    extract::{Path, State as AxumState},
};
use redoor_protocol::api::files::DirectorySizeResponse;

use crate::http::{
    agent_client::DirectorySize,
    agent_helpers::{AgentFilePath, absolute_path_from_url},
    error::ApiError,
    state::ServerState,
};

/// Agent-side walk limit; the browser only needs an approximate size quickly.
const DIRECTORY_SIZE_TIMEOUT_SECONDS: u64 = 10;

/// Route: `POST /api/v1/agents/{agent}/directory-size/{*path}` calculates recursive file bytes.
pub(crate) async fn directory_size_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    Path(AgentFilePath { path }): Path<AgentFilePath>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<DirectorySizeResponse>, ApiError> {
    let call = DirectorySize {
        path: absolute_path_from_url(path.unwrap_or_default()),
        timeout_seconds: DIRECTORY_SIZE_TIMEOUT_SECONDS,
    };
    Ok(Json(state.agents.call(&agent_id, call).await?))
}
