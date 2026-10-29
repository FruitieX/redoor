//! Atomic retry control for a managed agent's current startup attempt.

use crate::http::access::ResolvedDevice;
use axum::{Json, extract::State as AxumState};
use redoor_protocol::api::agents::RetryAgentStartResponse;

use super::{
    agents::{
        apply_managed_snapshot, current_agent_snapshot, evict_managed_snapshot,
        managed_agent_snapshot, managed_watchdog,
    },
    error::ApiError,
    state::ServerState,
};

/// Route: `POST /api/v1/agents/{agent}/retry-start` replaces one managed startup attempt.
pub(crate) async fn retry_agent_start_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<RetryAgentStartResponse>, ApiError> {
    // Configuration replacement and lifecycle controls must resolve against one supervisor identity.
    let _edit_guard = state.config_edit_lock.lock().await;
    let snapshot = managed_agent_snapshot(&state, &agent_id).await?;
    let watchdog = managed_watchdog(&state, &snapshot)?;

    let mut retry_snapshot = watchdog.snapshot();
    retry_snapshot.status = redoor_protocol::api::agents::AgentConnectionStatus::Starting;
    retry_snapshot.connection_issue = None;
    retry_snapshot.provisioning_status.clear();
    retry_snapshot.socket_id = None;
    // Bounded like shutdown: this handler holds the configuration lock, so a supervisor that
    // never acknowledges must not freeze every lifecycle and configuration route.
    let (retry_result, eviction_result) = tokio::join!(
        tokio::time::timeout(std::time::Duration::from_secs(10), watchdog.retry_startup()),
        evict_managed_snapshot(&state, agent_id.clone(), retry_snapshot),
    );
    retry_result.map_err(|_| {
        ApiError::status(
            axum::http::StatusCode::GATEWAY_TIMEOUT,
            format!("Timed out retrying managed agent startup: {agent_id}"),
        )
    })??;
    eviction_result?;

    apply_managed_snapshot(&state, agent_id.clone(), watchdog.snapshot()).await?;
    Ok(Json(RetryAgentStartResponse {
        agent: current_agent_snapshot(&state, &agent_id).await?,
    }))
}
