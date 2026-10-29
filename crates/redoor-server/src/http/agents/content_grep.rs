//! REST endpoint for bounded content search below an agent directory.

use crate::http::access::ResolvedDevice;
use crate::http::{
    agent_client::content_grep,
    agent_helpers::{AgentFilePath, absolute_path_from_url},
    error::ApiError,
    state::ServerState,
};
use axum::{
    Json,
    extract::{Path, State as AxumState},
};
use redoor_protocol::api::search::{ContentGrepResponse, GrepRequest, MAX_GREP_CONTEXT_LINES};

/// Route: `POST /api/v1/agents/{agent}/grep/{*path}` searches below one filesystem root.
pub(crate) async fn content_grep_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    Path(AgentFilePath { path }): Path<AgentFilePath>,
    AxumState(state): AxumState<ServerState>,
    Json(search): Json<GrepRequest>,
) -> Result<Json<ContentGrepResponse>, ApiError> {
    if !(1..=60).contains(&search.timeout) {
        return Err(ApiError::bad_request(
            "Content grep timeout must be between 1 and 60 seconds",
        ));
    }
    if search.before_context > MAX_GREP_CONTEXT_LINES
        || search.after_context > MAX_GREP_CONTEXT_LINES
    {
        return Err(ApiError::bad_request(format!(
            "Content grep context must be between 0 and {MAX_GREP_CONTEXT_LINES} lines per direction"
        )));
    }
    let call = content_grep(absolute_path_from_url(path.unwrap_or_default()), search);
    Ok(Json(state.agents.call(&agent_id, call).await?))
}
