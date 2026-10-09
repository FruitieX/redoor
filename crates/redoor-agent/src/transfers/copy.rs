//! Same-agent copies done directly on the filesystem, avoiding a round trip through the server.

use crate::errors::AgentCommandError;
use crate::runtime::responder::Responder;
use redoor_core::destination::{
    DestinationPlaceError, check_existing_destination, place_temp_at_destination,
};
use redoor_core::log;
use redoor_core::staging::{StagingKind, StagingPath};
use redoor_protocol::{
    api::transfers::CopyExistingMode,
    wire::{command::CommandResult, log::Level},
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
};
use tokio_util::sync::CancellationToken;

/// Keeps local-copy failures typed so the HTTP layer never has to infer them from text.
#[derive(Debug, Error)]
pub(crate) enum LocalCopyError {
    #[error("Failed to access source file: {0}")]
    AccessSourceFile(#[source] std::io::Error),
    #[error("Failed to access source directory: {0}")]
    AccessSourceDirectory(#[source] std::io::Error),
    #[error("Source path is not a file: {0}")]
    SourceNotFile(String),
    #[error("Source path is not a directory: {0}")]
    SourceNotDirectory(String),
    #[error("Source and destination must be different")]
    SamePath,
    #[error("Destination directory cannot be inside the source directory")]
    DestinationInsideSource,
    #[error("Destination parent not found for {0}")]
    DestinationParentNotFound(String),
    #[error("Destination parent is not a directory: {0}")]
    DestinationParentNotDirectory(String),
    #[error(transparent)]
    DestinationPlacement(#[from] DestinationPlaceError),
    #[error("Invalid source path: {0}")]
    InvalidSourcePath(String),
    #[error("Invalid destination path: {0}")]
    InvalidDestinationPath(String),
    #[error("Failed to access source parent: {0}")]
    AccessSourceParent(String),
    #[error("Failed to access destination parent: {0}")]
    AccessDestinationParent(String),
    #[error("Failed to open source file: {0}")]
    OpenSourceFile(String),
    #[error("Failed to create destination file: {0}")]
    CreateDestinationFile(String),
    #[error("Failed to read source file: {0}")]
    ReadSourceFile(String),
    #[error("Failed to write destination file: {0}")]
    WriteDestinationFile(String),
    #[error("Failed to flush destination file: {0}")]
    FlushDestinationFile(String),
    #[error("Failed to finalize copied file from {from} to {to}")]
    FinalizeCopiedFile { from: String, to: String },
    #[error("Failed to read directory: {0}")]
    ReadDirectory(String),
    #[error("Failed to read directory entry: {0}")]
    ReadDirectoryEntry(String),
    #[error("Failed to read entry metadata: {0}")]
    ReadEntryMetadata(String),
    #[error("Unsupported directory entry type in copy source: {0}")]
    UnsupportedEntryType(String),
    #[error("Failed to create destination directory: {0}")]
    CreateDestinationDirectory(String),
    #[error("Failed to create destination symbolic link: {0}")]
    CreateDestinationSymlink(String),
    #[error("Failed to create temp directory: {0}")]
    CreateTempDirectory(#[source] std::io::Error),
    #[error("Failed to create temporary symbolic link: {0}")]
    CreateTempSymlink(#[source] std::io::Error),
    #[error("Lost router connection while reporting local copy progress")]
    ProgressChannelClosed,
    #[error("Local copy canceled because its control connection ended")]
    Canceled,
}

impl LocalCopyError {
    /// Maps one local-copy failure to the stable command error kind carried over the protocol.
    pub(crate) fn kind(&self) -> redoor_protocol::wire::command::CommandErrorKind {
        match self {
            Self::AccessSourceFile(error)
            | Self::AccessSourceDirectory(error)
            | Self::CreateTempDirectory(error)
            | Self::CreateTempSymlink(error) => {
                redoor_protocol::wire::command::CommandErrorKind::from_io_error(error)
            }
            Self::DestinationPlacement(error) => error.kind(),
            Self::SourceNotDirectory(_) | Self::DestinationParentNotDirectory(_) => {
                redoor_protocol::wire::command::CommandErrorKind::NotADirectory
            }
            Self::SamePath
            | Self::DestinationInsideSource
            | Self::SourceNotFile(_)
            | Self::DestinationParentNotFound(_)
            | Self::InvalidSourcePath(_)
            | Self::InvalidDestinationPath(_)
            | Self::UnsupportedEntryType(_) => {
                redoor_protocol::wire::command::CommandErrorKind::InvalidInput
            }
            Self::AccessSourceParent(_)
            | Self::AccessDestinationParent(_)
            | Self::OpenSourceFile(_)
            | Self::CreateDestinationFile(_)
            | Self::ReadSourceFile(_)
            | Self::WriteDestinationFile(_)
            | Self::FlushDestinationFile(_)
            | Self::FinalizeCopiedFile { .. }
            | Self::ReadDirectory(_)
            | Self::ReadDirectoryEntry(_)
            | Self::ReadEntryMetadata(_)
            | Self::CreateDestinationDirectory(_)
            | Self::CreateDestinationSymlink(_)
            | Self::ProgressChannelClosed
            | Self::Canceled => redoor_protocol::wire::command::CommandErrorKind::Internal,
        }
    }
}

/// Verifies the copy destination differs from the source and does not recurse into it.
pub(crate) async fn validate_local_copy_destination(
    source_path: &Path,
    dest_path: &Path,
    source_is_dir: bool,
) -> Result<(), LocalCopyError> {
    let source_parent = source_path
        .parent()
        .ok_or_else(|| LocalCopyError::InvalidSourcePath(source_path.display().to_string()))?;
    let dest_parent = dest_path.parent().ok_or_else(|| {
        LocalCopyError::DestinationParentNotFound(dest_path.display().to_string())
    })?;

    let canonical_source_parent = tokio::fs::canonicalize(source_parent)
        .await
        .map_err(|_| LocalCopyError::AccessSourceParent(source_parent.display().to_string()))?;
    let canonical_dest_parent = tokio::fs::canonicalize(dest_parent)
        .await
        .map_err(|_| LocalCopyError::AccessDestinationParent(dest_parent.display().to_string()))?;

    // Resolve the parent but retain the selected directory entry so symlinks are not dereferenced.
    let source_canonical = canonical_source_parent.join(
        source_path
            .file_name()
            .ok_or_else(|| LocalCopyError::InvalidSourcePath(source_path.display().to_string()))?,
    );

    let dest_effective =
        canonical_dest_parent.join(dest_path.file_name().ok_or_else(|| {
            LocalCopyError::InvalidDestinationPath(dest_path.display().to_string())
        })?);

    if source_canonical == dest_effective {
        return Err(LocalCopyError::SamePath);
    }

    if source_is_dir && dest_effective.starts_with(&source_canonical) {
        return Err(LocalCopyError::DestinationInsideSource);
    }

    Ok(())
}

/// Verifies the destination parent exists and is a directory before copy work begins.
pub(crate) async fn validate_local_copy_parent(dest_path: &Path) -> Result<(), LocalCopyError> {
    let parent = dest_path.parent().ok_or_else(|| {
        LocalCopyError::DestinationParentNotFound(dest_path.display().to_string())
    })?;
    let parent_metadata = tokio::fs::metadata(parent)
        .await
        .map_err(|_| LocalCopyError::AccessDestinationParent(parent.display().to_string()))?;
    if !parent_metadata.is_dir() {
        return Err(LocalCopyError::DestinationParentNotDirectory(
            parent.display().to_string(),
        ));
    }
    Ok(())
}

/// Reports local-copy byte progress over the control lane without spamming tiny updates.
struct LocalCopyProgressReporter {
    responder: Responder,
    total_bytes: u64,
    transferred_bytes: u64,
    last_reported_bytes: u64,
    last_reported_at: Instant,
    cancel: CancellationToken,
}

impl LocalCopyProgressReporter {
    /// Byte threshold that triggers a progress update, so large copies do not flood the control lane.
    const REPORT_EVERY_BYTES: u64 = 1024 * 1024;
    /// Time threshold that triggers a progress update, so slow copies still show movement.
    const REPORT_EVERY_DURATION: Duration = Duration::from_millis(250);

    /// Starts tracking one local copy so progress is visible to the server UI.
    fn new(responder: Responder, total_bytes: u64, cancel: CancellationToken) -> Self {
        Self {
            responder,
            total_bytes,
            transferred_bytes: 0,
            last_reported_bytes: 0,
            last_reported_at: Instant::now() - Self::REPORT_EVERY_DURATION,
            cancel,
        }
    }

    /// Emits a progress update once enough bytes or time have passed.
    async fn report(&mut self, force: bool) -> Result<(), LocalCopyError> {
        ensure_active(&self.cancel)?;
        if self.responder.is_closed() {
            return Err(LocalCopyError::ProgressChannelClosed);
        }

        let now = Instant::now();
        let should_report = force
            || self
                .transferred_bytes
                .saturating_sub(self.last_reported_bytes)
                >= Self::REPORT_EVERY_BYTES
            || now.saturating_duration_since(self.last_reported_at) >= Self::REPORT_EVERY_DURATION;

        if !should_report {
            return Ok(());
        }

        tokio::select! {
            result = self.responder.progress(self.transferred_bytes, Some(self.total_bytes)) => {
                result.map_err(|_| LocalCopyError::ProgressChannelClosed)?;
            }
            _ = self.cancel.cancelled() => {
                return Err(LocalCopyError::Canceled);
            }
        }

        self.last_reported_bytes = self.transferred_bytes;
        self.last_reported_at = now;
        Ok(())
    }

    /// Adds copied bytes and reports if the throttle allows it.
    async fn advance(&mut self, bytes: u64) -> Result<(), LocalCopyError> {
        self.transferred_bytes = self.transferred_bytes.saturating_add(bytes);
        self.report(false).await
    }

    /// Expands an initially unknown directory total as traversal discovers regular files.
    fn add_total_bytes(&mut self, bytes: u64) {
        self.total_bytes = self.total_bytes.saturating_add(bytes);
    }

    /// Forces the final progress update so the UI lands on 100%.
    async fn finish(&mut self) -> Result<(), LocalCopyError> {
        self.transferred_bytes = self.total_bytes;
        self.report(true).await
    }
}

/// Rejects more work after cancellation without dropping an in-flight filesystem future.
pub(super) fn ensure_active(cancel: &CancellationToken) -> Result<(), LocalCopyError> {
    if cancel.is_cancelled() {
        Err(LocalCopyError::Canceled)
    } else {
        Ok(())
    }
}

/// Copies one file through a temp file so partially copied output is never exposed as final data.
async fn copy_file_streaming(
    source_path: &Path,
    dest_path: &Path,
    temp_path: &Path,
    reporter: &mut LocalCopyProgressReporter,
) -> Result<(), LocalCopyError> {
    // The temp path lies inside this copy's own fresh staging directory, so plain creation
    // cannot claim anyone else's entry.
    let destination = File::create(temp_path)
        .await
        .map_err(|_| LocalCopyError::CreateDestinationFile(temp_path.display().to_string()))?;
    ensure_active(&reporter.cancel)?;
    stream_file_to_temp(source_path, destination, temp_path, reporter).await?;
    ensure_active(&reporter.cancel)?;

    tokio::fs::rename(temp_path, dest_path).await.map_err(|_| {
        LocalCopyError::FinalizeCopiedFile {
            from: temp_path.display().to_string(),
            to: dest_path.display().to_string(),
        }
    })?;
    ensure_active(&reporter.cancel)?;

    Ok(())
}

/// Streams one source file into an already created temp file (at `temp_path`, named in
/// errors) without publishing the final destination yet.
async fn stream_file_to_temp(
    source_path: &Path,
    mut destination: File,
    temp_path: &Path,
    reporter: &mut LocalCopyProgressReporter,
) -> Result<(), LocalCopyError> {
    let mut source = File::open(source_path)
        .await
        .map_err(|_| LocalCopyError::OpenSourceFile(source_path.display().to_string()))?;
    ensure_active(&reporter.cancel)?;

    let mut buffer = vec![0u8; 1024 * 1024];

    loop {
        let bytes_read = source
            .read(&mut buffer)
            .await
            .map_err(|_| LocalCopyError::ReadSourceFile(source_path.display().to_string()))?;
        ensure_active(&reporter.cancel)?;

        if bytes_read == 0 {
            break;
        }

        destination
            .write_all(&buffer[..bytes_read])
            .await
            .map_err(|_| LocalCopyError::WriteDestinationFile(temp_path.display().to_string()))?;

        reporter.advance(bytes_read as u64).await?;
    }

    destination
        .flush()
        .await
        .map_err(|_| LocalCopyError::FlushDestinationFile(temp_path.display().to_string()))?;
    ensure_active(&reporter.cancel)?;
    drop(destination);

    Ok(())
}

/// Traverses and copies a directory incrementally so memory grows with depth, not entry count.
async fn copy_directory_streaming(
    source_root: &Path,
    temp_dest_root: &Path,
    reporter: &mut LocalCopyProgressReporter,
) -> std::result::Result<(), LocalCopyError> {
    let root_entries = tokio::fs::read_dir(source_root)
        .await
        .map_err(|_| LocalCopyError::ReadDirectory(source_root.display().to_string()))?;
    let mut stack = vec![(
        source_root.to_path_buf(),
        temp_dest_root.to_path_buf(),
        root_entries,
    )];

    while !stack.is_empty() {
        ensure_active(&reporter.cancel)?;
        let entry = {
            let (source_dir, _, entries) = stack.last_mut().expect("stack is known non-empty");
            entries
                .next_entry()
                .await
                .map_err(|_| LocalCopyError::ReadDirectoryEntry(source_dir.display().to_string()))?
        };
        let Some(entry) = entry else {
            stack.pop();
            continue;
        };
        let entry_path = entry.path();
        let metadata = tokio::fs::symlink_metadata(&entry_path)
            .await
            .map_err(|_| LocalCopyError::ReadEntryMetadata(entry_path.display().to_string()))?;
        ensure_active(&reporter.cancel)?;
        let destination_path = stack
            .last()
            .expect("the current directory remains on the stack")
            .1
            .join(entry.file_name());

        if metadata.is_dir() {
            tokio::fs::create_dir(&destination_path)
                .await
                .map_err(|_| {
                    LocalCopyError::CreateDestinationDirectory(
                        destination_path.display().to_string(),
                    )
                })?;
            let entries = tokio::fs::read_dir(&entry_path)
                .await
                .map_err(|_| LocalCopyError::ReadDirectory(entry_path.display().to_string()))?;
            stack.push((entry_path, destination_path, entries));
        } else if metadata.is_file() {
            reporter.add_total_bytes(metadata.len());
            // The enclosing staging directory owns this name, so its cleanup covers failures.
            let temp_path = StagingKind::LocalCopy.sibling_of(&destination_path);
            copy_file_streaming(&entry_path, &destination_path, &temp_path, reporter).await?;
        } else if metadata.file_type().is_symlink() {
            let target = tokio::fs::read_link(&entry_path)
                .await
                .map_err(|_| LocalCopyError::ReadEntryMetadata(entry_path.display().to_string()))?;
            tokio::fs::symlink(target, &destination_path)
                .await
                .map_err(|_| {
                    LocalCopyError::CreateDestinationSymlink(destination_path.display().to_string())
                })?;
        } else {
            return Err(LocalCopyError::UnsupportedEntryType(
                entry_path.display().to_string(),
            ));
        }
    }

    Ok(())
}

/// Cancellation and progress plumbing shared by local copy, move and trash restore.
pub(crate) struct CopyContext<'a> {
    pub(crate) responder: &'a Responder,
    /// Stops staging-owning work when the request is canceled or its connection ends.
    pub(crate) cancel: &'a CancellationToken,
}

impl CopyContext<'_> {
    /// Starts a progress reporter bound to this request.
    fn reporter(&self, total_bytes: u64) -> LocalCopyProgressReporter {
        LocalCopyProgressReporter::new(self.responder.clone(), total_bytes, self.cancel.clone())
    }

    /// Runs one `LocalCopyFile` or `LocalCopyDirectory` command and logs its outcome.
    pub(crate) async fn copy_command(
        &self,
        source_path: String,
        dest_path: String,
        on_existing: CopyExistingMode,
        directory: bool,
    ) -> CommandResult {
        let label = if directory {
            "Local copy directory"
        } else {
            "Local copy file"
        };
        let request_id = self.responder.request_id();
        log!(
            Level::Info,
            "Started {}: request_id={}, source={}, dest={}, on_existing={:?}",
            label.to_lowercase(),
            request_id,
            source_path,
            dest_path,
            on_existing
        );
        let (source, dest) = (PathBuf::from(source_path), PathBuf::from(dest_path));
        let result = if directory {
            self.copy_directory(source, dest, on_existing).await
        } else {
            self.copy_file(source, dest, on_existing).await
        };
        match result {
            Ok(result) => {
                log!(
                    Level::Info,
                    "{} complete: request_id={}, result={}",
                    label,
                    request_id,
                    result.summary()
                );
                result
            }
            Err(error) => {
                log!(
                    Level::Info,
                    "{} failed: request_id={}, error={}",
                    label,
                    request_id,
                    error
                );
                AgentCommandError::from(error).into()
            }
        }
    }

    /// Copies one file through a hidden sibling so readers never see partial output.
    pub(crate) async fn copy_file(
        &self,
        source_path: PathBuf,
        dest_path: PathBuf,
        on_existing: CopyExistingMode,
    ) -> Result<CommandResult, LocalCopyError> {
        let source_metadata = tokio::fs::metadata(&source_path)
            .await
            .map_err(LocalCopyError::AccessSourceFile)?;
        ensure_active(self.cancel)?;
        if !source_metadata.is_file() {
            return Err(LocalCopyError::SourceNotFile(
                source_path.display().to_string(),
            ));
        }
        validate_local_copy_destination(&source_path, &dest_path, false).await?;
        ensure_active(self.cancel)?;
        validate_local_copy_parent(&dest_path).await?;
        ensure_active(self.cancel)?;
        check_existing_destination(&dest_path, on_existing, false).await?;
        ensure_active(self.cancel)?;
        let mut reporter = self.reporter(source_metadata.len());
        reporter.report(true).await?;
        // Staging is owned only once `create_new` made it, so a colliding name is never
        // truncated or later deleted.
        let (staging, destination) =
            StagingPath::create_sibling(&dest_path, StagingKind::LocalCopy, |candidate| async {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(candidate)
                    .await
            })
            .await
            .map_err(|_| LocalCopyError::CreateDestinationFile(dest_path.display().to_string()))?;
        let result = async {
            stream_file_to_temp(&source_path, destination, staging.path(), &mut reporter).await?;
            reporter.finish().await?;
            place_temp_at_destination(staging.path(), &dest_path, on_existing, false)
                .await
                .map_err(LocalCopyError::from)?;
            Ok(CommandResult::LocalCopyFile)
        }
        .await;
        match result {
            Ok(_) => {
                staging.keep();
            }
            Err(_) => staging.discard().await,
        }
        result
    }

    /// Copies a symbolic link itself through a hidden name instead of dereferencing its target.
    pub(crate) async fn copy_symlink(
        &self,
        source_path: PathBuf,
        dest_path: PathBuf,
        on_existing: CopyExistingMode,
    ) -> Result<CommandResult, LocalCopyError> {
        let source_metadata = tokio::fs::symlink_metadata(&source_path)
            .await
            .map_err(LocalCopyError::AccessSourceFile)?;
        ensure_active(self.cancel)?;
        if !source_metadata.file_type().is_symlink() {
            return Err(LocalCopyError::SourceNotFile(
                source_path.display().to_string(),
            ));
        }
        validate_local_copy_destination(&source_path, &dest_path, false).await?;
        ensure_active(self.cancel)?;
        validate_local_copy_parent(&dest_path).await?;
        ensure_active(self.cancel)?;
        check_existing_destination(&dest_path, on_existing, false).await?;
        let target = tokio::fs::read_link(&source_path)
            .await
            .map_err(LocalCopyError::AccessSourceFile)?;
        ensure_active(self.cancel)?;
        // `symlink` refuses existing names, so creating the link is the reservation.
        let (staging, ()) =
            StagingPath::create_sibling(&dest_path, StagingKind::LocalCopy, |candidate| {
                tokio::fs::symlink(target.clone(), candidate)
            })
            .await
            .map_err(LocalCopyError::CreateTempSymlink)?;
        let result = async {
            ensure_active(self.cancel)?;
            place_temp_at_destination(staging.path(), &dest_path, on_existing, false)
                .await
                .map_err(LocalCopyError::from)?;
            Ok(CommandResult::LocalCopyFile)
        }
        .await;
        match result {
            Ok(_) => {
                staging.keep();
            }
            Err(_) => staging.discard().await,
        }
        result
    }

    /// Copies a directory tree into hidden staging and publishes it in one placement.
    pub(crate) async fn copy_directory(
        &self,
        source_path: PathBuf,
        dest_path: PathBuf,
        on_existing: CopyExistingMode,
    ) -> Result<CommandResult, LocalCopyError> {
        let source_metadata = tokio::fs::metadata(&source_path)
            .await
            .map_err(LocalCopyError::AccessSourceDirectory)?;
        ensure_active(self.cancel)?;
        if !source_metadata.is_dir() {
            return Err(LocalCopyError::SourceNotDirectory(
                source_path.display().to_string(),
            ));
        }
        validate_local_copy_destination(&source_path, &dest_path, true).await?;
        ensure_active(self.cancel)?;
        validate_local_copy_parent(&dest_path).await?;
        ensure_active(self.cancel)?;
        check_existing_destination(&dest_path, on_existing, true).await?;
        ensure_active(self.cancel)?;
        // `create_dir` refuses existing names, so creating the directory is the reservation.
        let (staging, ()) = StagingPath::create_sibling(
            &dest_path,
            StagingKind::LocalCopyDirectory,
            tokio::fs::create_dir,
        )
        .await
        .map_err(LocalCopyError::CreateTempDirectory)?;
        let result = async {
            ensure_active(self.cancel)?;
            let mut reporter = self.reporter(0);
            reporter.report(true).await?;
            copy_directory_streaming(&source_path, staging.path(), &mut reporter).await?;
            reporter.finish().await?;
            place_temp_at_destination(staging.path(), &dest_path, on_existing, true)
                .await
                .map_err(LocalCopyError::from)?;
            Ok(CommandResult::LocalCopyDirectory)
        }
        .await;
        match result {
            Ok(_) => {
                staging.keep();
            }
            Err(_) => staging.discard().await,
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::responder::Responder;
    use redoor_core::test_support::TempDir;
    use redoor_protocol::ids::RequestId;

    use redoor_protocol::wire::command::{CommandErrorKind, CommandResult};

    #[tokio::test]
    async fn local_copy_file_fails_when_progress_channel_is_closed() {
        let temp_dir = TempDir::create();
        let root = temp_dir.path().join("local-copy");
        tokio::fs::create_dir(&root)
            .await
            .expect("local copy test root should be created");
        let source_path = root.join("source");
        let dest_path = root.join("dest");
        let source_contents = b"copy me";
        tokio::fs::write(&source_path, source_contents)
            .await
            .expect("test source file should be created");

        let (responder, frames) = Responder::recording(RequestId::new(42), 1);
        drop(frames);
        let cancel = CancellationToken::new();

        CopyContext {
            responder: &responder,
            cancel: &cancel,
        }
        .copy_command(
            source_path.display().to_string(),
            dest_path.display().to_string(),
            CopyExistingMode::Error,
            false,
        )
        .await;

        assert!(
            !tokio::fs::try_exists(&dest_path)
                .await
                .expect("destination existence should be readable"),
            "the destination should stay absent when the router text lane has already closed"
        );

        let file_name = dest_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("temp test file name should stay utf-8");
        let mut entries = tokio::fs::read_dir(&root)
            .await
            .expect("temp test parent should stay readable");
        while let Some(entry) = entries
            .next_entry()
            .await
            .expect("temp test directory iteration should succeed")
        {
            let entry_name = entry.file_name().to_string_lossy().to_string();
            assert!(
                !entry_name.contains(&format!(".{file_name}.redoor-local-copy-")),
                "failed local copies should remove their temp file instead of leaking hidden copy output"
            );
        }
    }

    #[tokio::test]
    async fn local_copy_progress_reporter_surfaces_closed_channel() {
        let (responder, frames) = Responder::recording(RequestId::new(7), 1);
        drop(frames);
        let mut reporter = LocalCopyProgressReporter::new(responder, 10, CancellationToken::new());

        let error = reporter
            .report(true)
            .await
            .expect_err("closed text lanes should stop local copy reporting immediately");

        assert!(
            matches!(error, LocalCopyError::ProgressChannelClosed),
            "the reporter should return a dedicated connection-health error so the copy can abort cleanly"
        );
        assert_eq!(
            error.kind(),
            CommandErrorKind::Internal,
            "the surfaced protocol error kind should stay stable for router-side handling"
        );
        let command_result: CommandResult = AgentCommandError::from(error).into();
        assert!(
            matches!(
                command_result,
                CommandResult::Error {
                    kind: CommandErrorKind::Internal,
                    ..
                }
            ),
            "connection-loss during local copy reporting should become a final command error if it can still be sent"
        );
    }

    #[tokio::test]
    async fn local_copy_symlink_preserves_the_link_instead_of_its_target() {
        let temp_dir = TempDir::create();
        let source_path = temp_dir.path().join("source-link");
        let dest_path = temp_dir.path().join("dest-link");
        tokio::fs::symlink("missing-relative-target", &source_path)
            .await
            .expect("source symbolic link should be created");
        let (responder, _frames) = Responder::recording(RequestId::new(44), 1);
        let cancel = CancellationToken::new();

        let result = CopyContext {
            responder: &responder,
            cancel: &cancel,
        }
        .copy_symlink(source_path, dest_path.clone(), CopyExistingMode::Error)
        .await
        .expect("symbolic link transfer should succeed without reading its target");

        // Reusing the file result keeps the existing transfer protocol stable for link entries.
        assert!(matches!(result, CommandResult::LocalCopyFile));
        // A broken relative target proves the transfer copied link metadata rather than target bytes.
        assert_eq!(
            tokio::fs::read_link(&dest_path)
                .await
                .expect("copied destination should remain a symbolic link"),
            PathBuf::from("missing-relative-target")
        );
    }

    /// Verifies generation cancellation removes an in-progress hidden copy before returning.
    #[tokio::test]
    async fn local_copy_cancellation_awaits_temp_file_cleanup() {
        let temp_dir = TempDir::create();
        let root = temp_dir.path().join("local-copy-cancel-root");
        tokio::fs::create_dir(&root)
            .await
            .expect("copy cancellation test root should be created");
        let source_path = root.join("source.bin");
        let dest_path = root.join("dest.bin");
        tokio::fs::write(&source_path, vec![7; 2 * 1024 * 1024])
            .await
            .expect("copy cancellation source should be created");
        let (responder, _frames) = Responder::recording(RequestId::new(43), 1);
        let cancel = CancellationToken::new();
        let context = CopyContext {
            responder: &responder,
            cancel: &cancel,
        };
        let copy = context.copy_file(
            source_path.clone(),
            dest_path.clone(),
            CopyExistingMode::Error,
        );
        tokio::pin!(copy);

        let temp_path = loop {
            tokio::select! {
                result = &mut copy => panic!("copy completed before cancellation gate: {result:?}"),
                () = tokio::task::yield_now() => {
                    let mut entries = tokio::fs::read_dir(&root)
                        .await
                        .expect("copy test root should remain readable");
                    let mut found = None;
                    while let Some(entry) = entries
                        .next_entry()
                        .await
                        .expect("copy test root iteration should succeed")
                    {
                        if entry.file_name().to_string_lossy().contains(".dest.bin.redoor-local-copy-") {
                            found = Some(entry.path());
                            break;
                        }
                    }
                    if let Some(path) = found {
                        break path;
                    }
                }
            }
        };

        cancel.cancel();
        let result = copy.await;

        // The canceled result proves the control-generation signal won over blocked progress output.
        assert!(matches!(result, Err(LocalCopyError::Canceled)));
        // Completion must be delayed until the hidden partial output has been removed.
        assert!(
            !tokio::fs::try_exists(&temp_path)
                .await
                .expect("temp lookup")
        );
        // Cancellation must not publish the incomplete destination.
        assert!(
            !tokio::fs::try_exists(&dest_path)
                .await
                .expect("destination lookup")
        );
    }
}
