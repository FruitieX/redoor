//! Generic driver for agent-to-server downloads and the byte sources it streams.
//!
//! File ranges and directory archives differ only in how bytes are produced; the stream
//! lifecycle (lookahead framing, cancellation, producer shutdown, unregistration and the
//! result) lives in [`DownloadDriver`] so both follow the same ordering.

use super::stream::StreamWriter;
use crate::commands::ctx::{CommandCtx, Reply};
use crate::directory_measurement::{
    archive_member_path, directory_archive_root, measure_directory,
};
use crate::runtime::cancel::Registration;
use crate::runtime::responder::Responder;
use anyhow::{Context, Result, bail};
use redoor_core::{log, log_error};
use redoor_protocol::wire::request::{self, CommandError};
use redoor_protocol::{
    ids::RequestId,
    wire::{command::CommandErrorKind, log::Level, stream},
};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt},
    sync::mpsc,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

/// Producer of one download's payload bytes.
pub(crate) trait ByteSource: Send {
    /// Returns the next block of payload, `None` at end of stream, or a terminal error message.
    ///
    /// Must be cancellation safe: the driver drops it when cancellation wins.
    fn next_block(&mut self) -> impl Future<Output = Result<Option<Vec<u8>>, String>> + Send;

    /// Stops background producers and waits for them so nothing outlives the stream.
    fn finish(self) -> impl Future<Output = ()> + Send;
}

/// How the read loop ended.
enum DownloadEnd {
    Complete,
    Canceled,
    Failed(String),
    Disconnected,
}

/// Lifecycle owner for one download request.
pub(crate) struct DownloadDriver {
    label: &'static str,
    path: String,
    stream: StreamWriter,
    registration: Registration<RequestId>,
}

/// How a download ended, with the stream its result must follow.
pub(crate) struct DownloadOutcome {
    pub(crate) stream: StreamWriter,
    pub(crate) result: Result<(), CommandError>,
}

impl DownloadDriver {
    /// Binds the driver to the request's stream writer and cancellation registration.
    pub(crate) fn new(
        label: &'static str,
        path: String,
        stream: StreamWriter,
        registration: Registration<RequestId>,
    ) -> Self {
        Self {
            label,
            path,
            stream,
            registration,
        }
    }

    /// Streams the source with one-block lookahead so the final data frame carries `FIN`.
    ///
    /// Only a complete stream ends with `FIN`; a canceled or failed one just stops, and the
    /// result (sent after the data on the same lane) says why. On cancellation or failure the
    /// order is: stop reading, shut the producer down, unregister the request, then report.
    /// A successful stream keeps its registration until `FIN` is written so a late cancel
    /// still reaches it.
    pub(crate) async fn run<S: ByteSource>(mut self, opened: Result<S, String>) -> DownloadOutcome {
        let mut source = match opened {
            Ok(source) => source,
            Err(message) => return self.fail(message),
        };
        let cancel = self.registration.token().clone();
        let mut pending: Option<Vec<u8>> = None;
        let end = loop {
            let block = tokio::select! {
                _ = cancel.cancelled() => break DownloadEnd::Canceled,
                block = source.next_block() => block,
            };
            match block {
                Ok(Some(data)) => {
                    if let Some(previous) = pending.replace(data)
                        && let Some(end) = self.send_cancelable(&previous, &cancel).await
                    {
                        break end;
                    }
                }
                Ok(None) => break DownloadEnd::Complete,
                Err(message) => break DownloadEnd::Failed(message),
            }
        };
        source.finish().await;

        match end {
            DownloadEnd::Complete => self.complete(pending.unwrap_or_default(), &cancel).await,
            DownloadEnd::Canceled => self.cancel(),
            DownloadEnd::Failed(message) => self.fail(message),
            DownloadEnd::Disconnected => {
                log!(
                    Level::Warning,
                    "Connection closed, aborting {}",
                    self.label.to_lowercase()
                );
                self.fail("Connection closed".to_string())
            }
        }
    }

    /// Sends one non-final block while cancellation stays able to interrupt backpressure.
    async fn send_cancelable(
        &mut self,
        data: &[u8],
        cancel: &CancellationToken,
    ) -> Option<DownloadEnd> {
        tokio::select! {
            sent = self.stream.send(data, false) => (!sent).then_some(DownloadEnd::Disconnected),
            _ = cancel.cancelled() => Some(DownloadEnd::Canceled),
        }
    }

    /// Writes the final frame, still honoring a cancel that arrives while it is blocked.
    async fn complete(
        mut self,
        final_block: Vec<u8>,
        cancel: &CancellationToken,
    ) -> DownloadOutcome {
        let canceled = tokio::select! {
            _ = self.stream.send(&final_block, true) => false,
            _ = cancel.cancelled() => true,
        };
        if canceled {
            return self.cancel();
        }
        log!(
            Level::Info,
            "{} complete: request_id={}, path={}, chunks={}",
            self.label,
            self.registration.key(),
            self.path,
            self.stream.frames_sent()
        );
        drop(self.registration);
        DownloadOutcome {
            stream: self.stream,
            result: Ok(()),
        }
    }

    /// Unregisters and reports the cancellation.
    fn cancel(self) -> DownloadOutcome {
        log!(
            Level::Info,
            "Stopping {} after cancel: request_id={}, path={}",
            self.label.to_lowercase(),
            self.registration.key(),
            self.path
        );
        let message = format!("{} canceled by server", self.label);
        self.finish_with(Err(CommandError::new(CommandErrorKind::Canceled, message)))
    }

    /// Unregisters and reports the failure; the stream ends without `FIN`.
    fn fail(self, message: String) -> DownloadOutcome {
        self.finish_with(Err(CommandError::new(CommandErrorKind::Internal, message)))
    }

    /// Unregisters before the result can go out, so a late cancel finds nothing to stop.
    fn finish_with(self, result: Result<(), CommandError>) -> DownloadOutcome {
        drop(self.registration);
        DownloadOutcome {
            stream: self.stream,
            result,
        }
    }
}

/// Runs the `RawDownload` command for one file or byte range.
pub(crate) async fn download_file(
    path: String,
    range_start: Option<u64>,
    range_end: Option<u64>,
    expected_identity: Option<redoor_protocol::wire::command::MoveSourceIdentity>,
    ctx: CommandCtx,
) -> Reply {
    let request_id = ctx.responder.request_id();
    log!(
        Level::Info,
        "Started raw download: request_id={}, path={}, range_start={:?}, range_end={:?}",
        request_id,
        path,
        range_start,
        range_end
    );
    let output = match ctx.into_output() {
        Ok(output) => output,
        Err(error) => return Reply::failure(error),
    };
    let opened = FileRangeSource::open(&path, range_start, range_end).await;
    if let Ok(source) = &opened
        && let Some(identity) = expected_identity
    {
        match source.file.metadata().await {
            Ok(metadata)
                if redoor_protocol::wire::command::MoveSourceIdentity::from_metadata(&metadata)
                    == identity => {}
            _ => {
                drop(output.registration);
                return Reply::stream::<request::RawDownload>(
                    output.stream,
                    Err(CommandError::new(
                        CommandErrorKind::SourceChanged,
                        "Source changed before resumed read",
                    )),
                );
            }
        }
    }
    let outcome = DownloadDriver::new("Raw download", path, output.stream, output.registration)
        .run(opened)
        .await;
    Reply::stream::<request::RawDownload>(outcome.stream, outcome.result)
}

/// Runs the `TarDownload` command for one directory archive.
pub(crate) async fn download_directory(path: String, include_root: bool, ctx: CommandCtx) -> Reply {
    log!(
        Level::Info,
        "Started tar download: request_id={}, path={}",
        ctx.responder.request_id(),
        path
    );
    let output = match ctx.into_output() {
        Ok(output) => output,
        Err(error) => return Reply::failure(error),
    };
    let cancel = output.registration.token().clone();
    let opened = TarDirectorySource::open(&path, include_root, output.responder, cancel).await;
    let outcome = DownloadDriver::new(
        "Tar directory download",
        path,
        output.stream,
        output.registration,
    )
    .run(opened)
    .await;
    Reply::stream::<request::TarDownload>(outcome.stream, outcome.result)
}

/// Byte range of one regular file.
pub(crate) struct FileRangeSource {
    file: File,
    /// Bytes left in the requested range, or `None` to read to end of file.
    remaining: Option<u64>,
    buffer: Vec<u8>,
}

impl FileRangeSource {
    /// Opens the file and seeks to the range start.
    pub(crate) async fn open(
        path: &str,
        range_start: Option<u64>,
        range_end: Option<u64>,
    ) -> Result<Self, String> {
        let mut file = File::open(path).await.map_err(|error| {
            let message = format!("Failed to open file: {error:#}");
            log_error!(anyhow::Error::new(error), "Failed to open file");
            message
        })?;
        let mut remaining = None;
        if let Some(start) = range_start {
            file.seek(std::io::SeekFrom::Start(start))
                .await
                .map_err(|error| {
                    let message = format!("Failed to seek file: {error:#}");
                    log_error!(anyhow::Error::new(error), "Failed to seek file");
                    message
                })?;
            remaining = range_end.map(|end| end.saturating_sub(start));
        }
        Ok(Self {
            file,
            remaining,
            buffer: vec![0; stream::CHUNK_SIZE],
        })
    }
}

impl ByteSource for FileRangeSource {
    /// Reads at most one chunk, never past the requested range end.
    async fn next_block(&mut self) -> Result<Option<Vec<u8>>, String> {
        let read_size = match self.remaining {
            Some(0) => return Ok(None),
            Some(remaining) => {
                stream::CHUNK_SIZE.min(usize::try_from(remaining).unwrap_or(usize::MAX))
            }
            None => stream::CHUNK_SIZE,
        };
        let read = self
            .file
            .read(&mut self.buffer[..read_size])
            .await
            .map_err(|error| {
                let message = format!("Failed to read file: {error:#}");
                log_error!(anyhow::Error::new(error), "Failed to read file");
                message
            })?;
        if read == 0 {
            return Ok(None);
        }
        if let Some(remaining) = self.remaining.as_mut() {
            *remaining = remaining.saturating_sub(read as u64);
        }
        Ok(Some(self.buffer[..read].to_vec()))
    }

    /// Closing the file is the only cleanup a file range needs.
    async fn finish(self) {}
}

/// Appends a directory tree into a tar builder in stable order so streams are deterministic.
fn append_directory_entries<W: Write>(
    builder: &mut tar::Builder<W>,
    source_root: &Path,
    current_path: &Path,
    archive_root: Option<&Path>,
) -> Result<()> {
    let mut entries = std::fs::read_dir(current_path)
        .with_context(|| format!("Failed to read directory: {}", current_path.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("Failed to read directory entry: {}", current_path.display()))?;

    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let entry_path = entry.path();
        let archive_path = archive_member_path(source_root, &entry_path, archive_root)?;
        let metadata = std::fs::symlink_metadata(&entry_path)
            .with_context(|| format!("Failed to read entry metadata: {}", entry_path.display()))?;

        if metadata.is_dir() {
            builder
                .append_dir(&archive_path, &entry_path)
                .with_context(|| {
                    format!(
                        "Failed to append directory to tar: {}",
                        entry_path.display()
                    )
                })?;
            append_directory_entries(builder, source_root, &entry_path, archive_root)?;
        } else if metadata.is_file() {
            let mut file = std::fs::File::open(&entry_path).with_context(|| {
                format!("Failed to open file for tar: {}", entry_path.display())
            })?;
            builder
                .append_file(&archive_path, &mut file)
                .with_context(|| {
                    format!("Failed to append file to tar: {}", entry_path.display())
                })?;
        } else {
            bail!(
                "Unsupported directory entry type in copy source: {}",
                entry_path.display()
            );
        }
    }

    Ok(())
}

/// Writes the same directory archive the download worker streams, without reading
/// through a websocket adapter so tests can compare encoded length to the walk.
fn write_directory_tar<W: Write>(writer: W, source_path: &Path, include_root: bool) -> Result<W> {
    let mut builder = tar::Builder::new(writer);
    // Sparse encoding would omit holes and disagree with a metadata-only size walk.
    builder.sparse(false);
    let archive_root = directory_archive_root(source_path, include_root);
    if let Some(root) = archive_root.as_deref() {
        builder.append_dir(root, source_path).with_context(|| {
            format!(
                "Failed to append root directory to tar: {}",
                source_path.display()
            )
        })?;
    }
    append_directory_entries(
        &mut builder,
        source_path,
        source_path,
        archive_root.as_deref(),
    )?;
    builder.finish().context("Failed to finalize tar stream")?;
    builder
        .into_inner()
        .context("Failed to unwrap finished tar writer")
}

/// Bridges synchronous `tar::Builder` writes into the async websocket sender.
///
/// The tar crate exposes a blocking `std::io::Write` API, so directory archive
/// creation runs in a blocking task and pushes produced tar chunks through this
/// adapter into an async channel.
struct ChannelTarWriter {
    sender: mpsc::Sender<Vec<u8>>,
    runtime: tokio::runtime::Handle,
}

impl std::io::Write for ChannelTarWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.runtime
            .block_on(self.sender.send(buf.to_vec()))
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "Tar stream receiver dropped",
                )
            })?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Directory archive produced by a blocking tar writer, plus an optional size walk.
pub(crate) struct TarDirectorySource {
    receiver: mpsc::Receiver<Vec<u8>>,
    /// The blocking tar writer; its result is the only way to tell a finished archive from
    /// one that stopped half-way (both just drop the block sender). `None` once joined.
    archive: Option<JoinHandle<Result<(), String>>>,
    size_measure: Option<JoinHandle<()>>,
}

impl TarDirectorySource {
    /// Validates the directory and starts the archive writer (and size walk when requested).
    pub(crate) async fn open(
        path: &str,
        include_root: bool,
        responder: Responder,
        cancel: CancellationToken,
    ) -> Result<Self, String> {
        let source_path = PathBuf::from(path);
        let request_id = responder.request_id();
        let metadata = tokio::fs::metadata(&source_path).await.map_err(|error| {
            redoor_core::log_failure!(
                Level::Error,
                "Tar download open failed: request_id={}, path={}, error={}",
                request_id,
                path,
                error
            );
            format!("Failed to open directory: {error}")
        })?;
        if !metadata.is_dir() {
            redoor_core::log_failure!(
                Level::Error,
                "Tar download open failed: request_id={}, path={}, error=not a directory",
                request_id,
                path
            );
            return Err(format!("Source path is not a directory: {path}"));
        }
        // REST directory downloads include the root; remote copies do not and already ignore
        // the total, so they skip the extra metadata walk.
        let size_measure = include_root.then(|| {
            tokio::spawn(report_archive_size(
                source_path.clone(),
                include_root,
                responder,
                cancel,
            ))
        });
        let (sender, receiver) = mpsc::channel::<Vec<u8>>(8);
        let archive = tokio::task::spawn_blocking(move || {
            let writer = ChannelTarWriter {
                sender,
                runtime: tokio::runtime::Handle::current(),
            };
            write_directory_tar(writer, &source_path, include_root)
                .map(drop)
                .map_err(|error| {
                    redoor_core::log_failure!(
                        Level::Error,
                        "Tar directory streaming failed: path={}, error={:#}",
                        source_path.display(),
                        error
                    );
                    format!("Failed to archive directory: {error:#}")
                })
        });
        Ok(Self {
            receiver,
            archive: Some(archive),
            size_measure,
        })
    }
}

impl ByteSource for TarDirectorySource {
    /// Yields the next block the archive writer produced, and at the end of the blocks the
    /// writer's outcome, so a truncated archive ends with an error frame instead of `is_last`.
    async fn next_block(&mut self) -> Result<Option<Vec<u8>>, String> {
        if let Some(block) = self.receiver.recv().await {
            return Ok(Some(block));
        }
        let Some(archive) = self.archive.as_mut() else {
            return Ok(None);
        };
        // Awaiting by reference keeps this cancellation safe: `finish` joins it otherwise.
        let outcome = archive.await;
        self.archive = None;
        match outcome {
            Ok(Ok(())) => Ok(None),
            Ok(Err(message)) => Err(message),
            Err(error) => Err(format!("Directory archive task failed: {error}")),
        }
    }

    /// Closes the pipe so a still-running writer fails fast, then joins both producers.
    async fn finish(self) {
        drop(self.receiver);
        if let Some(archive) = self.archive {
            let _ = archive.await;
        }
        if let Some(size_measure) = self.size_measure {
            let _ = size_measure.await;
        }
    }
}

/// Publishes the predicted tar size on the control lane so progress can show a total while
/// bytes are still streaming.
async fn report_archive_size(
    source_path: PathBuf,
    include_root: bool,
    responder: Responder,
    cancel: CancellationToken,
) {
    let request_id = responder.request_id();
    match measure_directory(&source_path, include_root, &cancel).await {
        Ok(Some(measurement)) => {
            if !measurement.tar_complete || !measurement.errors.is_empty() {
                log!(
                    Level::Warning,
                    "Tar download size measure incomplete: request_id={}, path={}, read_errors={}",
                    request_id,
                    source_path.display(),
                    measurement.errors.len()
                );
                return;
            }
            log!(
                Level::Info,
                "Tar download size measured: request_id={}, path={}, total_bytes={}",
                request_id,
                source_path.display(),
                measurement.tar_bytes
            );
            let _ = responder.progress(0, Some(measurement.tar_bytes)).await;
        }
        Ok(None) => log!(
            Level::Info,
            "Tar download size measure canceled: request_id={}, path={}",
            request_id,
            source_path.display()
        ),
        Err(error) => log!(
            Level::Warning,
            "Tar download size measure failed: request_id={}, path={}, error={}",
            request_id,
            source_path.display(),
            error
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::cancel::CancelRegistry;
    use redoor_core::test_support::TempDir;
    use redoor_protocol::wire::stream::StreamChunk;

    async fn write_tree(root: &Path) {
        tokio::fs::create_dir_all(root.join("nested").join("deeper"))
            .await
            .expect("nested directories should be created");
        tokio::fs::create_dir_all(root.join("empty"))
            .await
            .expect("empty directory should be created");
        tokio::fs::write(root.join("top.txt"), b"directory archive root file")
            .await
            .expect("root file should be written");
        tokio::fs::write(
            root.join("nested").join("deeper").join("child.txt"),
            b"directory archive nested file",
        )
        .await
        .expect("nested file should be written");
        tokio::fs::write(root.join("a".repeat(120)), b"long name")
            .await
            .expect("long member name should be written so GNU long-link size is covered");
    }

    #[tokio::test]
    async fn predicted_tar_size_matches_builder_stream() {
        let temp_dir = TempDir::create();
        let source_root = temp_dir.path().join("tar-size-tree");
        write_tree(&source_root).await;

        for include_root in [true, false] {
            let predicted =
                measure_directory(&source_root, include_root, &CancellationToken::new())
                    .await
                    .expect("metadata walk should succeed")
                    .expect("uncanceled walk should return a size")
                    .tar_bytes;
            let encoded = write_directory_tar(Vec::new(), &source_root, include_root)
                .expect("tar builder should encode the same tree");
            assert_eq!(
                predicted,
                encoded.len() as u64,
                "predicted tar size must include headers, padding, and end blocks for include_root={include_root}"
            );
        }

        let empty_root = temp_dir.path().join("tar-size-empty");
        tokio::fs::create_dir_all(&empty_root)
            .await
            .expect("empty directory should be created");
        let predicted_empty = measure_directory(&empty_root, true, &CancellationToken::new())
            .await
            .expect("empty-directory walk should succeed")
            .expect("uncanceled empty walk should return a size")
            .tar_bytes;
        let encoded_empty = write_directory_tar(Vec::new(), &empty_root, true)
            .expect("empty directory should still encode a root member and end blocks");
        assert_eq!(
            predicted_empty,
            encoded_empty.len() as u64,
            "empty directories still have a root header plus two end blocks"
        );
    }

    #[tokio::test]
    async fn canceled_size_walk_returns_none() {
        let temp_dir = TempDir::create();
        let source_root = temp_dir.path().join("tar-size-canceled");
        tokio::fs::create_dir_all(&source_root)
            .await
            .expect("directory should be created");
        let cancel_receiver = CancellationToken::new();
        cancel_receiver.cancel();

        let measured = measure_directory(&source_root, true, &cancel_receiver)
            .await
            .expect("a canceled walk is not a filesystem failure");
        assert_eq!(
            measured, None,
            "cancel must stop the walk before it publishes a stale total"
        );
    }

    /// Source that yields scripted blocks and records when it was finished.
    struct ScriptedSource {
        blocks: Vec<Result<Option<Vec<u8>>, String>>,
        finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
        registry: CancelRegistry<RequestId>,
        registered_at_finish: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl ByteSource for ScriptedSource {
        /// Yields the next scripted block, then waits forever like an idle producer.
        async fn next_block(&mut self) -> Result<Option<Vec<u8>>, String> {
            if self.blocks.is_empty() {
                return std::future::pending().await;
            }
            self.blocks.remove(0)
        }

        /// Records that the producer was stopped and whether the request was still registered.
        async fn finish(self) {
            self.registered_at_finish.store(
                self.registry.contains(&RequestId::new(9)),
                std::sync::atomic::Ordering::SeqCst,
            );
            self.finished
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// What one scripted download produced.
    struct Driven {
        frames: Vec<StreamChunk>,
        result: Result<(), CommandError>,
        finished: bool,
        registered_at_finish: bool,
        registered_after: bool,
    }

    /// Runs a driver over scripted blocks and returns the decoded frames plus lifecycle flags.
    async fn drive(blocks: Vec<Result<Option<Vec<u8>>, String>>, cancel_first: bool) -> Driven {
        let registry = CancelRegistry::new(None);
        let registration = registry.register(RequestId::new(9)).unwrap();
        if cancel_first {
            registration.cancel();
        }
        let (transfer, mut frames) = mpsc::channel(16);
        let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let registered_at_finish = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let source = ScriptedSource {
            blocks,
            finished: finished.clone(),
            registry: registry.clone(),
            registered_at_finish: registered_at_finish.clone(),
        };
        let outcome = DownloadDriver::new(
            "Raw download",
            "/tmp/file".to_string(),
            StreamWriter::for_test(transfer, RequestId::new(9)),
            registration,
        )
        .run(Ok(source))
        .await;
        let mut decoded = Vec::new();
        while let Ok(frame) = frames.try_recv() {
            let Some(chunk) = frame.as_stream() else {
                panic!("downloads must use stream frames");
            };
            decoded.push(chunk.clone());
        }
        Driven {
            frames: decoded,
            result: outcome.result,
            finished: finished.load(std::sync::atomic::Ordering::SeqCst),
            registered_at_finish: registered_at_finish.load(std::sync::atomic::Ordering::SeqCst),
            registered_after: registry.contains(&RequestId::new(9)),
        }
    }

    /// Verifies lookahead framing marks only the final data frame as last.
    #[tokio::test]
    async fn completed_stream_marks_only_the_final_frame_last() {
        let driven = drive(
            vec![Ok(Some(b"a".to_vec())), Ok(Some(b"b".to_vec())), Ok(None)],
            false,
        )
        .await;

        // Lookahead holds one block back so the last data frame can carry FIN.
        assert_eq!(
            driven
                .frames
                .iter()
                .map(|frame| (frame.data.clone(), frame.is_last))
                .collect::<Vec<_>>(),
            vec![(b"a".to_vec(), false), (b"b".to_vec(), true)]
        );
        // The success result follows the data.
        assert!(driven.result.is_ok());
        // Producers are always joined before the stream ends.
        assert!(driven.finished);
        // Completion releases the request id.
        assert!(!driven.registered_after);
    }

    /// Verifies the cancel ordering: stop producer, unregister, then report.
    #[tokio::test]
    async fn cancellation_finishes_the_source_then_unregisters_then_reports() {
        let driven = drive(vec![Ok(Some(b"a".to_vec()))], true).await;

        // The producer is stopped while the request is still registered.
        assert!(driven.finished && driven.registered_at_finish);
        // The request is unregistered before the result reports cancellation.
        assert!(!driven.registered_after);
        // A canceled stream never claims completion with FIN.
        assert!(driven.frames.iter().all(|frame| !frame.is_last));
        // The result names the cancellation.
        assert!(matches!(
            driven.result,
            Err(CommandError {
                kind: CommandErrorKind::Canceled,
                ..
            })
        ));
    }

    /// Verifies source failures follow the same cleanup ordering as cancellation.
    #[tokio::test]
    async fn source_failure_reports_its_message_after_cleanup() {
        let driven = drive(
            vec![
                Ok(Some(b"a".to_vec())),
                Err("Failed to read file: boom".to_string()),
            ],
            false,
        )
        .await;

        // Failures follow the same cleanup ordering as cancellation.
        assert!(driven.finished && driven.registered_at_finish && !driven.registered_after);
        // Buffered lookahead data is not sent as if the stream had succeeded.
        assert!(driven.frames.is_empty());
        // The source's own message becomes the error result.
        assert!(matches!(
            driven.result,
            Err(CommandError { message, .. }) if message == "Failed to read file: boom"
        ));
    }

    /// A directory the tar writer cannot finish (here: a socket member) must end the source
    /// with an error, never with `Ok(None)`, or the truncated archive is reported complete.
    #[tokio::test]
    async fn tar_source_surfaces_a_failed_archive_writer() {
        let temp_dir = TempDir::create();
        let source_root = temp_dir.path().join("tar-fails-midway");
        tokio::fs::create_dir_all(&source_root).await.unwrap();
        tokio::fs::write(source_root.join("a.txt"), vec![1u8; 64 * 1024])
            .await
            .unwrap();
        // Sorted after the regular file, so the writer fails after streaming some blocks.
        let _socket = std::os::unix::net::UnixListener::bind(source_root.join("z.sock")).unwrap();
        let (responder, _frames) = Responder::recording(RequestId::new(3), 4);
        let mut source = TarDirectorySource::open(
            source_root.to_str().unwrap(),
            false,
            responder,
            CancellationToken::new(),
        )
        .await
        .expect("the directory itself is readable");
        let mut streamed = 0;
        let outcome = loop {
            match source.next_block().await {
                Ok(Some(block)) => streamed += block.len(),
                other => break other,
            }
        };
        // Some of the archive was produced before the failure.
        assert!(streamed > 0);
        // The writer's error is the terminal outcome, naming the offending entry.
        let Err(message) = outcome else {
            panic!("a truncated archive must not end successfully: {outcome:?}");
        };
        assert!(message.contains("z.sock"), "{message}");
        source.finish().await;
    }

    /// Failed range seeks must not retain cancellation handles after the worker exits.
    #[tokio::test]
    async fn failed_seek_unregisters_raw_download() {
        let registry = CancelRegistry::new(None);
        let registration = registry.register(RequestId::new(1)).unwrap();
        let (transfer, mut frames) = mpsc::channel(1);
        // Regular files cannot seek beyond the signed offset supported by the OS.
        let opened = FileRangeSource::open(
            concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
            Some(u64::MAX),
            None,
        )
        .await;

        let outcome = DownloadDriver::new(
            "Raw download",
            "Cargo.toml".to_string(),
            StreamWriter::for_test(transfer, RequestId::new(1)),
            registration,
        )
        .run(opened)
        .await;

        // A failed worker must not leave its cancellation handle in the registry.
        assert!(!registry.contains(&RequestId::new(1)));
        // No data or FIN was sent for a stream that never started.
        assert!(frames.try_recv().is_err());
        // Check the failing seek branch, rather than merely any early worker exit.
        assert!(matches!(
            outcome.result,
            Err(CommandError { message, .. }) if message.starts_with("Failed to seek file:")
        ));
    }
}
