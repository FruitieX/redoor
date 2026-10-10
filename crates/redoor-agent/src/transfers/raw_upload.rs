//! Raw file uploads staged in a hidden sibling file and published through the router gate.

use super::upload::{PublicationGate, SinkError, UploadSink};
use crate::errors::AgentCommandError;
use crate::ownership::{OwnershipPlan, ResolvedOwnership};
use redoor_core::destination::{check_existing_destination, place_temp_at_destination};
use redoor_core::log;
use redoor_core::staging::{StagingKind, StagingPath};
use redoor_protocol::{
    api::{files::CreationOwnershipOptions, transfers::CopyExistingMode},
    ids::RequestId,
    wire::{command::CommandErrorKind, log::Level},
};
use std::path::Path;
use tokio::{
    fs::{File, OpenOptions},
    io::AsyncWriteExt,
};
use tokio_util::sync::CancellationToken;

/// Captures metadata only from a regular destination entry and never from a symlink target.
async fn existing_destination_metadata(
    path: &Path,
) -> std::io::Result<Option<(std::fs::Permissions, u32, u32)>> {
    use std::os::unix::fs::MetadataExt;

    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_file() => Ok(Some((
            metadata.permissions(),
            metadata.uid(),
            metadata.gid(),
        ))),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Reserves a new sibling staging file without following a colliding symlink: `create_new`
/// refuses any existing entry, so ownership only ever covers a file this upload created.
async fn create_staging_file(path: &Path) -> std::io::Result<(StagingPath, File)> {
    StagingPath::create_sibling(path, StagingKind::Upload, |candidate| async move {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(candidate)
            .await
    })
    .await
}

/// Replaces or creates one file through a hidden sibling staging file.
pub(crate) struct RawFileSink {
    path: String,
    request_id: RequestId,
    on_existing: CopyExistingMode,
    staging: StagingPath,
    file: File,
    existing_permissions: Option<std::fs::Permissions>,
    ownership: ResolvedOwnership,
}

impl RawFileSink {
    /// Validates the destination and reserves staging before the server starts streaming.
    pub(crate) async fn prepare(
        request_id: RequestId,
        path: String,
        on_existing: CopyExistingMode,
        ownership: CreationOwnershipOptions,
    ) -> Result<Self, AgentCommandError> {
        let destination = Path::new(&path);
        check_existing_destination(destination, on_existing, false)
            .await
            .map_err(|error| AgentCommandError::new(error.kind(), error.to_string()))?;
        let existing_metadata =
            existing_destination_metadata(destination)
                .await
                .map_err(|error| {
                    AgentCommandError::new(
                        CommandErrorKind::from_io_error(&error),
                        format!("Failed to inspect upload destination: {error}"),
                    )
                })?;
        let existing_ids = existing_metadata.as_ref().map(|(_, uid, gid)| (*uid, *gid));
        let ownership = OwnershipPlan::resolve(ownership, existing_ids)
            .await
            .map_err(|error| AgentCommandError::new(error.kind(), error.to_string()))?;
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        let ownership = ownership
            .for_parent(parent)
            .await
            .map_err(|error| AgentCommandError::new(error.kind(), error.to_string()))?;
        let (staging, file) = create_staging_file(destination).await.map_err(|error| {
            AgentCommandError::new(
                CommandErrorKind::from_io_error(&error),
                format!("Failed to create file: {error}"),
            )
        })?;
        log!(
            Level::Info,
            "Started raw upload: request_id={}, path={}, temp_path={}, on_existing={:?}",
            request_id,
            path,
            staging.path().display(),
            on_existing
        );
        Ok(Self {
            path,
            request_id,
            on_existing,
            staging,
            file,
            existing_permissions: existing_metadata.map(|(permissions, _, _)| permissions),
            ownership,
        })
    }

    /// Logs a publication-phase failure and discards staging before it is reported.
    async fn fail(self, kind: CommandErrorKind, message: String) -> AgentCommandError {
        redoor_core::log_failure!(
            Level::Error,
            "Upload finalize failed: request_id={}, path={}, temp_path={}, error={}",
            self.request_id,
            self.path,
            self.staging.path().display(),
            message
        );
        self.staging.discard().await;
        AgentCommandError::new(kind, message)
    }
}

impl UploadSink for RawFileSink {
    /// Appends chunk bytes to the staging file.
    async fn write(&mut self, data: Vec<u8>, _cancel: &CancellationToken) -> Result<(), SinkError> {
        self.file.write_all(&data).await.map_err(|error| {
            let message = format!("Failed to write upload chunk: {error}");
            redoor_core::log_failure!(
                Level::Error,
                "Upload write failed: request_id={}, path={}, error={}",
                self.request_id,
                self.path,
                message
            );
            SinkError::Failed(AgentCommandError::new(
                CommandErrorKind::from_io_error(&error),
                message,
            ))
        })
    }

    /// Applies ownership and permissions to staging, waits for router authorization, then places.
    async fn commit(mut self, gate: PublicationGate<'_>) -> Result<(), AgentCommandError> {
        if let Err(error) = self.file.flush().await {
            let kind = CommandErrorKind::from_io_error(&error);
            return Err(self
                .fail(kind, format!("Failed to flush uploaded file: {error}"))
                .await);
        }
        if let Err(error) = self.ownership.apply(self.staging.path()).await {
            let kind = error.kind();
            return Err(self
                .fail(
                    kind,
                    format!("Failed to set uploaded file ownership: {error}"),
                )
                .await);
        }
        if let Some(permissions) = self.existing_permissions.clone()
            && let Err(error) = tokio::fs::set_permissions(self.staging.path(), permissions).await
        {
            let kind = CommandErrorKind::from_io_error(&error);
            return Err(self
                .fail(
                    kind,
                    format!("Failed to preserve uploaded file permissions: {error}"),
                )
                .await);
        }
        if !gate.authorize().await {
            self.staging.discard().await;
            return Err(AgentCommandError::new(
                CommandErrorKind::InvalidInput,
                "Upload publication was not authorized",
            ));
        }
        if let Err(error) = place_temp_at_destination(
            self.staging.path(),
            Path::new(&self.path),
            self.on_existing,
            false,
        )
        .await
        {
            let kind = error.kind();
            return Err(self
                .fail(kind, format!("Failed to finalize uploaded file: {error}"))
                .await);
        }
        self.staging.keep();
        Ok(())
    }

    /// Closes and removes the staging file.
    async fn abort(self) {
        drop(self.file);
        self.staging.discard().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::ctx::CommandCtx;
    use crate::runtime::cancel::CancelRegistry;
    use crate::runtime::responder::ResultLedger;
    use crate::transfers::upload::UploadDriver;
    use redoor_core::test_support::TempDir;
    use redoor_protocol::wire::request::CommandError;
    use redoor_protocol::wire::{AgentToServer, policy::UploadKind, stream::StreamChunk};
    use std::os::unix::fs::{PermissionsExt, symlink};

    /// Waits for the real worker's publication request so cancellation cannot depend on IO timing.
    #[tokio::test]
    async fn cancellation_at_raw_publication_gate_preserves_destination_and_cleans_staging() {
        let temp = TempDir::create();
        let destination = temp.path().join("destination");
        tokio::fs::write(&destination, b"original").await.unwrap();
        let request_id = RequestId::new(3);
        let (mut ctx, mut frames) = CommandCtx::detached(request_id);
        let registry = CancelRegistry::new(None);
        let registration = registry.register(request_id).unwrap();
        ctx.cancel = registration.token().clone();
        ctx.registration = Some(registration);
        let routes = ctx.services.upload_routes.clone();
        let prepared = RawFileSink::prepare(
            request_id,
            destination.display().to_string(),
            CopyExistingMode::Override,
            CreationOwnershipOptions::default(),
        )
        .await;
        let task = tokio::spawn(
            UploadDriver::new(
                UploadKind::RawUpload,
                destination.display().to_string(),
                ctx,
            )
            .run(prepared),
        );
        // Readiness precedes any chunk.
        assert!(matches!(
            frames.recv().await.unwrap().as_message(),
            Some(AgentToServer::Credit { .. })
        ));
        let (control, _control_frames) = tokio::sync::mpsc::channel(1);
        routes
            .route_chunk(
                StreamChunk::data(request_id, b"replacement".to_vec(), true),
                &control,
                &ResultLedger::default(),
            )
            .await;
        // Raw finalization must ask the router before changing the destination.
        assert!(matches!(
            frames.recv().await.unwrap().as_message(),
            Some(AgentToServer::PublishRequest { .. })
        ));

        // The post-body worker must retain cancellation ownership.
        assert!(registry.cancel(&request_id));
        let result = task.await.unwrap();

        // Canceling a publication wait must produce failure rather than success.
        assert!(matches!(result, Err(CommandError { .. })));
        // Post-body cancellation must never overwrite the existing file.
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), b"original");
        let mut entries = tokio::fs::read_dir(temp.path()).await.unwrap();
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name());
        }
        // Canceled raw finalization must remove all owned staging siblings.
        assert_eq!(names, vec![destination.file_name().unwrap().to_os_string()]);
    }

    #[tokio::test]
    async fn permission_capture_uses_destination_entry_without_following_symlinks() {
        let temp = TempDir::create();
        let target = temp.path().join("target");
        let peer = temp.path().join("peer");
        let valid_link = temp.path().join("valid-link");
        let dangling_link = temp.path().join("dangling-link");
        tokio::fs::write(&target, b"target").await.unwrap();
        tokio::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o751))
            .await
            .unwrap();
        tokio::fs::hard_link(&target, &peer).await.unwrap();
        symlink(&target, &valid_link).unwrap();
        symlink("missing", &dangling_link).unwrap();

        let regular = existing_destination_metadata(&target)
            .await
            .unwrap()
            .unwrap()
            .0;
        let hard_link = existing_destination_metadata(&peer)
            .await
            .unwrap()
            .unwrap()
            .0;

        // A regular entry keeps its own previous mode for replacement publication.
        assert_eq!(regular.mode() & 0o777, 0o751);
        // A hard-link name directly identifies a regular entry and supplies the same mode.
        assert_eq!(hard_link.mode() & 0o777, 0o751);
        // A valid symlink must not leak its target mode into the replacement file.
        assert!(
            existing_destination_metadata(&valid_link)
                .await
                .unwrap()
                .is_none()
        );
        // A dangling symlink is still an entry and must likewise provide no target metadata.
        assert!(
            existing_destination_metadata(&dangling_link)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn staging_creation_reserves_a_new_regular_sibling() {
        let temp = TempDir::create();
        let destination = temp.path().join("destination");

        let (staging, file) = create_staging_file(&destination).await.unwrap();
        drop(file);

        // The reserved staging entry must be a newly created regular file.
        assert!(
            tokio::fs::symlink_metadata(staging.path())
                .await
                .unwrap()
                .is_file()
        );
        staging.discard().await;
    }
}
