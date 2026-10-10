//! REST endpoint for bounded content search below an agent directory.

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
use redoor_protocol::{
    api::search::{ContentGrepResponse, GrepRequest, MAX_GREP_CONTEXT_LINES},
    ids::AgentId,
};

/// Route: `POST /api/v1/agents/{agent}/grep/{*path}` searches below one filesystem root.
pub(crate) async fn content_grep_handler(
    Path(AgentFilePath { agent, path }): Path<AgentFilePath>,
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
    let context_requested = search.before_context > 0 || search.after_context > 0;
    let call = content_grep(absolute_path_from_url(path.unwrap_or_default()), search);
    let result = state.agents.call(&AgentId::from(agent), call).await?;
    if context_requested && !result.context_supported {
        return Err(ApiError::conflict(
            "The connected agent does not support content grep context; upgrade the agent",
        ));
    }
    Ok(Json(result))
}
