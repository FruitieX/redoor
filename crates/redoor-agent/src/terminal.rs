//! Interactive terminal sessions: one PTY bridged to one stream of the agent connection.
//!
//! PTY output travels on the interactive lane, which the connection writer drains before bulk
//! transfer data, so keystroke echoes never queue behind a download. Both directions are
//! credited with a terminal-sized window.

use crate::runtime::responder::{ControlSender, OutboundFrame};
use crate::transfers::stream::{OutputCredits, StreamWriter};
use anyhow::{Context, Result, anyhow};
use nix::{
    errno::Errno,
    sys::signal::{Signal, kill, killpg},
    unistd::Pid,
};
use redoor_core::{log, stream_credit::CreditLedger};
use redoor_protocol::{
    ids::RequestId,
    wire::{
        AgentToServer,
        log::Level,
        stream::{StreamChunk, TERMINAL_CREDIT_WINDOW_FRAMES, receive_queue_frames},
        terminal::{TerminalServerMessage, TerminalSize},
    },
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::{ffi::OsString, os::unix::process::ExitStatusExt, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Child,
    sync::mpsc,
    task::JoinHandle,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

/// PTY read size; small enough to keep interactive output latency low.
const PTY_BUFFER_SIZE: usize = 8 * 1024;
/// Pending resizes per terminal; resizes are rare, and a full queue only drops a stale size.
const RESIZE_QUEUE_SIZE: usize = 4;
/// How long teardown waits for the shell or a bridge task before escalating, so a stuck
/// session cannot hold resources indefinitely.
const PROCESS_EXIT_GRACE: Duration = Duration::from_secs(2);

/// Input queues of one terminal, fed by the connection reader.
struct TerminalRoute {
    input: mpsc::Sender<StreamChunk>,
    resize: mpsc::Sender<TerminalSize>,
}

/// Routes inbound terminal input frames and resizes to their PTY sessions.
///
/// Shared by the connection reader (frames), the runtime (resizes) and the sessions, so input
/// never passes through the runtime mailbox.
#[derive(Clone, Default)]
pub(crate) struct TerminalInputs {
    routes: Arc<Mutex<HashMap<RequestId, TerminalRoute>>>,
}

/// Input of one terminal; dropping it unroutes the stream.
pub(crate) struct TerminalInput {
    inputs: TerminalInputs,
    stream: RequestId,
    pub(crate) frames: mpsc::Receiver<StreamChunk>,
    pub(crate) resizes: mpsc::Receiver<TerminalSize>,
}

impl Drop for TerminalInput {
    /// Unroutes the stream so late frames are dropped instead of queued for nobody.
    fn drop(&mut self) {
        self.inputs.table().remove(&self.stream);
    }
}

impl TerminalInputs {
    /// Locks the table, tolerating poison because every operation leaves it consistent.
    fn table(&self) -> std::sync::MutexGuard<'_, HashMap<RequestId, TerminalRoute>> {
        self.routes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Routes stream `stream` to a new terminal.
    pub(crate) fn open(&self, stream: RequestId) -> TerminalInput {
        let (input, frames) = mpsc::channel(receive_queue_frames(TERMINAL_CREDIT_WINDOW_FRAMES));
        let (resize, resizes) = mpsc::channel(RESIZE_QUEUE_SIZE);
        self.table().insert(stream, TerminalRoute { input, resize });
        TerminalInput {
            inputs: self.clone(),
            stream,
            frames,
            resizes,
        }
    }

    /// Hands `chunk` to its terminal, or returns it when the stream is not a terminal.
    ///
    /// The server sends at most its credit window, so a full queue is a protocol violation;
    /// it unroutes only that terminal, whose session then ends.
    pub(crate) fn deliver(&self, chunk: StreamChunk) -> Option<StreamChunk> {
        let mut table = self.table();
        let Some(route) = table.get(&chunk.request_id) else {
            return Some(chunk);
        };
        let stream = chunk.request_id;
        if let Err(mpsc::error::TrySendError::Full(_)) = route.input.try_send(chunk) {
            log!(
                Level::Warning,
                "Terminal input exceeded its flow-control credit: stream={}",
                stream
            );
            table.remove(&stream);
        }
        None
    }

    /// Queues a resize for terminal `stream`; resizes for unknown terminals are ignored.
    pub(crate) fn resize(&self, stream: RequestId, size: TerminalSize) {
        if let Some(route) = self.table().get(&stream) {
            let _ = route.resize.try_send(size);
        }
    }
}

/// Connection resources one terminal session uses.
pub(crate) struct TerminalLink {
    pub(crate) stream: RequestId,
    /// Interactive lane for output frames and lifecycle events, in one order.
    pub(crate) interactive: ControlSender,
    /// Control lane for input credit grants.
    pub(crate) control: ControlSender,
    pub(crate) credits: OutputCredits,
    pub(crate) input: TerminalInput,
}

/// Why the session ended first.
enum SessionEnd {
    Cancelled,
    Child(std::process::ExitStatus),
    /// PTY EOF; carries the writer back because its task must not be polled again.
    OutputEnded(Option<StreamWriter>),
    InputEnded(Result<()>),
}

/// Runs one terminal until its shell exits, the server cancels it or the connection ends.
///
/// The stream always ends with FIN after the last output and lifecycle event, so the server
/// knows no frame of it follows.
pub(crate) async fn run(
    link: TerminalLink,
    size: TerminalSize,
    cwd: String,
    cancel: CancellationToken,
) -> Result<()> {
    size.validate()
        .map_err(|error| anyhow!(error.to_string()))?;
    let TerminalLink {
        stream,
        interactive,
        control,
        credits,
        input,
    } = link;
    let mut writer = StreamWriter::new(
        interactive,
        stream,
        credits.register_with_window(stream, TERMINAL_CREDIT_WINDOW_FRAMES),
    );
    let (reader, pty_writer, mut child, process_group_id) = match start_pty(size, &cwd) {
        Ok(started) => started,
        Err(error) => {
            send_event(
                &mut writer,
                stream,
                TerminalServerMessage::Error {
                    message: "Failed to start terminal".to_string(),
                },
            )
            .await;
            writer.send(&[], true).await;
            return Err(error);
        }
    };
    log!(
        Level::Info,
        "Terminal created: stream={}, cwd={}",
        stream,
        cwd
    );
    send_event(&mut writer, stream, TerminalServerMessage::Ready).await;

    let mut output = tokio::spawn(forward_output(reader, writer));
    let mut input_task = tokio::spawn(apply_input(input, pty_writer, control, stream));
    let end = wait_for_end(&mut child, &cancel, &mut output, &mut input_task).await;

    let (status, finished_writer, result) = match end {
        SessionEnd::Child(status) => (Some(status), None, Ok(())),
        SessionEnd::OutputEnded(writer) => (
            timeout(Duration::from_millis(100), child.wait())
                .await
                .ok()
                .and_then(Result::ok),
            Some(writer),
            Ok(()),
        ),
        SessionEnd::Cancelled => (None, None, Ok(())),
        SessionEnd::InputEnded(result) => (None, None, result),
    };
    teardown_process_group(&mut child, process_group_id).await;
    input_task.abort();
    // The PTY closes with the shell, so the output task drains the remaining output and
    // hands the writer back; the exit status then follows that output on the same lane.
    let writer = match finished_writer {
        Some(writer) => writer,
        None => timeout(PROCESS_EXIT_GRACE, &mut output)
            .await
            .ok()
            .and_then(Result::ok),
    };
    if let Some(mut writer) = writer {
        if let Some(status) = status.filter(|_| !cancel.is_cancelled()) {
            send_event(
                &mut writer,
                stream,
                TerminalServerMessage::Exit {
                    code: status.code(),
                    signal: status.signal(),
                },
            )
            .await;
        }
        writer.send(&[], true).await;
    } else {
        output.abort();
    }
    log!(Level::Info, "Terminal destroyed: stream={}", stream);
    result
}

/// Waits for the first terminal-ending event with small select arms.
async fn wait_for_end(
    child: &mut Child,
    cancel: &CancellationToken,
    output: &mut JoinHandle<StreamWriter>,
    input: &mut JoinHandle<Result<()>>,
) -> SessionEnd {
    tokio::select! {
        _ = cancel.cancelled() => SessionEnd::Cancelled,
        status = child.wait() => match status {
            Ok(status) => SessionEnd::Child(status),
            Err(error) => SessionEnd::InputEnded(Err(error.into())),
        },
        writer = &mut *output => SessionEnd::OutputEnded(writer.ok()),
        result = &mut *input => SessionEnd::InputEnded(
            result.context("terminal input task failed").and_then(|result| result),
        ),
    }
}

/// Sends one lifecycle event on the terminal's own lane, ordered with its output.
async fn send_event(writer: &mut StreamWriter, stream: RequestId, event: TerminalServerMessage) {
    writer
        .send_message(AgentToServer::TerminalEvent { stream, event })
        .await;
}

/// Copies PTY output into credited stream frames until EOF or a closed lane, then returns
/// the writer so the session can end the stream after the last output.
async fn forward_output(
    mut reader: pty_process::OwnedReadPty,
    mut writer: StreamWriter,
) -> StreamWriter {
    let mut buffer = vec![0; PTY_BUFFER_SIZE];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => return writer,
            Ok(read) => read,
        };
        if !writer.send(&buffer[..read], false).await {
            return writer;
        }
    }
}

/// Writes input frames to the PTY, applies resizes and returns credit for consumed input.
async fn apply_input(
    mut input: TerminalInput,
    mut pty: pty_process::OwnedWritePty,
    control: ControlSender,
    stream: RequestId,
) -> Result<()> {
    let mut ledger = CreditLedger::default();
    loop {
        let event = tokio::select! {
            size = input.resizes.recv() => size.map(InputEvent::Resize),
            chunk = input.frames.recv() => chunk.map(InputEvent::Frame),
        };
        match event {
            None => return Ok(()),
            Some(InputEvent::Resize(size)) => resize(&pty, size)?,
            Some(InputEvent::Frame(chunk)) => {
                if !write_input(&mut pty, &chunk).await? {
                    return Ok(());
                }
                if let Some(frames) = ledger.drained(chunk.consumes_credit()) {
                    let grant = AgentToServer::Credit { stream, frames };
                    let _ = control.send(OutboundFrame::message(grant)).await;
                }
            }
        }
    }
}

/// One piece of terminal input from the server.
enum InputEvent {
    Resize(TerminalSize),
    Frame(StreamChunk),
}

/// Writes one input frame; returns `false` once the server ended the input stream.
async fn write_input(pty: &mut pty_process::OwnedWritePty, chunk: &StreamChunk) -> Result<bool> {
    pty.write_all(&chunk.data)
        .await
        .context("failed to write PTY input")?;
    Ok(!chunk.is_last)
}

/// Validates a resize again before invoking the PTY ioctl.
fn resize(pty: &pty_process::OwnedWritePty, size: TerminalSize) -> Result<()> {
    size.validate()
        .map_err(|error| anyhow!(error.to_string()))?;
    pty.resize(pty_process::Size::new(size.rows, size.cols))
        .context("failed to resize PTY")
}

/// Allocates the PTY and starts the login shell in `cwd`.
fn start_pty(
    size: TerminalSize,
    cwd: &str,
) -> Result<(
    pty_process::OwnedReadPty,
    pty_process::OwnedWritePty,
    Child,
    i32,
)> {
    let (pty, pts) = pty_process::open().context("failed to allocate PTY")?;
    pty.resize(pty_process::Size::new(size.rows, size.cols))
        .context("failed to set initial PTY size")?;

    // Resolve the fallback through PATH because Android/Termux has no `/bin/sh`.
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("sh"));
    // Login mode restores the user's profile environment when the agent was started by
    // a service manager such as launchd, which otherwise supplies only a minimal PATH.
    let command = pty_process::Command::new(shell)
        .arg("-l")
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .current_dir(cwd)
        .kill_on_drop(true);
    let child = command
        .spawn(pts)
        .context("failed to spawn terminal shell")?;
    let process_group_id = child
        .id()
        .ok_or_else(|| anyhow!("terminal shell has no process id"))?;
    let process_group_id =
        i32::try_from(process_group_id).context("terminal process id is too large")?;
    let (reader, writer) = pty.into_split();
    Ok((reader, writer, child, process_group_id))
}

/// Sends escalating signals to every PTY session member and reaps its leader.
async fn teardown_process_group(child: &mut Child, process_group_id: i32) {
    signal_terminal_session(process_group_id, Signal::SIGHUP).await;
    let _ = timeout(PROCESS_EXIT_GRACE, child.wait()).await;
    signal_terminal_session(process_group_id, Signal::SIGKILL).await;
    let _ = child.wait().await;
}

/// Signals both the leader group and jobs moved into separate interactive process groups.
async fn signal_terminal_session(session_id: i32, signal: Signal) {
    signal_process_group(session_id, signal);
    for process_id in terminal_session_processes(session_id).await {
        match kill(process_id, signal) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(_) => {}
        }
    }
}

/// Enumerates Linux process metadata asynchronously so teardown never blocks the actor runtime.
async fn terminal_session_processes(session_id: i32) -> Vec<Pid> {
    let Ok(mut entries) = tokio::fs::read_dir("/proc").await else {
        return Vec::new();
    };
    let mut processes = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Some(process_id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        let Ok(stat) = tokio::fs::read_to_string(entry.path().join("stat")).await else {
            continue;
        };
        if process_session_id(&stat) == Some(session_id) {
            processes.push(Pid::from_raw(process_id));
        }
    }
    processes
}

/// Extracts the session field while allowing spaces and parentheses in a process name.
fn process_session_id(stat: &str) -> Option<i32> {
    stat.rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(3)?
        .parse()
        .ok()
}

/// Ignores a missing process group because it means teardown has already won the race.
fn signal_process_group(process_group_id: i32, signal: Signal) {
    match killpg(Pid::from_raw(process_group_id), signal) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::process_session_id;

    /// Verifies process names cannot shift the session field parsed from procfs.
    #[test]
    fn parses_proc_session_id_with_complex_process_name() {
        let stat = "123 (shell (worker)) S 1 123 456 0";

        // The sixth proc stat field identifies every process belonging to the PTY session.
        assert_eq!(process_session_id(stat), Some(456));
    }
}
