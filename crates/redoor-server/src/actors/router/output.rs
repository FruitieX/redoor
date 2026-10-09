//! Agent-to-consumer output streams: file downloads (with transfer history) and exec output.
//!
//! The router admits the stream and opens its inbound slot; frames then flow from the transfer
//! reader to the consumer's [`OutputReceiver`](super::streams::OutputReceiver) directly. The
//! router sees the stream again only for its terminal frame, a control-lane fallback result,
//! or cancellation.

use super::RouterError;
use super::flows::{Flow, FlowId, OutputFlow, OutputKind, OutputPhase};
use super::messages::{ExecuteStreamRequest, OutputStreamTracking, RouteResponse};
use super::progress::DownloadStartContext;
use super::state::{AgentConnection, RouterState};
use super::streams::{StreamEnd, StreamLease, StreamOutcome};
use redoor_core::log;
use redoor_protocol::{
    api::transfers::TransferProgressState,
    ids::RequestId,
    wire::{
        ServerToAgent,
        command::{Command, CommandResult},
        exec::ExecEvent,
        log::Level,
        policy::CommandLane,
    },
};
use std::sync::atomic::Ordering;

/// Message shown when an agent sends more frames than its credit allowed.
const CREDIT_VIOLATION: &str = "Agent exceeded the stream flow-control credit";

impl AgentConnection {
    /// Queues `cancel_transfer` on the priority lane, retaining it in a task when the lane is
    /// momentarily full so an admitted cancellation is never lost; a closed lane is an error
    /// the caller can report and retry.
    pub(crate) fn queue_priority_cancel(
        &self,
        request_id: RequestId,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, RouterError> {
        self.lanes
            .queue_priority(ServerToAgent::CancelTransfer { request_id })
            .map_err(|_| RouterError::ControlQueueFull {
                agent_id: self.agent_id.to_string(),
            })
    }
}

/// Parses an exec terminal frame; a malformed one still ends the execution visibly.
fn exec_event(payload: &[u8]) -> ExecEvent {
    serde_json::from_slice(payload).unwrap_or_else(|error| ExecEvent::Error {
        message: format!("Invalid execution acknowledgement: {error}"),
    })
}

impl RouterState {
    /// Admits an exec or download stream.
    ///
    /// Ownership is validated from the command policy before anything is admitted, so process
    /// output can never acquire file history. The inbound slot opens before the command is
    /// queued, so no frame can arrive unroutable.
    pub(crate) fn start_output(&mut self, request: ExecuteStreamRequest) {
        let ExecuteStreamRequest {
            agent_id,
            command,
            tracking,
            reply,
        } = request;
        let is_exec = matches!(command, Command::Exec { .. });
        if command.policy().lane != CommandLane::Output
            || is_exec != matches!(tracking, OutputStreamTracking::Execution { .. })
        {
            let _ = reply.send(Err(RouterError::UnexpectedResponseType {
                operation: "registering output command ownership",
            }));
            return;
        }
        let id = self.next_id();
        let Some(connection) = self.agents.by_id.get(&agent_id) else {
            let _ = reply.send(Err(RouterError::AgentNotFound {
                agent_id: agent_id.to_string(),
            }));
            return;
        };
        if let Err(error) = connection.transfer_connection() {
            let _ = reply.send(Err(error));
            return;
        }
        let lease = StreamLease::new(id, connection.link(), 0);
        let receiver = connection.streams.open_inbound(lease.clone());
        if !connection.send_message(ServerToAgent::Command {
            agent_id: agent_id.clone(),
            request_id: id,
            command,
        }) {
            connection.streams.close_inbound(id);
            let _ = reply.send(Err(RouterError::ControlQueueFull {
                agent_id: agent_id.to_string(),
            }));
            return;
        }
        let kind = match tracking {
            OutputStreamTracking::Download {
                path,
                total_bytes,
                full_size,
                resume_offset,
            } => {
                let progress_id = self.record_download_start(DownloadStartContext {
                    request_id: id,
                    agent_id: agent_id.clone(),
                    path,
                    total_bytes,
                    full_size,
                    resume_offset,
                });
                let base_bytes = self.transferred_bytes(progress_id).unwrap_or(0);
                // No frame is drained before the consumer receives its receiver below.
                lease.bytes.store(base_bytes, Ordering::Relaxed);
                OutputKind::Download {
                    progress_id,
                    base_bytes,
                }
            }
            OutputStreamTracking::Execution { terminal } => OutputKind::Execution { terminal },
        };
        self.flows.insert(
            id,
            Flow::Output(OutputFlow {
                request_id: id,
                agent_id: agent_id.clone(),
                kind,
                lease,
                phase: OutputPhase::Streaming,
            }),
        );
        if reply.send(Ok(receiver)).is_err() {
            self.cancel_transfer(id, agent_id);
        }
    }

    /// Settles an output stream whose terminal frame the transfer reader saw.
    pub(crate) fn finish_output_stream(&mut self, flow_id: FlowId, end: StreamEnd) {
        let Some(Flow::Output(flow)) = self.flows.get(flow_id) else {
            return;
        };
        if flow.agent_id != end.agent_id {
            return;
        }
        let Some(Flow::Output(flow)) = self.flows.remove(flow_id) else {
            return;
        };
        let canceling = matches!(flow.phase, OutputPhase::Canceling { .. });
        if matches!(end.outcome, StreamOutcome::Overflow) && !canceling {
            self.fail_overflowing_output(&flow);
        }
        match &flow.kind {
            OutputKind::Execution { terminal } => {
                // Delivered terminal frames reach the consumer in-band; otherwise (stopped or
                // violating stream) the independent lane carries the real outcome.
                if !end.delivered || canceling {
                    terminal.send_replace(Some(match &end.outcome {
                        StreamOutcome::Completed { payload } => exec_event(payload),
                        StreamOutcome::Failed { message } => ExecEvent::Error {
                            message: message.clone(),
                        },
                        StreamOutcome::Overflow => ExecEvent::Error {
                            message: CREDIT_VIOLATION.to_string(),
                        },
                    }));
                }
            }
            OutputKind::Download {
                progress_id,
                base_bytes,
            } => {
                let progress_id = *progress_id;
                if canceling {
                    self.acknowledge_download_cancel(progress_id);
                } else {
                    self.settle_download(progress_id, *base_bytes, &end);
                }
            }
        }
    }

    /// Stops an agent that ignored its credit and reports the violation to the consumer.
    fn fail_overflowing_output(&mut self, flow: &OutputFlow) {
        log!(
            Level::Warning,
            "Output stream exceeded flow-control credit: agent_id={}, request_id={}",
            flow.agent_id,
            flow.request_id
        );
        flow.lease.fail(CREDIT_VIOLATION.to_string());
        if let Some(connection) = self.agents.by_id.get(&flow.agent_id) {
            // The worker may still be producing; it must stop even though its stream is gone.
            let _ = connection.queue_priority_cancel(flow.request_id);
        }
    }

    /// Records a download's outcome from its terminal frame.
    fn settle_download(
        &mut self,
        progress_id: redoor_protocol::ids::TransferId,
        base_bytes: u64,
        end: &StreamEnd,
    ) {
        match &end.outcome {
            StreamOutcome::Completed { .. } if end.delivered => {
                if let Some(entry) = self.progress.entries.get_mut(&progress_id)
                    && matches!(entry.state, TransferProgressState::Active)
                {
                    // Count everything queued for the consumer, not just what it drained yet,
                    // so unknown-size archives complete with their full size.
                    entry.transferred_bytes = base_bytes.saturating_add(end.received_bytes);
                }
                self.mark_transfer_completed(progress_id);
            }
            StreamOutcome::Completed { .. } => {
                // The consumer vanished before the final frame could be handed over.
                self.mark_transfer_errored(progress_id, "Download canceled by client".to_string());
            }
            StreamOutcome::Failed { message } => {
                self.mark_transfer_errored(progress_id, message.clone());
            }
            StreamOutcome::Overflow => {
                self.mark_transfer_errored(progress_id, CREDIT_VIOLATION.to_string());
            }
        }
        self.notify_transfer_refresh();
    }

    /// Explicit file cancellation becomes terminal only when the agent released its worker.
    pub(crate) fn acknowledge_download_cancel(
        &mut self,
        progress_id: redoor_protocol::ids::TransferId,
    ) {
        if matches!(
            self.progress
                .entries
                .get(&progress_id)
                .map(|entry| &entry.state),
            Some(TransferProgressState::Canceling)
        ) {
            self.mark_transfer_canceled(progress_id);
            self.notify_transfer_refresh();
        }
    }

    /// Settles an output stream from its control-lane result, which the agent sends when the
    /// terminal frame could not travel in-band (or the command failed before streaming).
    pub(crate) fn finish_output_response(&mut self, flow_id: FlowId, response: RouteResponse) {
        let Some(Flow::Output(flow)) = self.flows.get(flow_id) else {
            return;
        };
        if flow.agent_id != response.agent_id {
            return;
        }
        let terminal_event = match (&flow.kind, &response.result) {
            (OutputKind::Execution { .. }, CommandResult::Exec { event }) => Some(event.clone()),
            (_, CommandResult::Error { message, .. }) => Some(ExecEvent::Error {
                message: message.clone(),
            }),
            _ => None,
        };
        let Some(event) = terminal_event else {
            log!(
                Level::Warning,
                "Received command response for output stream: request_id={}, result={:?}",
                response.request_id,
                response.result
            );
            return;
        };
        let Some(Flow::Output(flow)) = self.flows.remove(flow_id) else {
            return;
        };
        if let Some(connection) = self.agents.by_id.get(&flow.agent_id) {
            // No terminal frame will follow; the consumer ends after draining queued output.
            connection.streams.close_inbound(flow.request_id);
        }
        match flow.kind {
            OutputKind::Execution { ref terminal } => {
                // The fallback is authoritative even when no terminal payload was forwarded.
                terminal.send_replace(Some(event));
            }
            OutputKind::Download { progress_id, .. } => {
                let ExecEvent::Error { message } = event else {
                    return;
                };
                self.mark_transfer_errored(progress_id, message.clone());
                self.notify_transfer_refresh();
                flow.lease.fail(message);
            }
        }
    }

    /// Admits cancellation of an output stream on the agent's priority lane, then stops its
    /// data path. Failed admission leaves the stream untouched so the caller can retry.
    pub(crate) fn request_output_cancel(&mut self, flow_id: FlowId) -> Result<(), RouterError> {
        let Some(Flow::Output(flow)) = self.flows.get_mut(flow_id) else {
            return Ok(());
        };
        if matches!(flow.phase, OutputPhase::Canceling { .. }) {
            return Ok(());
        }
        let connection =
            self.agents
                .by_id
                .get(&flow.agent_id)
                .ok_or_else(|| RouterError::AgentNotFound {
                    agent_id: flow.agent_id.to_string(),
                })?;
        let retained_delivery = connection.queue_priority_cancel(flow.request_id)?;
        flow.phase = OutputPhase::Canceling { retained_delivery };
        flow.lease.stop();
        Ok(())
    }

    /// Applies a late-discovered archive total to the download that owns its history row.
    pub(crate) fn update_download_total(
        &mut self,
        flow_id: FlowId,
        agent_id: &redoor_protocol::ids::AgentId,
        total_bytes: Option<u64>,
    ) {
        let Some(Flow::Output(flow)) = self.flows.get(flow_id) else {
            return;
        };
        let OutputKind::Download { progress_id, .. } = flow.kind else {
            return;
        };
        if &flow.agent_id != agent_id {
            return;
        }
        if let Some(total) = total_bytes {
            self.set_download_total(progress_id, total);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::messages::RegisterAgentRequest;
    use super::super::streams::{ConnectionStreams, OutputReceiver};
    use super::*;
    use crate::actors::lanes::ControlLanes;
    use crate::{log_registry::LogRegistry, terminal_registry::TerminalRegistry};
    use redoor_protocol::ids::{AgentId, ChunkIndex, SocketId};
    use redoor_protocol::wire::stream::{StreamChunk, StreamPayloadKind};
    use tokio::sync::{mpsc, oneshot, watch};

    /// Keeps lane receivers alive so tests can exercise real admission pressure.
    struct Fixture {
        state: RouterState,
        commands: mpsc::Receiver<ServerToAgent>,
        priority: mpsc::Receiver<ServerToAgent>,
        streams: ConnectionStreams,
        agent: AgentId,
    }

    impl Fixture {
        /// Single-slot control lanes make saturation deterministic without a network peer.
        fn new(credit: bool) -> Self {
            let mut state = RouterState::new(
                tokio::spawn(std::future::pending()),
                TerminalRegistry::new(),
                LogRegistry::new(),
            );
            let agent = AgentId::from("output-test");
            let (lanes, receivers) = ControlLanes::bounded(1, 1);
            let streams = ConnectionStreams::new(agent.clone(), credit);
            let mut connection = AgentConnection::from_register_request(
                RegisterAgentRequest::for_test(&agent, lanes, streams.clone()),
            );
            let (binary, _binary_receiver) = mpsc::channel(1);
            connection.transfer = Some(super::super::state::TransferConnection {
                socket_id: SocketId::new(),
                outgoing_binary: binary,
                shutdown: watch::channel(false).0,
            });
            state.agents.by_id.insert(agent.clone(), connection);
            Self {
                state,
                commands: receivers.commands,
                priority: receivers.priority,
                streams,
                agent,
            }
        }

        /// Admits an output stream through the real start path.
        async fn start(
            &mut self,
            command: Command,
            tracking: OutputStreamTracking,
        ) -> Result<OutputReceiver, RouterError> {
            let (reply, receiver) = oneshot::channel();
            self.state.start_output(ExecuteStreamRequest {
                agent_id: self.agent.clone(),
                command,
                tracking,
                reply,
            });
            let result = receiver.await.unwrap();
            // Drain the admitted command so the single-slot lane is free again.
            let _ = self.commands.try_recv();
            result
        }

        /// Starts an execution and returns its consumer plus the independent terminal lane.
        async fn exec(&mut self) -> (OutputReceiver, watch::Receiver<Option<ExecEvent>>) {
            let (terminal, terminal_receiver) = watch::channel(None);
            let receiver = self
                .start(
                    Command::Exec {
                        request: redoor_protocol::wire::exec::ExecRequest {
                            argv: vec!["true".into()],
                            cwd: None,
                            env: Default::default(),
                            timeout_ms: None,
                        },
                    },
                    OutputStreamTracking::Execution { terminal },
                )
                .await
                .unwrap();
            (receiver, terminal_receiver)
        }

        /// Exact resource-specific cancellation path used by DELETE.
        fn cancel(&mut self, id: RequestId) -> Result<bool, RouterError> {
            self.state.cancel_execution(id, self.agent.clone())
        }

        /// Delivers one agent frame through the transfer reader's routing.
        async fn deliver(&mut self, id: RequestId, is_last: bool, data: Vec<u8>) {
            let end = self
                .streams
                .deliver(StreamChunk {
                    request_id: id,
                    chunk_index: ChunkIndex::new(0),
                    is_last,
                    is_error: false,
                    payload_kind: StreamPayloadKind::RawFile,
                    data,
                })
                .await;
            if let Some(end) = end {
                let flow_id = self.state.flows.id_for(id).unwrap();
                self.state.finish_output_stream(flow_id, end);
            }
        }
    }

    /// Both full lanes must retain one cancellation intent, with no fabricated termination.
    #[tokio::test]
    async fn saturated_lanes_retain_cancel_without_acknowledging_termination() {
        let mut fixture = Fixture::new(true);
        let (mut output, terminal) = fixture.exec().await;
        let id = output.request_id();
        let connection = &fixture.state.agents.by_id[&fixture.agent];
        let occupied = || ServerToAgent::Error {
            message: "occupied".to_string(),
        };
        assert!(connection.lanes.try_command(occupied()));
        assert!(connection.lanes.try_priority(occupied()));
        // Saturation accepts retained intent; repeated DELETE must not enqueue another task.
        assert_eq!(fixture.cancel(id), Ok(true));
        assert_eq!(fixture.cancel(id), Ok(true));
        assert!(terminal.borrow().is_none());
        // Releasing priority capacity delivers cancellation while ordinary commands stay full.
        fixture.priority.recv().await.unwrap();
        assert!(matches!(
            fixture.priority.recv().await,
            Some(ServerToAgent::CancelTransfer { request_id }) if request_id == id
        ));
        assert!(fixture.priority.try_recv().is_err());
        // The stopped stream ends for the consumer without waiting for more output.
        assert!(output.recv().await.is_none());
        // The agent's terminal frame confirms cleanup before ownership is released.
        fixture
            .deliver(id, true, serde_json::to_vec(&ExecEvent::Canceled).unwrap())
            .await;
        assert!(matches!(&*terminal.borrow(), Some(ExecEvent::Canceled)));
        assert!(fixture.state.flows.id_for(id).is_none());
    }

    /// Failed admission must leave the stream untouched so a caller can retry.
    #[tokio::test]
    async fn closed_priority_lane_rejects_without_losing_retry() {
        let mut fixture = Fixture::new(true);
        let (_output, terminal) = fixture.exec().await;
        let id = _output.request_id();
        fixture.priority.close();
        // Failure cannot stop output or pretend the process has stopped.
        assert!(fixture.cancel(id).is_err());
        assert!(matches!(
            fixture.state.flows.get(id),
            Some(Flow::Output(OutputFlow {
                phase: OutputPhase::Streaming,
                ..
            }))
        ));
        assert!(terminal.borrow().is_none());
        let (lanes, mut receivers) = ControlLanes::bounded(1, 1);
        fixture
            .state
            .agents
            .by_id
            .get_mut(&fixture.agent)
            .unwrap()
            .lanes = lanes;
        // The same execution remains cancelable once delivery is available again.
        assert_eq!(fixture.cancel(id), Ok(true));
        assert!(matches!(
            receivers.priority.recv().await,
            Some(ServerToAgent::CancelTransfer { request_id }) if request_id == id
        ));
    }

    /// Output queued before a cancel is still drained, then the consumer sees the end.
    #[tokio::test]
    async fn cancel_keeps_queued_output_and_releases_consumer() {
        let mut fixture = Fixture::new(false);
        let (mut output, _terminal) = fixture.exec().await;
        let id = output.request_id();
        fixture.deliver(id, false, b"queued".to_vec()).await;
        assert_eq!(fixture.cancel(id), Ok(true));
        // Accepted output is not discarded by cancellation.
        assert_eq!(output.recv().await.unwrap().data, b"queued");
        // Releasing a waiting consumer must not require another agent frame.
        assert!(output.recv().await.is_none());
    }

    /// Bad tracking metadata is rejected before remote work or file history is admitted.
    #[tokio::test]
    async fn command_tracking_mismatch_is_rejected_before_admission() {
        let mut fixture = Fixture::new(true);
        let result = fixture
            .start(
                Command::Exec {
                    request: redoor_protocol::wire::exec::ExecRequest {
                        argv: vec!["true".into()],
                        cwd: None,
                        env: Default::default(),
                        timeout_ms: None,
                    },
                },
                OutputStreamTracking::Download {
                    path: "/fake".into(),
                    total_bytes: 0,
                    full_size: None,
                    resume_offset: None,
                },
            )
            .await;
        // Mismatched resource metadata cannot fabricate progress or send a command.
        assert!(matches!(
            result,
            Err(RouterError::UnexpectedResponseType { .. })
        ));
        assert!(fixture.state.progress.entries.is_empty());
    }

    /// The control-lane fallback carries the actual outcome, not an invented cancellation.
    #[tokio::test]
    async fn command_fallback_preserves_actual_terminal_outcome() {
        for event in [
            ExecEvent::Canceled,
            ExecEvent::Exit {
                code: Some(0),
                signal: None,
            },
        ] {
            let mut fixture = Fixture::new(true);
            let (_output, terminal) = fixture.exec().await;
            let id = _output.request_id();
            // Admission alone is deliberately not termination.
            assert_eq!(fixture.cancel(id), Ok(true));
            assert!(terminal.borrow().is_none());
            fixture.state.finish_output_response(
                id,
                RouteResponse {
                    agent_id: fixture.agent.clone(),
                    socket_id: redoor_protocol::ids::SocketId::new(),
                    request_id: id,
                    result: CommandResult::Exec {
                        event: event.clone(),
                    },
                },
            );
            // The fallback is authoritative even if no terminal payload was forwarded.
            assert_eq!(
                serde_json::to_value(terminal.borrow().clone()).unwrap(),
                serde_json::to_value(Some(event)).unwrap()
            );
            assert!(fixture.state.flows.id_for(id).is_none());
        }
    }

    /// An exit already acknowledged by the agent wins over a later cancel request.
    #[tokio::test]
    async fn terminal_after_cancel_retains_normal_exit() {
        let mut fixture = Fixture::new(true);
        let (_output, terminal) = fixture.exec().await;
        let id = _output.request_id();
        assert_eq!(fixture.cancel(id), Ok(true));
        let exit = ExecEvent::Exit {
            code: Some(7),
            signal: None,
        };
        fixture
            .deliver(id, true, serde_json::to_vec(&exit).unwrap())
            .await;
        // The real exit is published even though the stopped consumer never saw the frame.
        assert!(matches!(
            &*terminal.borrow(),
            Some(ExecEvent::Exit { code: Some(7), .. })
        ));
    }

    /// Downloads complete with every byte the reader queued, and cancellation of a download
    /// is not reachable through the execution endpoint.
    #[tokio::test]
    async fn download_completion_counts_queued_bytes_and_is_not_an_execution() {
        let mut fixture = Fixture::new(true);
        let output = fixture
            .start(
                Command::TarDownload {
                    path: "/dir".into(),
                    include_root: true,
                },
                OutputStreamTracking::Download {
                    path: "/dir".into(),
                    total_bytes: 0,
                    full_size: None,
                    resume_offset: None,
                },
            )
            .await
            .unwrap();
        let id = output.request_id();
        // File history belongs to downloads only; DELETE /exec must not reach it.
        assert_eq!(fixture.cancel(id), Ok(false));
        fixture.deliver(id, false, vec![1; 10]).await;
        fixture.deliver(id, true, vec![1; 5]).await;
        let entry = &fixture.state.progress.entries[&id.as_transfer_id()];
        // Unknown-size archives complete with every queued byte even before they are drained.
        assert!(matches!(entry.state, TransferProgressState::Completed));
        assert_eq!(entry.total_bytes, 15);
    }

    /// A credit violation fails only the violating stream and stops its agent worker.
    #[tokio::test]
    async fn credit_overflow_fails_stream_and_cancels_worker() {
        let mut fixture = Fixture::new(true);
        // Room for the cancel and the credit grants the drained frames return.
        let (lanes, receivers) = ControlLanes::bounded(1, 16);
        fixture
            .state
            .agents
            .by_id
            .get_mut(&fixture.agent)
            .unwrap()
            .lanes = lanes;
        fixture.commands = receivers.commands;
        fixture.priority = receivers.priority;
        let (mut output, terminal) = fixture.exec().await;
        let id = output.request_id();
        for _ in 0..=redoor_protocol::wire::stream::STREAM_CREDIT_WINDOW_FRAMES + 1 {
            fixture.deliver(id, false, b"x".to_vec()).await;
        }
        // The worker is told to stop because its frames are no longer routed.
        assert!(matches!(
            fixture.priority.try_recv(),
            Ok(ServerToAgent::CancelTransfer { request_id }) if request_id == id
        ));
        // The independent terminal lane reports the violation.
        assert!(matches!(&*terminal.borrow(), Some(ExecEvent::Error { .. })));
        let mut last = None;
        while let Some(chunk) = output.recv().await {
            last = Some(chunk);
        }
        // The consumer ends with an error instead of a silently truncated stream.
        assert!(last.is_some_and(|chunk| chunk.is_error));
    }
}
