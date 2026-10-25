//! REST endpoints that read and change server and agent log levels at runtime.

use crate::http::access::ResolvedDevice;
use axum::{Json, extract::State as AxumState};
use redoor_core::logging;
use redoor_protocol::api::logging::{LoggingLevelRequest, LoggingLevelResponse};

use super::{
    agent_client::{GetLoggingLevel, SetLoggingLevel},
    error::ApiError,
    state::ServerState,
};

/// Returns the threshold used by all subsequent server log macro calls.
pub(crate) async fn get_server_logging_level_handler() -> Json<LoggingLevelResponse> {
    Json(LoggingLevelResponse {
        level: logging::level(),
    })
}

/// Changes only process-lifetime server state so startup configuration remains untouched.
pub(crate) async fn update_server_logging_level_handler(
    Json(request): Json<LoggingLevelRequest>,
) -> Json<LoggingLevelResponse> {
    logging::set_level(request.level);
    Json(LoggingLevelResponse {
        level: request.level,
    })
}

/// Reads the threshold over the connected agent's authoritative command socket.
pub(crate) async fn get_agent_logging_level_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<LoggingLevelResponse>, ApiError> {
    let level = state.agents.call(&agent_id, GetLoggingLevel {}).await?;
    Ok(Json(LoggingLevelResponse { level }))
}

/// Updates a connected agent without coupling the setting to its ephemeral log-stream socket.
pub(crate) async fn update_agent_logging_level_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    AxumState(state): AxumState<ServerState>,
    Json(request): Json<LoggingLevelRequest>,
) -> Result<Json<LoggingLevelResponse>, ApiError> {
    let call = SetLoggingLevel {
        level: request.level,
    };
    let level = state.agents.call(&agent_id, call).await?;
    Ok(Json(LoggingLevelResponse { level }))
}
