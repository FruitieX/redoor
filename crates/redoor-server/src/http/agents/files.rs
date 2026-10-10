//! REST endpoints for listing, path search, metadata and echo on an agent.

use crate::http::{
    agent_client::{Echo, Ls, Metadata, file_search},
    agent_helpers::{AgentFilePath, absolute_path_from_url},
    error::ApiError,
    state::ServerState,
};
use axum::{
    Json,
    extract::{Path, State as AxumState},
    response::{IntoResponse, Response},
};
use redoor_protocol::{
    api::{
        agents::{EchoRequest, EchoResponse},
        files::{LsDirectoryResponse, LsFileResponse, MetadataResponse},
        search::{FileSearchResponse, FindRequest},
    },
    ids::AgentId,
    wire::request::LsResponse,
};

/// Route: `GET /api/v1/agents/{agent}/ls/{*path}`
pub(crate) async fn ls_agent_handler(
    Path(AgentFilePath { agent, path }): Path<AgentFilePath>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Response, ApiError> {
    let call = Ls {
        path: Some(absolute_path_from_url(path.unwrap_or_default())),
    };
    Ok(
        match state.agents.call(&AgentId::from(agent), call).await? {
            LsResponse::Directory(listing) => Json(LsDirectoryResponse {
                files: listing.files,
                path: listing.path,
                owner: listing.owner,
                group: listing.group,
                uid: listing.uid,
                gid: listing.gid,
                permissions: listing.permissions,
            })
            .into_response(),
            LsResponse::File(file) => Json(LsFileResponse {
                size: file.size,
                path: file.path,
                owner: file.owner,
                group: file.group,
                uid: file.uid,
                gid: file.gid,
                permissions: file.permissions,
            })
            .into_response(),
        },
    )
}

/// Route: `POST /api/v1/agents/{agent}/find/{*path}` searches below one filesystem root.
pub(crate) async fn file_search_agent_handler(
    Path(AgentFilePath { agent, path }): Path<AgentFilePath>,
    AxumState(state): AxumState<ServerState>,
    Json(search): Json<FindRequest>,
) -> Result<Json<FileSearchResponse>, ApiError> {
    if !(1..=60).contains(&search.timeout) {
        return Err(ApiError::bad_request(
            "File search timeout must be between 1 and 60 seconds",
        ));
    }
    let call = file_search(absolute_path_from_url(path.unwrap_or_default()), search);
    Ok(Json(state.agents.call(&AgentId::from(agent), call).await?))
}

/// Route: `GET /api/v1/agents/{agent}/metadata/{*path}`
pub(crate) async fn metadata_agent_handler(
    Path(AgentFilePath { agent, path }): Path<AgentFilePath>,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<MetadataResponse>, ApiError> {
    let path = absolute_path_from_url(path.unwrap_or_default());
    let agent_id = AgentId::from(agent);
    let mut metadata = state
        .agents
        .call(&agent_id, Metadata { path: path.clone() })
        .await?;
    metadata.one_time_tokens = state.one_time_token_registry.list(&agent_id, &path);
    Ok(Json(metadata))
}

/// Route: `POST /api/v1/agents/{agent}/echo`
pub(crate) async fn echo_agent_handler(
    Path(agent): Path<String>,
    AxumState(state): AxumState<ServerState>,
    Json(payload): Json<EchoRequest>,
) -> Result<Json<EchoResponse>, ApiError> {
    let echo = state
        .agents
        .call(&AgentId::from(agent), Echo { request: payload })
        .await?;
    Ok(Json(EchoResponse {
        message: echo.message,
    }))
}
