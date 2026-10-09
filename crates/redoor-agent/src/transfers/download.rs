//! Generic driver for agent-to-server downloads and the byte sources it streams.
//!
//! File ranges and directory archives differ only in how bytes are produced; the stream
//! lifecycle (lookahead framing, cancellation, producer shutdown, unregistration and the
//! terminal frame) lives in [`DownloadDriver`] so both follow the same ordering.

use super::stream::{CANCELED_MESSAGE, StreamWriter};
use crate::commands::ctx::{CommandCtx, Reply};
use crate::directory_measurement::{
    archive_member_path, directory_archive_root, measure_directory,
};
use crate::runtime::cancel::Registration;
use crate::runtime::responder::Responder;
use anyhow::{Context, Result, bail};
use redoor_core::{log, log_error};
use redoor_protocol::{
    ids::RequestId,
    wire::{command::CommandResult, log::Level, stream, stream::StreamPayloadKind},
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

    /// Streams the source with one-block lookahead so the final data frame carries `is_last`.
    ///
    /// On cancellation or failure the order is: stop reading, shut the producer down,
    /// unregister the request, then send the terminal error frame. A successful stream keeps
    /// its registration until the final frame is written so a late cancel still reaches it.
    pub(crate) async fn run<S: ByteSource>(mut self, opened: Result<S, String>) -> Reply {
        let mut source = match opened {
            Ok(source) => source,
            Err(message) => {
                self.fail(message.as_bytes()).await;
                return Reply::Streamed;
            }
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
            DownloadEnd::Canceled => self.cancel().await,
            DownloadEnd::Failed(message) => self.fail(message.as_bytes()).await,
            DownloadEnd::Disconnected => {
                log!(
                    Level::Warning,
                    "WebSocket channel full or closed, aborting {}",
                    self.label.to_lowercase()
                );
            }
        }
        Reply::Streamed
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
    async fn complete(mut self, final_block: Vec<u8>, cancel: &CancellationToken) {
        let canceled = tokio::select! {
            _ = self.stream.send(&final_block, true) => false,
            _ = cancel.cancelled() => true,
        };
        if canceled {
            self.cancel().await;
            return;
        }
        log!(
            Level::Info,
            "{} complete: request_id={}, path={}, chunks={}",
            self.label,
            self.registration.key(),
            self.path,
            self.stream.frames_sent()
        );
    }

    /// Unregisters and sends the cancellation frame the server expects.
    async fn cancel(self) {
        log!(
            Level::Info,
            "Stopping {} after cancel: request_id={}, path={}",
            self.label.to_lowercase(),
            self.registration.key(),
            self.path
        );
        self.fail(CANCELED_MESSAGE).await;
    }

    /// Unregisters, then sends a terminal error frame unless the connection is being torn down.
    ///
    /// Teardown must be able to join this worker even if the payload queue is still full.
    async fn fail(mut self, message: &[u8]) {
        let revoked = self.registration.revoked().clone();
        drop(self.registration);
        tokio::select! {
            biased;
            _ = self.stream.send_error(message) => {}
            _ = revoked.cancelled() => {}
        }
    }
}

/// Runs the `RawDownload` command for one file or byte range.
pub(crate) async fn download_file(
    path: String,
    range_start: Option<u64>,
    range_end: Option<u64>,
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
    let output = match ctx.into_output(StreamPayloadKind::RawFile) {
        Ok(output) => output,
        Err(error) => return CommandResult::from(error).into(),
    };
    let opened = FileRangeSource::open(&path, range_start, range_end).await;
    DownloadDriver::new("Raw download", path, output.stream, output.registration)
        .run(opened)
        .await
}

/// Runs the `TarDownload` command for one directory archive.
pub(crate) async fn download_directory(path: String, include_root: bool, ctx: CommandCtx) -> Reply {
    log!(
        Level::Info,
        "Started tar download: request_id={}, path={}",
        ctx.responder.request_id(),
        path
    );
    let output = match ctx.into_output(StreamPayloadKind::Tar) {
        Ok(output) => output,
        Err(error) => return CommandResult::from(error).into(),
    };
    let cancel = output.registration.token().clone();
    let opened = TarDirectorySource::open(&path, include_root, output.responder, cancel).await;
    DownloadDriver::new(
        "Tar directory download",
        path,
        output.stream,
        output.registration,
    )
    .run(opened)
    .await
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
    archive: JoinHandle<()>,
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
            if let Err(error) = write_directory_tar(writer, &source_path, include_root) {
                redoor_core::log_failure!(
                    Level::Error,
                    "Tar directory streaming failed: path={}, error={}",
                    source_path.display(),
                    error
                );
            }
        });
        Ok(Self {
            receiver,
            archive,
            size_measure,
        })
    }
}

impl ByteSource for TarDirectorySource {
    /// Yields the next block the archive writer produced.
    async fn next_block(&mut self) -> Result<Option<Vec<u8>>, String> {
        Ok(self.receiver.recv().await)
    }

    /// Closes the pipe so a still-running writer fails fast, then joins both producers.
    async fn finish(self) {
        drop(self.receiver);
        let _ = self.archive.await;
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

    /// Runs a driver over scripted blocks and returns the decoded frames plus lifecycle flags.
    async fn drive(
        blocks: Vec<Result<Option<Vec<u8>>, String>>,
        cancel_first: bool,
    ) -> (Vec<StreamChunk>, bool, bool, bool) {
        redoor_core::logging::init(None).await.unwrap();
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
        DownloadDriver::new(
            "Raw download",
            "/tmp/file".to_string(),
            StreamWriter::new(
                transfer,
                RequestId::new(9),
                redoor_protocol::wire::stream::StreamPayloadKind::RawFile,
            ),
            registration,
        )
        .run(Ok(source))
        .await;
        let mut decoded = Vec::new();
        while let Ok(frame) = frames.try_recv() {
            let tokio_tungstenite::tungstenite::protocol::Message::Binary(bytes) = frame else {
                panic!("downloads must use binary frames");
            };
            decoded.push(StreamChunk::from_bytes(&bytes).unwrap());
        }
        (
            decoded,
            finished.load(std::sync::atomic::Ordering::SeqCst),
            registered_at_finish.load(std::sync::atomic::Ordering::SeqCst),
            registry.contains(&RequestId::new(9)),
        )
    }

    /// Verifies lookahead framing marks only the final data frame as last.
    #[tokio::test]
    async fn completed_stream_marks_only_the_final_frame_last() {
        let (frames, finished, _, registered_after) = drive(
            vec![Ok(Some(b"a".to_vec())), Ok(Some(b"b".to_vec())), Ok(None)],
            false,
        )
        .await;

        // Lookahead holds one block back so the last data frame can carry the end marker.
        assert_eq!(
            frames
                .iter()
                .map(|frame| (frame.data.clone(), frame.is_last, frame.is_error))
                .collect::<Vec<_>>(),
            vec![(b"a".to_vec(), false, false), (b"b".to_vec(), true, false)]
        );
        // Producers are always joined before the stream ends.
        assert!(finished);
        // Completion releases the request id.
        assert!(!registered_after);
    }

    /// Verifies the cancel ordering: stop producer, unregister, then report.
    #[tokio::test]
    async fn cancellation_finishes_the_source_then_unregisters_then_reports() {
        let (frames, finished, registered_at_finish, registered_after) =
            drive(vec![Ok(Some(b"a".to_vec()))], true).await;

        // The producer is stopped while the request is still registered.
        assert!(finished && registered_at_finish);
        // The request is unregistered before the stream reports cancellation.
        assert!(!registered_after);
        // The only frame is the terminal cancellation error the server expects.
        assert_eq!(frames.len(), 1);
        assert!(frames[0].is_error);
        assert_eq!(frames[0].data, CANCELED_MESSAGE);
    }

    /// Verifies source failures follow the same cleanup ordering as cancellation.
    #[tokio::test]
    async fn source_failure_reports_its_message_after_cleanup() {
        let (frames, finished, registered_at_finish, registered_after) = drive(
            vec![
                Ok(Some(b"a".to_vec())),
                Err("Failed to read file: boom".to_string()),
            ],
            false,
        )
        .await;

        // Failures follow the same cleanup ordering as cancellation.
        assert!(finished && registered_at_finish && !registered_after);
        // Buffered lookahead data is not sent as if the stream had succeeded.
        assert_eq!(frames.len(), 1);
        // The source's own message becomes the terminal error frame.
        assert!(frames[0].is_error);
        assert_eq!(frames[0].data, b"Failed to read file: boom");
    }

    /// Failed range seeks must not retain cancellation handles after the worker exits.
    #[tokio::test]
    async fn failed_seek_unregisters_raw_download() {
        redoor_core::logging::init(None).await.unwrap();
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

        DownloadDriver::new(
            "Raw download",
            "Cargo.toml".to_string(),
            StreamWriter::new(
                transfer,
                RequestId::new(1),
                redoor_protocol::wire::stream::StreamPayloadKind::RawFile,
            ),
            registration,
        )
        .run(opened)
        .await;

        // A failed worker must not leave its cancellation handle in the registry.
        assert!(!registry.contains(&RequestId::new(1)));
        let tokio_tungstenite::tungstenite::protocol::Message::Binary(bytes) =
            frames.recv().await.unwrap()
        else {
            panic!("seek failure should use the binary stream protocol");
        };
        let chunk = StreamChunk::from_bytes(&bytes).unwrap();
        // Check the failing seek branch, rather than merely any early worker exit.
        assert!(chunk.is_error);
        assert!(chunk.data.starts_with(b"Failed to seek file:"));
    }
}
