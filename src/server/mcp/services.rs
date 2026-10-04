//! Transport-neutral device services used by MCP and suitable for REST handler reuse.

use redoor::{
    actors::{
        self,
        router::{ExecuteCommandRequest, ExecuteStreamRequest, OutputStreamTracking, RouterMsg},
    },
    commands::{Command, CommandResult, CopyExistingMode, CreationOwnershipOptions},
    types::{AgentId, RequestId},
};

use crate::server::{
    agents,
    raw::{AgentUpload, AgentUploadStartError},
    state::ServerState,
};

/// Keeps inline MCP reads small; larger files use ranges or the streaming endpoint.
pub(super) const INLINE_FILE_LIMIT: usize = 64 * 1024;
/// Executes one ordinary agent command through the same router path as REST.
pub(super) async fn command(
    state: &ServerState,
    device_id: &str,
    command: Command,
    timeout_ms: u64,
) -> Result<CommandResult, String> {
    state
        .router_ref
        .request(timeout_ms, |reply| {
            RouterMsg::ExecuteCommandRest(ExecuteCommandRequest {
                agent_id: AgentId::from(device_id),
                command,
                reply,
            })
        })
        .await
        .map_err(|error| format!("Agent request failed: {error:?}"))
}

/// Returns retained inventory for policy-filtering without exposing connection secrets.
pub(super) async fn inventory(
    state: &ServerState,
) -> Result<Vec<redoor::commands::AgentInfoResponse>, String> {
    agents::list_agent_snapshots(state).await
}

/// Downloads one bounded range while preserving router backpressure and cancellation.
pub(super) async fn read_file(
    state: &ServerState,
    device_id: &str,
    path: &str,
    offset: u64,
    limit: usize,
) -> Result<(Vec<u8>, bool), String> {
    let limit = limit.min(INLINE_FILE_LIMIT);
    let end = offset.saturating_add(limit as u64);
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let (cancel_sender, _cancel_receiver) = tokio::sync::watch::channel(false);
    let agent_id = AgentId::from(device_id);
    let request_id = state
        .router_ref
        .request(30_000, |reply| {
            RouterMsg::ExecuteStreamCommandRest(ExecuteStreamRequest {
                agent_id: agent_id.clone(),
                command: Command::RawDownload {
                    path: path.to_string(),
                    range_start: Some(offset),
                    range_end: Some(end.saturating_add(1)),
                },
                tracking: OutputStreamTracking::Download {
                    path: path.to_string(),
                    total_bytes: limit as u64 + 1,
                    full_size: None,
                    resume_offset: Some(offset),
                },
                reply,
                chunk_sender: sender,
                rest_cancel_sender: Some(cancel_sender),
            })
        })
        .await
        .map_err(|error| format!("Read admission failed: {error:?}"))?
        .map_err(|error| format!("Read rejected: {error:?}"))?;
    let mut bytes = Vec::with_capacity(limit.min(8192));
    while let Some(chunk) = receiver.recv().await {
        if chunk.is_error {
            return Err(String::from_utf8_lossy(&chunk.data).into_owned());
        }
        let remaining = limit.saturating_add(1).saturating_sub(bytes.len());
        bytes.extend_from_slice(&chunk.data[..chunk.data.len().min(remaining)]);
        if bytes.len() > limit || chunk.is_last {
            break;
        }
    }
    if bytes.len() > limit {
        bytes.truncate(limit);
        let _ = state.router_ref.send(RouterMsg::CancelTransfer {
            agent_id,
            request_id,
        });
        Ok((bytes, true))
    } else {
        Ok((bytes, false))
    }
}

/// Writes bounded inline bytes through the existing streamed upload worker.
pub(super) async fn write_file(
    state: &ServerState,
    device_id: &str,
    path: &str,
    bytes: &[u8],
    mode: CopyExistingMode,
) -> Result<u64, String> {
    let command = Command::RawUpload {
        path: path.to_string(),
        on_existing: mode,
        ownership: CreationOwnershipOptions::default(),
    };
    let mut upload = match AgentUpload::start(
        state,
        AgentId::from(device_id),
        command,
        path.to_string(),
        bytes.len() as u64,
    )
    .await
    {
        Ok(upload) => upload,
        Err(AgentUploadStartError::Response(_)) => {
            return Err("Upload admission failed".to_string());
        }
        Err(AgentUploadStartError::Finished(result)) => {
            return match *result {
                Ok(CommandResult::RawUpload) if bytes.is_empty() => Ok(0),
                Ok(CommandResult::Error { message, .. }) => Err(message),
                _ => Err("Upload ended before readiness".to_string()),
            };
        }
    };
    upload
        .send(bytes)
        .await
        .map_err(|_| "Upload body failed".to_string())?;
    match upload
        .finish()
        .await
        .map_err(|_| "Upload completion failed".to_string())?
    {
        (CommandResult::RawUpload, written) => Ok(written),
        (CommandResult::Error { message, .. }, _) => Err(message),
        _ => Err("Unexpected upload completion".to_string()),
    }
}

/// Admits non-PTY execution and returns the output and independent terminal channels.
pub(super) async fn start_execution(
    state: &ServerState,
    device_id: &str,
    request: redoor::exec_protocol::ExecRequest,
) -> Result<
    (
        RequestId,
        tokio::sync::mpsc::Receiver<redoor::streaming::StreamChunk>,
        tokio::sync::watch::Receiver<Option<redoor::exec_protocol::ExecEvent>>,
    ),
    String,
> {
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let (terminal_sender, terminal_receiver) = tokio::sync::watch::channel(None);
    let agent_id = AgentId::from(device_id);
    let id = state
        .router_ref
        .request(30_000, |reply| {
            RouterMsg::ExecuteStreamCommandRest(ExecuteStreamRequest {
                agent_id: agent_id.clone(),
                command: Command::Exec { request },
                tracking: OutputStreamTracking::Execution {
                    terminal: terminal_sender.clone(),
                },
                reply,
                chunk_sender: sender,
                rest_cancel_sender: None,
            })
        })
        .await
        .map_err(|error| format!("Execution admission failed: {error:?}"))?
        .map_err(|error| format!("Execution rejected: {error:?}"))?;
    drop(terminal_sender);
    Ok((id, receiver, terminal_receiver))
}

/// Cancels an admitted execution while leaving acknowledgement to the terminal lane.
pub(super) async fn cancel_execution(
    state: &ServerState,
    device_id: &str,
    request_id: RequestId,
) -> Result<bool, String> {
    state
        .router_ref
        .request(5_000, |reply| RouterMsg::CancelExecution {
            agent_id: AgentId::from(device_id),
            request_id,
            reply,
        })
        .await
        .map_err(|error| format!("Cancellation failed: {error:?}"))?
        .map_err(|error| format!("Cancellation delivery failed: {error:?}"))
}

/// Starts the existing same/cross-device copy pipeline and returns its public transfer ID.
pub(super) async fn start_transfer(
    state: &ServerState,
    source_device: &str,
    source_path: &str,
    destination_device: &str,
    destination_path: &str,
    mode: CopyExistingMode,
    is_move: bool,
) -> Result<
    (
        u64,
        tokio::sync::watch::Receiver<Option<redoor::commands::TransferProgressEntry>>,
    ),
    String,
> {
    let metadata_command = if is_move {
        Command::MoveMetadata {
            path: source_path.to_string(),
        }
    } else {
        Command::Metadata {
            path: source_path.to_string(),
        }
    };
    let metadata = command(state, source_device, metadata_command, 30_000).await?;
    let (size, is_file, is_dir, identity) = match metadata {
        CommandResult::Metadata(value) => (value.file_size, value.is_file, value.is_dir, None),
        CommandResult::MoveMetadata(value) => (
            value.file_size,
            value.is_file,
            value.is_dir,
            Some(value.identity),
        ),
        CommandResult::Error { message, .. } => return Err(message),
        _ => return Err("Unexpected transfer metadata response".to_string()),
    };
    let content_kind = if is_file {
        actors::router::CopyContentKind::RawFile
    } else if is_dir {
        actors::router::CopyContentKind::TarDirectory
    } else {
        return Err("Transfers support regular files and directories only".to_string());
    };
    let (terminal_sender, terminal_receiver) = tokio::sync::watch::channel(None);
    let id = state
        .router_ref
        .request(30_000, |reply| {
            RouterMsg::StartCopyRest(actors::router::StartCopyRequest {
                source_agent_id: AgentId::from(source_device),
                source_path: source_path.to_string(),
                dest_agent_id: AgentId::from(destination_device),
                dest_path: destination_path.to_string(),
                total_bytes: if is_file { size } else { 0 },
                content_kind,
                on_existing: mode,
                operation: if is_move {
                    actors::router::CopyOperation::Move
                } else {
                    actors::router::CopyOperation::Copy
                },
                source_identity: identity,
                terminal: Some(terminal_sender.clone()),
                reply,
            })
        })
        .await
        .map_err(|error| format!("Transfer admission failed: {error:?}"))?
        .map_err(|error| format!("Transfer rejected: {error:?}"))?;
    Ok((id.0, terminal_receiver))
}
