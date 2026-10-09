use axum::{
    Json,
    extract::{Path, State as AxumState},
};
use redoor_protocol::{
    api::files::{ChmodPathRequest, ChmodPathResponse},
    ids::AgentId,
};

use crate::http::{
    agent_client::ChmodPath,
    agent_helpers::{AgentFilePath, absolute_path_from_url},
    error::ApiError,
    state::ServerState,
};

/// Route: `POST /api/v1/agents/{agent}/chmod/{*path}` replaces ordinary rwx bits only.
pub(crate) async fn chmod_path_handler(
    Path(AgentFilePath { agent, path }): Path<AgentFilePath>,
    AxumState(state): AxumState<ServerState>,
    Json(request): Json<ChmodPathRequest>,
) -> Result<Json<ChmodPathResponse>, ApiError> {
    if request.permissions > 0o777 {
        return Err(ApiError::bad_request(format!(
            "Permissions must be between 0 and 0o777, got {:#o}",
            request.permissions
        )));
    }
    let call = ChmodPath {
        path: absolute_path_from_url(path.unwrap_or_default()),
        permissions: request.permissions,
    };
    Ok(Json(state.agents.call(&AgentId::from(agent), call).await?))
}
