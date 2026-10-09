//! Runs pipe-based processes independently of the control mailbox and bounds every output read.

use crate::commands::ctx::Reply;
use crate::runtime::cancel::Registration;
use crate::transfers::stream::StreamWriter;
use redoor_protocol::{
    ids::RequestId,
    wire::{
        command::CommandResult,
        exec::{ExecEvent, ExecRequest},
    },
};
use std::{os::unix::process::ExitStatusExt, process::Stdio};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

/// Owns the process group so disconnect, timeout and dropped tasks also stop descendants.
struct ProcessGroup(i32);

impl Drop for ProcessGroup {
    /// Group cleanup is synchronous signal delivery, never a blocking process wait.
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.0),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

/// Holds stream ordering while each event remains small enough for one bounded HTTP record.
struct ExecWorker {
    stream: StreamWriter,
}

impl ExecWorker {
    /// Serializes one bounded event rather than collecting process output.
    async fn send(&mut self, event: ExecEvent, last: bool) -> anyhow::Result<()> {
        let mut data = serde_json::to_vec(&event)?;
        data.push(b'\n');
        anyhow::ensure!(
            self.stream.send(&data, last).await,
            "Execution output connection closed"
        );
        Ok(())
    }

    /// Reads both pipes fairly and waits for their EOF before reporting exit, retaining trailing bytes.
    async fn output(&mut self, child: &mut tokio::process::Child) -> anyhow::Result<ExecEvent> {
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Missing stdout pipe"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Missing stderr pipe"))?;
        let mut out = [0u8; 8192];
        let mut err = [0u8; 8192];
        let (mut out_open, mut err_open) = (true, true);
        while out_open || err_open {
            let event = tokio::select! {
                count = stdout.read(&mut out), if out_open => (true, count?),
                count = stderr.read(&mut err), if err_open => (false, count?),
            };
            match event {
                (true, 0) => out_open = false,
                (false, 0) => err_open = false,
                (true, count) => {
                    self.send(
                        ExecEvent::Stdout {
                            data: out[..count].to_vec(),
                        },
                        false,
                    )
                    .await?
                }
                (false, count) => {
                    self.send(
                        ExecEvent::Stderr {
                            data: err[..count].to_vec(),
                        },
                        false,
                    )
                    .await?
                }
            }
        }
        let status = child.wait().await?;
        Ok(ExecEvent::Exit {
            code: status.code(),
            signal: status.signal(),
        })
    }

    /// Keeps cancellation/deadlines outside pipe writes so backpressure cannot hide control requests.
    async fn execute(
        &mut self,
        request: ExecRequest,
        cancel: &CancellationToken,
    ) -> anyhow::Result<ExecEvent> {
        request.validate().map_err(anyhow::Error::msg)?;
        if cancel.is_cancelled() {
            return Ok(ExecEvent::Canceled);
        }
        let mut command = tokio::process::Command::new(&request.argv[0]);
        command
            .args(&request.argv[1..])
            .envs(request.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        if let Some(cwd) = request.cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn()?;
        let group = ProcessGroup(
            child
                .id()
                .ok_or_else(|| anyhow::anyhow!("Missing process ID"))? as i32,
        );
        let deadline = async {
            match request.timeout_ms {
                Some(ms) => tokio::time::sleep(std::time::Duration::from_millis(ms)).await,
                None => std::future::pending::<()>().await,
            }
        };
        let event = tokio::select! {
            result = self.output(&mut child) => result,
            _ = cancel.cancelled() => Ok(ExecEvent::Canceled),
            _ = deadline => Ok(ExecEvent::TimedOut),
        };
        drop(group);
        // Reap the direct child asynchronously after group termination, including cancellation paths.
        let _ = child.wait().await;
        event
    }

    /// Preserves normal completion under backpressure without retaining workers during teardown.
    async fn terminal(
        &mut self,
        event: ExecEvent,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        if matches!(event, ExecEvent::Canceled | ExecEvent::TimedOut) {
            // These paths must finish so generation shutdown can join the worker even with a full lane.
            return tokio::time::timeout(std::time::Duration::from_secs(5), self.send(event, true))
                .await?;
        }
        anyhow::ensure!(
            !cancel.is_cancelled(),
            "Execution canceled before terminal delivery"
        );
        tokio::select! {
            biased;
            _ = cancel.cancelled() => anyhow::bail!("Execution canceled during terminal delivery"),
            result = self.send(event, true) => result,
        }
    }
}

/// Emits a terminal event even for spawn failures; generation teardown drops the process guard.
///
/// When the payload lane cannot deliver completion, the reply asks the dispatcher to close
/// server ownership on the control lane instead, retained until the registry generation (and
/// with it the connection that would carry it) is torn down.
pub(crate) async fn run(
    request: ExecRequest,
    stream: StreamWriter,
    registration: Registration<RequestId>,
) -> Reply {
    let cancel = registration.token().clone();
    let abandon = registration.revoked().clone();
    let mut worker = ExecWorker { stream };
    let event = worker
        .execute(request, &cancel)
        .await
        .unwrap_or_else(|error| ExecEvent::Error {
            message: error.to_string(),
        });
    let delivered = worker.terminal(event.clone(), &cancel).await.is_ok();
    drop(registration);
    if delivered {
        Reply::Streamed
    } else {
        Reply::Fallback {
            result: CommandResult::Exec { event },
            abandon,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::cancel::CancelRegistry;
    use crate::runtime::responder::Responder;
    use futures_util::poll;
    use redoor_protocol::wire::{
        command::CommandErrorKind,
        stream::{StreamChunk, StreamPayloadKind},
    };
    use std::time::Duration;
    use tokio::sync::mpsc;
    use tokio_tungstenite::tungstenite::protocol::Message;

    /// Uses a real nonzero exit so delivery cannot accidentally substitute a local success or failure.
    fn exit_request() -> ExecRequest {
        ExecRequest {
            argv: vec!["sh".into(), "-c".into(), "exit 37".into()],
            cwd: None,
            env: Default::default(),
            timeout_ms: None,
        }
    }

    /// Saturates the actual framed-payload sender without involving socket buffer sizes or wall time.
    async fn blocked_stream() -> (StreamWriter, mpsc::Receiver<Message>) {
        let (write, receiver) = mpsc::channel(1);
        write.send(Message::binary(vec![0])).await.unwrap();
        (
            StreamWriter::new(write, RequestId::new(41), StreamPayloadKind::RawFile),
            receiver,
        )
    }

    /// A completed and reaped process must retain its remote status for arbitrarily slow consumers.
    #[tokio::test]
    async fn normal_exit_waits_beyond_five_seconds_and_preserves_control_lane() {
        let (stream, mut payload) = blocked_stream().await;
        let mut worker = ExecWorker { stream };
        let cancel = CancellationToken::new();
        let event = worker.execute(exit_request(), &cancel).await.unwrap();
        // This also synchronizes with child reaping before testing terminal-only backpressure.
        assert!(matches!(
            event,
            ExecEvent::Exit {
                code: Some(37),
                signal: None
            }
        ));
        tokio::time::pause();
        let terminal = worker.terminal(event, &cancel);
        tokio::pin!(terminal);
        // Polling installs the blocked enqueue before advancing virtual time.
        assert!(poll!(&mut terminal).is_pending());
        tokio::time::advance(Duration::from_secs(60)).await;
        // Normal completion has no five-second fallback (or other delivery deadline).
        assert!(poll!(&mut terminal).is_pending());

        let (responder, mut responses) = Responder::recording(RequestId::new(42), 1);
        responder
            .respond(CommandResult::error(
                CommandErrorKind::ServiceUnavailable,
                "independent response",
            ))
            .await
            .unwrap();
        // Control delivery remains independent while the shared binary lane is full.
        assert!(responses.try_recv().is_ok());
        payload.recv().await.unwrap();
        terminal.await.unwrap();
        let Message::Binary(bytes) = payload.recv().await.unwrap() else {
            panic!("Expected framed terminal event");
        };
        let chunk = StreamChunk::from_bytes(&bytes).unwrap();
        // Resuming the consumer must deliver the last frame and the actual nonzero remote exit.
        assert!(chunk.is_last);
        assert!(matches!(
            serde_json::from_slice::<ExecEvent>(&chunk.data).unwrap(),
            ExecEvent::Exit {
                code: Some(37),
                signal: None
            }
        ));
    }

    /// Cancellation, teardown and transfer disconnection must release a normal terminal enqueue.
    #[tokio::test(start_paused = true)]
    async fn blocked_normal_terminal_observes_cancel_and_disconnect() {
        for mode in 0..3 {
            let (stream, payload) = blocked_stream().await;
            let mut worker = ExecWorker { stream };
            let registry = CancelRegistry::new(None);
            let registration = registry.register(RequestId::new(41)).unwrap();
            let cancel = registration.token().clone();
            let terminal = worker.terminal(
                ExecEvent::Exit {
                    code: Some(37),
                    signal: None,
                },
                &cancel,
            );
            tokio::pin!(terminal);
            // All cases start blocked on the same saturated lane.
            assert!(poll!(&mut terminal).is_pending());
            match mode {
                0 => registration.cancel(),
                1 => registry.revoke_all(), // Generation teardown revokes every registration.
                _ => drop(payload),         // Transfer disconnection closes the payload sender.
            }
            // No clock advancement is needed to release cancellation/disconnection.
            assert!(matches!(
                poll!(&mut terminal),
                std::task::Poll::Ready(Err(_))
            ));
        }
    }

    /// An already-cancelled token must not be lost when execute hands ownership back to run.
    #[tokio::test(start_paused = true)]
    async fn observed_cancel_releases_terminal() {
        let (stream, _payload) = blocked_stream().await;
        let mut worker = ExecWorker { stream };
        let cancel = CancellationToken::new();
        cancel.cancel();
        // The already cancelled state still cancels delivery.
        assert!(
            worker
                .terminal(
                    ExecEvent::Exit {
                        code: Some(37),
                        signal: None
                    },
                    &cancel
                )
                .await
                .is_err()
        );
    }

    /// A closed payload lane must hand the real terminal outcome to the control fallback.
    #[tokio::test]
    async fn closed_payload_lane_falls_back_with_the_actual_error() {
        let (write, payload) = mpsc::channel(1);
        drop(payload);
        let registry = CancelRegistry::new(None);
        let registration = registry.register(RequestId::new(41)).unwrap();
        let invalid = ExecRequest {
            argv: vec![],
            cwd: None,
            env: Default::default(),
            timeout_ms: None,
        };

        let reply = run(
            invalid,
            StreamWriter::new(write, RequestId::new(41), StreamPayloadKind::RawFile),
            registration,
        )
        .await;

        let Reply::Fallback { result, abandon } = reply else {
            panic!("undeliverable completion must fall back to the control lane");
        };
        // Preserve the real validation failure rather than losing completion or inventing Canceled.
        assert!(matches!(
            result,
            CommandResult::Exec {
                event: ExecEvent::Error { .. }
            }
        ));
        // Only teardown of the generation may abandon the retained acknowledgement.
        assert!(!abandon.is_cancelled());
        registry.revoke_all();
        assert!(abandon.is_cancelled());
    }

    /// A canceled request with a blocked payload lane is bounded and then falls back.
    #[tokio::test(start_paused = true)]
    async fn canceled_terminal_is_bounded_before_falling_back() {
        let (stream, _payload) = blocked_stream().await;
        let registry = CancelRegistry::new(None);
        let registration = registry.register(RequestId::new(41)).unwrap();
        registration.cancel();
        let task = run(exit_request(), stream, registration);
        tokio::pin!(task);

        // An already canceled request takes the bounded terminal path without spawning a process.
        assert!(poll!(&mut task).is_pending());
        tokio::time::advance(Duration::from_secs(6)).await;
        let std::task::Poll::Ready(reply) = poll!(&mut task) else {
            panic!("canceled terminal delivery must be bounded");
        };
        // The fallback must name the cancellation so the router can close ownership.
        assert!(matches!(
            reply,
            Reply::Fallback {
                result: CommandResult::Exec {
                    event: ExecEvent::Canceled
                },
                ..
            }
        ));
    }
}
