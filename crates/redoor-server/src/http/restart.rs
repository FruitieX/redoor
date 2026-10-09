use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use redoor_protocol::api::{ErrorResponse, server::RestartResponse};

use super::state::ServerState;
use redoor_core::config::{parse_config_file, require_server_section};

/// Validates config.toml then asks the process to restart so startup
/// applies the file from scratch (agents, auth, bind/port/log).
///
/// Parse errors are rejected with 400 before any shutdown so a bad edit
/// cannot take the running process down. Concurrent restarts get 409 once
/// the oneshot sender has already been taken.
pub(crate) async fn restart_server_handler(State(state): State<ServerState>) -> impl IntoResponse {
    let validated = async {
        let config = parse_config_file(&state.config_path).await?;
        require_server_section(&config)?;
        Ok::<(), redoor_core::config::ConfigError>(())
    }
    .await;
    if let Err(error) = validated {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("Invalid config, not restarting: {error}"),
            }),
        )
            .into_response();
    }

    let mut guard = state.shutdown_tx.lock().await;
    let Some(tx) = guard.take() else {
        return (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "Restart already in progress".to_string(),
            }),
        )
            .into_response();
    };

    // Drop the lock before signaling so we do not hold it across restart.
    drop(guard);
    let _ = tx.send(());

    Json(RestartResponse { restarting: true }).into_response()
}
