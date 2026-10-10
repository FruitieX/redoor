//! Starting copies and moves between agent paths, shared by REST copy, REST move and MCP.

use crate::actors::router::{
    CopyContentKind, CopyOperation, RouterHandle, RouterMsg, StartCopyRequest,
};
use crate::http::{
    agent_client::{AgentClient, Metadata, MoveMetadata},
    agent_helpers::require_absolute_path,
    error::ApiError,
};
use redoor_core::log;
use redoor_protocol::{
    api::transfers::{CopyEndpoint, CopyExistingMode, TransferProgressEntry},
    ids::TransferId,
    wire::{command::MoveSourceIdentity, log::Level},
};
use tokio::sync::watch;

/// One requested copy or move.
pub(crate) struct CopySpec {
    /// Copy keeps the source; move deletes it after the destination is published.
    pub(crate) op: CopyOperation,
    pub(crate) source: CopyEndpoint,
    pub(crate) dest: CopyEndpoint,
    pub(crate) on_existing: CopyExistingMode,
    /// Whether the caller needs the terminal outcome after public history may be pruned (MCP
    /// operation records); REST clients poll the progress API instead.
    pub(crate) observe_terminal: bool,
}

/// An admitted copy or move.
pub(crate) struct StartedCopy {
    /// Public id used by progress listings and cancellation.
    pub(crate) transfer_id: TransferId,
    /// Terminal outcome, present when the spec asked to observe it.
    pub(crate) terminal: Option<watch::Receiver<Option<TransferProgressEntry>>>,
}

/// What the source is, as far as choosing a copy mode is concerned.
struct SourceShape {
    total_bytes: u64,
    content_kind: CopyContentKind,
    identity: Option<MoveSourceIdentity>,
}

/// Admits copies and moves; the router runs them.
#[derive(Clone, Debug)]
pub(crate) struct TransferService {
    agents: AgentClient,
    router: RouterHandle,
}

impl TransferService {
    /// Creates the service on the shared router.
    pub(crate) fn new(router: RouterHandle) -> Self {
        Self {
            agents: AgentClient::new(router.clone()),
            router,
        }
    }

    /// Validates the endpoints, inspects the source and starts the transfer.
    ///
    /// Moves read the source's filesystem identity so the router deletes only the object it
    /// copied, never a replacement created meanwhile.
    pub(crate) async fn start_copy(&self, spec: CopySpec) -> Result<StartedCopy, ApiError> {
        let source_path = require_absolute_path(spec.source.path)?;
        let dest_path = require_absolute_path(spec.dest.path)?;
        if spec.source.agent == spec.dest.agent && source_path == dest_path {
            return Err(ApiError::bad_request(
                "Source and destination must be different",
            ));
        }
        let source = self
            .inspect_source(spec.op, &spec.source.agent, &source_path)
            .await?;
        let (terminal, terminal_receiver) = if spec.observe_terminal {
            let (sender, receiver) = watch::channel(None);
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let transfer_id = self
            .router
            .request(|reply| {
                RouterMsg::StartCopyRest(StartCopyRequest {
                    source_agent_id: spec.source.agent.clone(),
                    source_path: source_path.clone(),
                    dest_agent_id: spec.dest.agent.clone(),
                    dest_path: dest_path.clone(),
                    total_bytes: source.total_bytes,
                    content_kind: source.content_kind,
                    on_existing: spec.on_existing,
                    operation: spec.op,
                    source_identity: source.identity,
                    terminal,
                    reply,
                })
            })
            .await??;
        log!(
            Level::Debug,
            "Transfer started: transfer_id={}, operation={}, source_agent={}, source_path={}, dest_agent={}, dest_path={}, total_bytes={}",
            transfer_id,
            spec.op.label(),
            spec.source.agent,
            source_path,
            spec.dest.agent,
            dest_path,
            source.total_bytes
        );
        Ok(StartedCopy {
            transfer_id,
            terminal: terminal_receiver,
        })
    }

    /// Reads the source metadata the operation needs and picks raw-file or archive transport.
    async fn inspect_source(
        &self,
        op: CopyOperation,
        agent_id: &redoor_protocol::ids::AgentId,
        path: &str,
    ) -> Result<SourceShape, ApiError> {
        let path = path.to_string();
        let (file_size, is_file, is_dir, identity) = match op {
            CopyOperation::Copy => {
                let metadata = self.agents.call(agent_id, Metadata { path }).await?;
                (metadata.file_size, metadata.is_file, metadata.is_dir, None)
            }
            CopyOperation::Move => {
                let metadata = self.agents.call(agent_id, MoveMetadata { path }).await?;
                (
                    metadata.file_size,
                    metadata.is_file,
                    metadata.is_dir,
                    Some(metadata.identity),
                )
            }
        };
        let (total_bytes, content_kind) = if is_file {
            (file_size, CopyContentKind::RawFile)
        } else if is_dir {
            // Archive size is unknown until the agent walks the tree.
            (0, CopyContentKind::TarDirectory)
        } else {
            return Err(ApiError::bad_request(format!(
                "{} supports regular files and directories only",
                op.title()
            )));
        };
        Ok(SourceShape {
            total_bytes,
            content_kind,
            identity,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake_agent::FakeAgent;
    use axum::http::StatusCode;
    use redoor_protocol::wire::request;
    use redoor_protocol::{
        api::files::MetadataResponse,
        wire::command::{Command, MoveMetadataResult},
    };

    /// A spec between two paths of the fake agent.
    fn spec(agent: &FakeAgent, op: CopyOperation, source: &str, dest: &str) -> CopySpec {
        CopySpec {
            op,
            source: CopyEndpoint {
                agent: agent.agent_id.clone(),
                path: source.to_string(),
            },
            dest: CopyEndpoint {
                agent: agent.agent_id.clone(),
                path: dest.to_string(),
            },
            on_existing: CopyExistingMode::Error,
            observe_terminal: true,
        }
    }

    /// Metadata of a regular or special file.
    fn metadata(is_file: bool) -> MetadataResponse {
        MetadataResponse {
            path: "/src".to_string(),
            mime_type: "text/plain".to_string(),
            file_size: 5,
            is_file,
            is_dir: false,
            editable: true,
            viewable_image: false,
            one_time_tokens: Vec::new(),
        }
    }

    /// Invalid endpoints are rejected before any agent work.
    #[tokio::test]
    async fn rejects_identical_and_relative_endpoints() {
        let mut agent = FakeAgent::connect().await;
        let service = TransferService::new(agent.router.clone());
        let same = service
            .start_copy(spec(&agent, CopyOperation::Copy, "/a", "/a"))
            .await
            .err()
            .unwrap();
        // Copying a path onto itself is a client error for REST and MCP alike.
        assert_eq!(same.status_code(), StatusCode::BAD_REQUEST);
        let relative = service
            .start_copy(spec(&agent, CopyOperation::Move, "a", "/b"))
            .await
            .err()
            .unwrap();
        // Relative paths would depend on the agent's working directory.
        assert_eq!(relative.to_string(), "Filesystem path must be absolute");
        // Neither request reached the agent.
        assert!(agent.commands.try_recv().is_err());
    }

    /// Special files are refused with an operation-specific message.
    #[tokio::test]
    async fn rejects_sources_that_are_neither_files_nor_directories() {
        let mut agent = FakeAgent::connect().await;
        let service = TransferService::new(agent.router.clone());
        let started = tokio::spawn({
            let spec = spec(&agent, CopyOperation::Copy, "/dev/null", "/b");
            async move { service.start_copy(spec).await }
        });
        let (request_id, command) = agent.next_command().await;
        // Copies only need plain metadata.
        assert!(matches!(
            command,
            Command::Metadata(request::Metadata { .. })
        ));
        agent
            .respond_ok::<request::Metadata>(request_id, metadata(false))
            .await;
        let error = started.await.unwrap().err().unwrap();
        assert_eq!(
            error.to_string(),
            "Copy supports regular files and directories only"
        );
    }

    /// A move captures the source identity so only the copied object is deleted later.
    #[tokio::test]
    async fn move_forwards_the_captured_source_identity() {
        let mut agent = FakeAgent::connect().await;
        let service = TransferService::new(agent.router.clone());
        let started = tokio::spawn({
            let spec = spec(&agent, CopyOperation::Move, "/src", "/dst");
            async move { service.start_copy(spec).await }
        });
        let (request_id, command) = agent.next_command().await;
        // Moves read the filesystem identity, not just metadata.
        assert!(matches!(
            command,
            Command::MoveMetadata(request::MoveMetadata { .. })
        ));
        let identity = redoor_protocol::wire::command::MoveSourceIdentity {
            device: 1,
            inode: 2,
            size: 5,
            modified_seconds: 3,
            modified_nanoseconds: 4,
            is_directory: false,
        };
        agent
            .respond_ok::<request::MoveMetadata>(
                request_id,
                MoveMetadataResult {
                    file_size: 5,
                    is_file: true,
                    is_dir: false,
                    identity: identity.clone(),
                },
            )
            .await;
        let started = started.await.unwrap().unwrap();
        // Observers get the terminal lane they asked for.
        assert!(started.terminal.is_some());
        let (_, command) = agent.next_command().await;
        // The same-agent move carries exactly the identity read before copying.
        assert!(matches!(
            command,
            Command::LocalMove(request::LocalMove { expected_identity, .. }) if expected_identity == identity
        ));
    }
}
