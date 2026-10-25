//! Size-bounded log files.
//!
//! Every log file a redoor process owns (its own logger output and the forwarded output
//! of managed agents and relay SSH sessions) is written through [`RotatingLogFile`], so a
//! long-running server or agent on a small device cannot slowly fill the disk.

use std::{
    io,
    path::{Path, PathBuf},
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::RwLock,
};

/// Size a log file may reach before its older half is dropped when no limit is configured.
pub const DEFAULT_LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// Written messages between size checks; checking on every write would add a metadata
/// syscall to the logging hot path for a limit that only matters over long periods.
const ROTATION_CHECK_INTERVAL: u32 = 100;
/// Chunk used while scanning and compacting a file, so truncation never holds the whole
/// log in memory.
const COPY_CHUNK_BYTES: usize = 64 * 1024;
/// Longest forwarded child output line; longer lines are written in pieces so a child
/// that never prints a newline cannot grow server memory.
const MAX_FORWARDED_LINE_BYTES: usize = 64 * 1024;

/// Serializes truncation against appends inside this process.
///
/// A relay writes its own logger output and its SSH session output to the same file
/// through separate [`RotatingLogFile`]s, so a truncation must not lose a line appended
/// by another writer between copying the newer half and cutting the file. Writers take
/// the shared side and truncation the exclusive side; truncation is rare, so one
/// process-wide lock is enough.
static ROTATION_LOCK: RwLock<()> = RwLock::const_new(());

/// Where a log file lives and how large it may grow.
///
/// Kept separate from the open [`RotatingLogFile`] so configuration and spawners can pass
/// it around and open it only where the writes happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFileSpec {
    /// Log file path; parent directories are created on open.
    pub path: PathBuf,
    /// Size above which the older half of the lines is dropped.
    pub max_bytes: u64,
}

impl LogFileSpec {
    /// Applies the default limit here so every role resolves an unset limit the same way.
    pub fn new(path: impl Into<PathBuf>, max_bytes: Option<u64>) -> Self {
        Self {
            path: path.into(),
            max_bytes: max_bytes.unwrap_or(DEFAULT_LOG_MAX_BYTES),
        }
    }

    /// Opens the file for appending and applies the startup size check, so a log that
    /// grew past its limit before a restart shrinks before new output is added.
    ///
    /// The error names the path because callers show it to operators unchanged.
    pub async fn open(&self) -> io::Result<RotatingLogFile> {
        if let Some(parent) = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "Failed to create log directory '{}': {error}",
                        parent.display()
                    ),
                )
            })?;
        }
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("Failed to open log file '{}': {error}", self.path.display()),
                )
            })?;
        let log_file = RotatingLogFile {
            spec: self.clone(),
            file,
            messages_since_check: 0,
        };
        log_file.rotate_if_needed().await;
        Ok(log_file)
    }
}

/// An append-only log file that keeps itself under its size limit.
///
/// Truncation rewrites the file in place instead of renaming it, so every open append
/// handle (including other writers of the same file) keeps writing to the live log.
pub struct RotatingLogFile {
    /// Path and limit the file was opened with.
    spec: LogFileSpec,
    /// Append handle; appends land at the current end even after a truncation.
    file: tokio::fs::File,
    /// Messages written since the last size check, counted in memory so checks cost
    /// nothing between intervals.
    messages_since_check: u32,
}

impl RotatingLogFile {
    /// Path shown in diagnostics about this file.
    pub fn path(&self) -> &Path {
        &self.spec.path
    }

    /// Appends one complete message and checks the size every [`ROTATION_CHECK_INTERVAL`]
    /// messages.
    ///
    /// Only the write error is returned: a failed truncation leaves a working, merely
    /// oversized log, which must not make callers stop logging.
    pub async fn write_message(&mut self, bytes: &[u8]) -> io::Result<()> {
        {
            let _shared = ROTATION_LOCK.read().await;
            self.file.write_all(bytes).await?;
            // tokio completes file writes in the background; flushing makes the bytes
            // visible to a truncation or a diagnostic reader as soon as this returns.
            self.file.flush().await?;
        }
        self.messages_since_check += 1;
        if self.messages_since_check >= ROTATION_CHECK_INTERVAL {
            self.messages_since_check = 0;
            self.rotate_if_needed().await;
        }
        Ok(())
    }

    /// Drops the older half of the lines when the file is over its limit.
    ///
    /// Reports failures on stderr rather than through the logger, because this file may
    /// be the logger's own output.
    async fn rotate_if_needed(&self) {
        let _exclusive = ROTATION_LOCK.write().await;
        let result = match tokio::fs::metadata(&self.spec.path).await {
            Ok(metadata) if metadata.len() > self.spec.max_bytes => self.keep_newer_half().await,
            Ok(_) => Ok(()),
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            eprintln!(
                "Failed to rotate log file '{}': {error}",
                self.spec.path.display()
            );
        }
    }

    /// Moves the newer half of the lines to the start of the file and cuts the rest.
    ///
    /// Streams in fixed chunks (two scans and one copy) so devices with little memory
    /// can rotate a large log. A trailing line without a newline counts as a line, and a
    /// single-line file is emptied so the limit holds even for output without newlines.
    async fn keep_newer_half(&self) -> io::Result<()> {
        let mut file = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.spec.path)
            .await?;
        let (newlines, ends_with_newline) = Self::count_newlines(&mut file).await?;
        let length = file.metadata().await?.len();
        let lines = newlines + u64::from(length > 0 && !ends_with_newline);
        let dropped_lines = lines - lines / 2;
        let keep_from = Self::offset_after_newlines(&mut file, dropped_lines, length).await?;
        let kept = Self::move_to_start(&mut file, keep_from).await?;
        file.set_len(kept).await?;
        file.flush().await
    }

    /// Counts newline bytes and reports whether the file ends with one.
    async fn count_newlines(file: &mut tokio::fs::File) -> io::Result<(u64, bool)> {
        file.seek(io::SeekFrom::Start(0)).await?;
        let mut buffer = vec![0_u8; COPY_CHUNK_BYTES];
        let mut newlines = 0_u64;
        let mut ends_with_newline = false;
        loop {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                return Ok((newlines, ends_with_newline));
            }
            let chunk = &buffer[..read];
            newlines += chunk.iter().filter(|byte| **byte == b'\n').count() as u64;
            ends_with_newline = chunk.last() == Some(&b'\n');
        }
    }

    /// Returns the offset just after the `count`th newline, or `length` when the file
    /// has fewer newlines (the dropped half then includes the trailing partial line).
    async fn offset_after_newlines(
        file: &mut tokio::fs::File,
        count: u64,
        length: u64,
    ) -> io::Result<u64> {
        if count == 0 {
            return Ok(0);
        }
        file.seek(io::SeekFrom::Start(0)).await?;
        let mut buffer = vec![0_u8; COPY_CHUNK_BYTES];
        let mut offset = 0_u64;
        let mut remaining = count;
        loop {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                return Ok(length);
            }
            for (index, byte) in buffer[..read].iter().enumerate() {
                if *byte == b'\n' {
                    remaining -= 1;
                    if remaining == 0 {
                        return Ok(offset + index as u64 + 1);
                    }
                }
            }
            offset += read as u64;
        }
    }

    /// Copies everything from `from` to the end of the file to its start and returns the
    /// copied length; the write position never passes the read position, so the copy is
    /// safe within one file.
    async fn move_to_start(file: &mut tokio::fs::File, from: u64) -> io::Result<u64> {
        let mut buffer = vec![0_u8; COPY_CHUNK_BYTES];
        let mut read_at = from;
        let mut write_at = 0_u64;
        loop {
            file.seek(io::SeekFrom::Start(read_at)).await?;
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                return Ok(write_at);
            }
            file.seek(io::SeekFrom::Start(write_at)).await?;
            file.write_all(&buffer[..read]).await?;
            file.flush().await?;
            read_at += read as u64;
            write_at += read as u64;
        }
    }

    /// Forwards a child's stdout and stderr into this file line by line until both
    /// close, and returns the last `tail_limit` bytes of that output.
    ///
    /// Children used to write straight into the log through an inherited descriptor,
    /// which nothing could keep bounded. Forwarding lets each line count toward the
    /// rotation interval, and the returned tail lets supervisors explain a failed exit
    /// without rereading a file that may have been truncated meanwhile. Write failures
    /// are reported once and the pipes are still drained, so the child never blocks.
    pub async fn forward_child_output<O, E>(
        mut self,
        stdout: Option<O>,
        stderr: Option<E>,
        tail_limit: usize,
    ) -> Vec<u8>
    where
        O: AsyncRead + Unpin,
        E: AsyncRead + Unpin,
    {
        let mut stdout = LineSplitter::new(stdout);
        let mut stderr = LineSplitter::new(stderr);
        let mut forwarded = ForwardedOutput::new(tail_limit);
        loop {
            let line = tokio::select! {
                line = stdout.next_line(), if stdout.is_open() => line,
                line = stderr.next_line(), if stderr.is_open() => line,
                else => break,
            };
            if let Some(line) = line {
                forwarded.write(&mut self, &line).await;
            }
        }
        forwarded.tail
    }
}

/// Bookkeeping of one [`RotatingLogFile::forward_child_output`] run.
struct ForwardedOutput {
    /// Final bytes of the output, bounded by `tail_limit`.
    tail: Vec<u8>,
    /// Size bound of `tail`.
    tail_limit: usize,
    /// Set after the first write error so a broken disk is reported once, not per line.
    write_failed: bool,
}

impl ForwardedOutput {
    /// Starts with an empty tail.
    fn new(tail_limit: usize) -> Self {
        Self {
            tail: Vec::new(),
            tail_limit,
            write_failed: false,
        }
    }

    /// Appends one line to the log and the bounded tail.
    async fn write(&mut self, log_file: &mut RotatingLogFile, line: &[u8]) {
        if !self.write_failed
            && let Err(error) = log_file.write_message(line).await
        {
            self.write_failed = true;
            crate::log!(
                redoor_protocol::wire::log::Level::Warning,
                "Failed to write child output to log file '{}': {error}",
                log_file.path().display()
            );
        }
        self.tail.extend_from_slice(line);
        if self.tail.len() > self.tail_limit {
            let excess = self.tail.len() - self.tail_limit;
            self.tail.drain(..excess);
        }
    }
}

/// Splits one child pipe into lines with a bounded buffer.
///
/// Whole lines keep stdout and stderr from interleaving mid-line in the shared log and
/// give the rotation interval a meaningful unit.
struct LineSplitter<R> {
    /// Pipe still being read; `None` once it reached EOF, failed, or was never piped.
    reader: Option<R>,
    /// Bytes read but not yet returned as a line.
    pending: Vec<u8>,
}

impl<R: AsyncRead + Unpin> LineSplitter<R> {
    /// Accepts an absent pipe so callers can treat unpiped streams like closed ones.
    fn new(reader: Option<R>) -> Self {
        Self {
            reader,
            pending: Vec::new(),
        }
    }

    /// Whether another line may still arrive.
    fn is_open(&self) -> bool {
        self.reader.is_some() || !self.pending.is_empty()
    }

    /// Returns the next newline-terminated line, a piece of an over-long line, or the
    /// final unterminated bytes at EOF; `None` means nothing is left.
    ///
    /// Cancel safe, as `tokio::select!` requires: the only await is a single `read`, and
    /// bytes are buffered only after it completes.
    async fn next_line(&mut self) -> Option<Vec<u8>> {
        loop {
            if let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
                return Some(self.pending.drain(..=end).collect());
            }
            if self.pending.len() >= MAX_FORWARDED_LINE_BYTES {
                return Some(self.pending.drain(..MAX_FORWARDED_LINE_BYTES).collect());
            }
            let Some(reader) = self.reader.as_mut() else {
                return (!self.pending.is_empty()).then(|| std::mem::take(&mut self.pending));
            };
            let mut chunk = [0_u8; 8 * 1024];
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => self.reader = None,
                Ok(read) => self.pending.extend_from_slice(&chunk[..read]),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    /// Writes `count` numbered lines so tests can tell which half survived.
    fn numbered_lines(count: usize) -> String {
        (0..count)
            .map(|index| format!("line-{index:04}\n"))
            .collect()
    }

    /// Opening an oversized log keeps exactly its newer half of lines.
    #[tokio::test]
    async fn startup_check_keeps_the_newer_half_of_lines() {
        let temp_dir = TempDir::create();
        let path = temp_dir.path().join("startup.log");
        tokio::fs::write(&path, numbered_lines(1_000))
            .await
            .expect("seed log");

        LogFileSpec::new(&path, Some(1_024))
            .open()
            .await
            .expect("open log");

        let content = tokio::fs::read_to_string(&path).await.expect("read log");
        // Truncation is by line count: half of the 1000 lines remain.
        assert_eq!(content.lines().count(), 500);
        // The oldest lines are dropped so the most recent history is kept.
        assert!(content.starts_with("line-0500\n"));
        // The newest line survives intact, including its newline.
        assert!(content.ends_with("line-0999\n"));
    }

    /// A log under its limit is left untouched on open.
    #[tokio::test]
    async fn startup_check_ignores_logs_under_the_limit() {
        let temp_dir = TempDir::create();
        let path = temp_dir.path().join("small.log");
        tokio::fs::write(&path, numbered_lines(10))
            .await
            .expect("seed log");

        LogFileSpec::new(&path, None)
            .open()
            .await
            .expect("open log");

        // Logs under the default limit must keep their full history.
        assert_eq!(
            tokio::fs::read_to_string(&path).await.expect("read log"),
            numbered_lines(10)
        );
    }

    /// The size is rechecked after every hundred writes, not after each one.
    #[tokio::test]
    async fn size_is_checked_every_hundred_messages() {
        let temp_dir = TempDir::create();
        let path = temp_dir.path().join("counted.log");
        let mut log_file = LogFileSpec::new(&path, Some(100))
            .open()
            .await
            .expect("open log");

        for index in 0..99 {
            log_file
                .write_message(format!("line-{index:04}\n").as_bytes())
                .await
                .expect("write line");
        }
        let before = tokio::fs::read_to_string(&path).await.expect("read log");
        // Writes between checks must not pay for a size check or truncation.
        assert_eq!(before.lines().count(), 99);

        log_file
            .write_message(b"line-0099\n")
            .await
            .expect("write hundredth line");
        let after = tokio::fs::read_to_string(&path).await.expect("read log");
        // The hundredth write triggers the check and halves the oversized file.
        assert_eq!(after.lines().count(), 50);
        // The newer half is the one kept.
        assert!(after.starts_with("line-0050\n"));

        log_file
            .write_message(b"after-truncation\n")
            .await
            .expect("write after truncation");
        let appended = tokio::fs::read_to_string(&path).await.expect("read log");
        // The append handle keeps writing at the new end instead of leaving a hole.
        assert!(appended.ends_with("line-0099\nafter-truncation\n"));
        // No gap of zero bytes may appear where the old end used to be.
        assert!(!appended.contains('\0'));
    }

    /// A trailing partial line counts as a line and output without newlines is bounded.
    #[tokio::test]
    async fn partial_and_single_lines_are_handled() {
        let temp_dir = TempDir::create();
        let path = temp_dir.path().join("partial.log");
        tokio::fs::write(&path, "a\nb\nc\npartial")
            .await
            .expect("seed log");
        LogFileSpec::new(&path, Some(1))
            .open()
            .await
            .expect("open log");
        // Four lines including the unterminated one leave the newest two.
        assert_eq!(
            tokio::fs::read_to_string(&path).await.expect("read log"),
            "c\npartial"
        );

        tokio::fs::write(&path, "x".repeat(4_096))
            .await
            .expect("seed single line");
        LogFileSpec::new(&path, Some(1))
            .open()
            .await
            .expect("reopen log");
        // A single oversized line cannot be halved, so it is dropped to honor the limit.
        assert_eq!(tokio::fs::metadata(&path).await.expect("metadata").len(), 0);
    }

    /// Child output is forwarded as whole lines and its tail is returned.
    #[tokio::test]
    async fn child_output_is_forwarded_and_tail_returned() {
        let temp_dir = TempDir::create();
        let path = temp_dir.path().join("child.log");
        let log_file = LogFileSpec::new(&path, None)
            .open()
            .await
            .expect("open log");

        let tail = log_file
            .forward_child_output(
                Some(&b"out-1\nout-2\nunterminated"[..]),
                Some(&b"err-1\n"[..]),
                12,
            )
            .await;

        let content = tokio::fs::read_to_string(&path).await.expect("read log");
        // Every line of both streams reaches the log.
        for line in ["out-1\n", "out-2\n", "err-1\n"] {
            assert!(content.contains(line), "missing {line:?} in {content:?}");
        }
        // Output that ends without a newline is still persisted.
        assert!(content.contains("unterminated"));
        // The returned diagnostic is bounded to the requested tail size.
        assert!(tail.len() <= 12);
    }

    /// Over-long lines are split so memory stays bounded.
    #[tokio::test]
    async fn over_long_lines_are_split() {
        let input = vec![b'x'; MAX_FORWARDED_LINE_BYTES + 10];
        let mut splitter = LineSplitter::new(Some(input.as_slice()));

        let first = splitter.next_line().await.expect("first piece");
        let second = splitter.next_line().await.expect("second piece");

        // The buffer never grows past the line bound.
        assert_eq!(first.len(), MAX_FORWARDED_LINE_BYTES);
        // The remainder is delivered at EOF instead of being lost.
        assert_eq!(second.len(), 10);
        // Nothing is left once both pieces are returned.
        assert!(splitter.next_line().await.is_none());
    }
}
