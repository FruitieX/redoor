//! Copies and moves on one agent or between two agents.
//!
//! A cross-agent copy is a source download and a destination upload joined by a
//! [`CopyForwarder`] task that moves frames directly between the two streams. A slow
//! destination therefore stalls only this copy's forwarder and, through credit, its source
//! stream, never the source agent's transfer reader or other streams of either agent.

use super::super::RouterError;
use super::super::flows::{
    CopyFlow, CopyPhase, DeleteSource, Flow, FlowId, RemoteCopy, RemoteStage,
};
use super::super::messages::{
    CancelPublicTransferError, RouteResponse, StartCopyRequest, TransferProgressUpdateRequest,
};
use super::super::progress::CopyStartContext;
use super::super::state::{CopyContentKind, CopyOperation, RouterState};
use super::super::streams::{
    OutboundError, OutboundStream, OutputReceiver, StreamEnd, StreamLease, StreamOutcome,
};
use redoor_core::log;
use redoor_protocol::ids::{AgentId, ChunkIndex, RequestId};
use redoor_protocol::wire::ServerToAgent;
use redoor_protocol::wire::log::Level;
use redoor_protocol::wire::stream::{StreamChunk, StreamChunkFrameRequest, StreamPayloadKind};
use redoor_protocol::{
    api::transfers::{TransferDirection, TransferProgressState},
    wire::command::{Command, CommandResult},
};

/// How a finished copy releases its streams.
pub(crate) enum CopyRelease<'a> {
    /// Completed or fully acknowledged: stop the data path without notifying agents.
    Quiet,
    /// Failed: abort the destination with this reason and cancel an unfinished source.
    Fail(&'a str),
}

/// Moves one remote copy's frames from the source stream to the destination stream.
///
/// Runs outside the router so per-frame work never touches its mailbox. It owns the
/// destination chunk numbering, so it is also the one that sends the destination's abort
/// frame when the router fails the copy.
struct CopyForwarder {
    source: OutputReceiver,
    dest: OutboundStream,
    payload_kind: StreamPayloadKind,
    next_index: ChunkIndex,
}

impl CopyForwarder {
    /// Forwards frames until the source ends, the destination fails or the router stops it.
    async fn run(mut self) {
        let stop = self.dest.lease().cancel.clone();
        loop {
            let chunk = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                chunk = self.source.recv() => chunk,
            };
            let Some(chunk) = chunk else {
                break;
            };
            if !self.forward(chunk).await {
                return;
            }
        }
        self.abort_if_failed().await;
    }

    /// Forwards one source frame and reports whether more frames should follow.
    async fn forward(&mut self, chunk: StreamChunk) -> bool {
        if chunk.is_error {
            let message = if chunk.data.is_empty() {
                "Copy download failed on agent".to_string()
            } else {
                String::from_utf8_lossy(&chunk.data).into_owned()
            };
            self.abort(&message).await;
            return false;
        }
        if chunk.payload_kind != self.payload_kind {
            // The destination's error response carries this reason back to the router.
            let message = format!(
                "Copy payload kind mismatch: expected {:?}, got {:?}",
                self.payload_kind, chunk.payload_kind
            );
            self.abort(&message).await;
            return false;
        }
        let is_last = chunk.is_last;
        let request = StreamChunkFrameRequest::new(self.dest.request_id(), &chunk.data)
            .payload_kind(self.payload_kind)
            .is_last(is_last);
        match self.dest.send_frames(&mut self.next_index, request).await {
            Ok(()) => !is_last,
            Err(OutboundError::Stopped) => {
                self.abort_if_failed().await;
                false
            }
            // The router fails the copy when it processes the lost transfer socket.
            Err(OutboundError::LaneClosed) => false,
        }
    }

    /// Sends the destination's abort frame when the router failed (not merely stopped) it.
    async fn abort_if_failed(&mut self) {
        if let Some(reason) = self.dest.fault().map(str::to_string) {
            self.abort(&reason).await;
        }
    }

    /// Ends the destination upload with an error frame so its worker discards staging.
    async fn abort(&mut self, reason: &str) {
        self.dest
            .send_abort(self.next_index, self.payload_kind, reason)
            .await;
        self.next_index = self.next_index.saturating_next_index();
    }
}

impl RouterState {
    /// Starts a local copy or move, or a remote streamed one, and replies with its public id.
    pub(crate) fn start_copy(&mut self, request: StartCopyRequest) {
        let (Some(source), Some(dest)) = (
            self.agents.by_id.get(&request.source_agent_id),
            self.agents.by_id.get(&request.dest_agent_id),
        ) else {
            let missing = if self.agents.by_id.contains_key(&request.source_agent_id) {
                &request.dest_agent_id
            } else {
                &request.source_agent_id
            };
            let _ = request.reply.send(Err(RouterError::AgentNotFound {
                agent_id: missing.to_string(),
            }));
            return;
        };
        let remote = request.source_agent_id != request.dest_agent_id;
        if remote
            && let Err(error) = source
                .transfer_connection()
                .and_then(|_| dest.transfer_connection())
        {
            let _ = request.reply.send(Err(error));
            return;
        }
        let then = match (request.operation, request.source_identity.clone()) {
            (CopyOperation::Copy, _) => None,
            (CopyOperation::Move, Some(identity)) => Some(DeleteSource {
                path: request.source_path.clone(),
                identity,
            }),
            (CopyOperation::Move, None) => {
                // Deleting a source whose identity was never captured could remove a
                // replacement created while copying.
                let _ = request.reply.send(Err(RouterError::UnexpectedResponseType {
                    operation: "starting a move without its source identity",
                }));
                return;
            }
        };

        let flow_id = self.next_id();
        let progress_id = flow_id.as_transfer_id();
        self.record_copy_start(CopyStartContext {
            request_id: progress_id,
            source_agent_id: request.source_agent_id.clone(),
            source_path: request.source_path.clone(),
            dest_agent_id: request.dest_agent_id.clone(),
            dest_path: request.dest_path.clone(),
            total_bytes: request.total_bytes,
            direction: match request.operation {
                CopyOperation::Copy => TransferDirection::Copy,
                CopyOperation::Move => TransferDirection::Move,
            },
        });
        if let Some(terminal) = request.terminal.clone() {
            self.progress
                .terminal_observers
                .insert(progress_id, terminal);
        }
        if then.is_some() {
            log!(
                Level::Debug,
                "Smart move routing started: public_request_id={}, source_agent_id={}, dest_agent_id={}, source_path={}, dest_path={}, local={}, content_kind={:?}, on_existing={:?}, source_identity={:?}",
                progress_id,
                request.source_agent_id,
                request.dest_agent_id,
                request.source_path,
                request.dest_path,
                !remote,
                request.content_kind,
                request.on_existing,
                request.source_identity
            );
        }

        let (queued_agent, queued) = if remote {
            self.start_remote_copy(flow_id, &request, then)
        } else {
            self.start_local_copy(flow_id, &request, then)
        };
        if !queued {
            if let Some(Flow::Copy(flow)) = self.flows.remove(flow_id) {
                self.release_copy(&flow, CopyRelease::Quiet);
            }
            self.mark_transfer_errored(progress_id, "Agent control queue is full".to_string());
            let _ = request.reply.send(Err(RouterError::ControlQueueFull {
                agent_id: queued_agent.to_string(),
            }));
            return;
        }
        let _ = request.reply.send(Ok(progress_id));
    }

    /// Runs the whole copy or move as one agent-local command; returns the agent and whether
    /// the command was queued.
    fn start_local_copy(
        &mut self,
        flow_id: FlowId,
        request: &StartCopyRequest,
        then: Option<DeleteSource>,
    ) -> (AgentId, bool) {
        let request_id = self.next_id();
        let command = match &then {
            Some(delete) => Command::LocalMove {
                source_path: request.source_path.clone(),
                dest_path: request.dest_path.clone(),
                source_is_directory: request.content_kind == CopyContentKind::TarDirectory,
                expected_identity: delete.identity.clone(),
                on_existing: request.on_existing,
            },
            None => match request.content_kind {
                CopyContentKind::RawFile => Command::LocalCopyFile {
                    source_path: request.source_path.clone(),
                    dest_path: request.dest_path.clone(),
                    on_existing: request.on_existing,
                },
                CopyContentKind::TarDirectory => Command::LocalCopyDirectory {
                    source_path: request.source_path.clone(),
                    dest_path: request.dest_path.clone(),
                    on_existing: request.on_existing,
                },
            },
        };
        self.flows.insert(
            flow_id,
            Flow::Copy(Box::new(CopyFlow {
                progress_id: flow_id.as_transfer_id(),
                source_agent_id: request.source_agent_id.clone(),
                dest_agent_id: request.dest_agent_id.clone(),
                content_kind: request.content_kind,
                then,
                phase: CopyPhase::Local { request_id },
            })),
        );
        let queued = self
            .agents
            .by_id
            .get(&request.source_agent_id)
            .is_some_and(|agent| {
                agent.send_message(ServerToAgent::Command {
                    agent_id: request.source_agent_id.clone(),
                    request_id,
                    command,
                })
            });
        (request.source_agent_id.clone(), queued)
    }

    /// Opens both streams and the forwarder, then starts the destination upload; the source
    /// download waits for the destination's readiness.
    fn start_remote_copy(
        &mut self,
        flow_id: FlowId,
        request: &StartCopyRequest,
        then: Option<DeleteSource>,
    ) -> (AgentId, bool) {
        let dest_id = self.next_id();
        let source_id = self.next_id();
        let (Some(source_agent), Some(dest_agent)) = (
            self.agents.by_id.get(&request.source_agent_id),
            self.agents.by_id.get(&request.dest_agent_id),
        ) else {
            return (request.source_agent_id.clone(), false);
        };
        let source = StreamLease::new(source_id, source_agent.link(), 0);
        let source_receiver = source_agent.streams.open_inbound(source.clone());
        let dest = OutboundStream::new(
            StreamLease::new(dest_id, dest_agent.link(), 0),
            dest_agent.streams.open_outbound(dest_id),
        );
        tokio::spawn(
            CopyForwarder {
                source: source_receiver,
                dest: dest.clone(),
                payload_kind: request.content_kind.payload_kind(),
                next_index: ChunkIndex::new(0),
            }
            .run(),
        );
        let queued = dest_agent.send_message(ServerToAgent::Command {
            agent_id: request.dest_agent_id.clone(),
            request_id: dest_id,
            command: request
                .content_kind
                .upload_command(request.dest_path.clone(), request.on_existing),
        });
        self.flows.insert(
            flow_id,
            Flow::Copy(Box::new(CopyFlow {
                progress_id: flow_id.as_transfer_id(),
                source_agent_id: request.source_agent_id.clone(),
                dest_agent_id: request.dest_agent_id.clone(),
                content_kind: request.content_kind,
                then,
                phase: CopyPhase::Remote(Box::new(RemoteCopy {
                    source,
                    dest,
                    stage: RemoteStage::AwaitingDestination {
                        source_command: request
                            .content_kind
                            .download_command(request.source_path.clone()),
                    },
                    source_finished: false,
                })),
            })),
        );
        (request.dest_agent_id.clone(), queued)
    }

    /// Stops a copy's data path and forgets its stream state on both agents.
    pub(crate) fn release_copy(&self, flow: &CopyFlow, release: CopyRelease<'_>) {
        if let CopyPhase::Remote(remote) = &flow.phase {
            self.release_remote_copy(&flow.source_agent_id, &flow.dest_agent_id, remote, release);
        }
    }

    /// Stops both streams of a remote copy; a failure also aborts the destination worker and
    /// cancels a source that is still producing.
    pub(crate) fn release_remote_copy(
        &self,
        source_agent_id: &AgentId,
        dest_agent_id: &AgentId,
        remote: &RemoteCopy,
        release: CopyRelease<'_>,
    ) {
        if let CopyRelease::Fail(reason) = release {
            // The forwarder owns the destination's chunk numbering and sends the abort frame.
            remote.dest.lease().fail(reason.to_string());
            let source_started = !matches!(remote.stage, RemoteStage::AwaitingDestination { .. });
            if source_started
                && !remote.source_finished
                && let Some(source) = self.agents.by_id.get(source_agent_id)
            {
                source
                    .lanes
                    .try_priority_or_command(ServerToAgent::CancelTransfer {
                        request_id: remote.source_id(),
                    });
            }
        } else {
            remote.dest.lease().stop();
        }
        remote.source.stop();
        if let Some(source) = self.agents.by_id.get(source_agent_id) {
            source.streams.close_inbound(remote.source_id());
        }
        if let Some(dest) = self.agents.by_id.get(dest_agent_id) {
            dest.streams.close_outbound(remote.dest_id());
        }
    }

    /// Removes a copy, releases its streams and records its terminal outcome.
    fn end_copy(&mut self, flow_id: FlowId, failure: Option<String>) {
        let Some(Flow::Copy(flow)) = self.flows.remove(flow_id) else {
            return;
        };
        match failure {
            Some(message) => {
                self.release_copy(&flow, CopyRelease::Fail(&message));
                self.mark_transfer_errored(flow.progress_id, message);
            }
            None => self.release_copy(&flow, CopyRelease::Quiet),
        }
    }

    /// Starts the source download once the destination worker can receive its bytes.
    pub(crate) fn start_copy_source(
        &mut self,
        flow_id: FlowId,
        dest_agent_id: &AgentId,
        dest_request_id: RequestId,
    ) {
        let Some(Flow::Copy(flow)) = self.flows.get_mut(flow_id) else {
            return;
        };
        let CopyPhase::Remote(remote) = &mut flow.phase else {
            return;
        };
        if &flow.dest_agent_id != dest_agent_id || remote.dest_id() != dest_request_id {
            return;
        }
        // A cancel accepted before readiness already settled the never-started source.
        let RemoteStage::AwaitingDestination { .. } = remote.stage else {
            return;
        };
        let RemoteStage::AwaitingDestination { source_command } =
            std::mem::replace(&mut remote.stage, RemoteStage::Streaming)
        else {
            return;
        };
        let source_agent_id = flow.source_agent_id.clone();
        let source_id = remote.source_id();
        let queued = self
            .agents
            .by_id
            .get(&source_agent_id)
            .is_some_and(|source| {
                source.send_message(ServerToAgent::Command {
                    agent_id: source_agent_id.clone(),
                    request_id: source_id,
                    command: source_command,
                })
            });
        if !queued {
            self.end_copy(
                flow_id,
                Some("Source agent control queue is unavailable".to_string()),
            );
        }
    }

    /// Settles a copy's source stream from its terminal frame.
    pub(crate) fn finish_copy_source(&mut self, flow_id: FlowId, end: StreamEnd) {
        let Some(Flow::Copy(flow)) = self.flows.get_mut(flow_id) else {
            return;
        };
        let CopyPhase::Remote(remote) = &mut flow.phase else {
            return;
        };
        if flow.source_agent_id != end.agent_id || remote.source_id() != end.request_id {
            log!(
                Level::Warning,
                "Copy source mismatch: request_id={}, expected_agent_id={}, actual_agent_id={}",
                flow.progress_id,
                flow.source_agent_id,
                end.agent_id
            );
            return;
        }
        remote.source_finished = true;
        let canceling = matches!(remote.stage, RemoteStage::Canceling { .. });
        let failure = match end.outcome {
            // A finished source download holds nothing to clean up, so while canceling its
            // completion is its acknowledgement; otherwise the destination's response completes
            // the copy once it published the bytes.
            StreamOutcome::Completed { .. } if canceling => {
                self.acknowledge_copy_cancel(flow_id, end.request_id);
                return;
            }
            StreamOutcome::Completed { .. } => return,
            StreamOutcome::Failed { message } if message.is_empty() => {
                "Copy download failed on agent".to_string()
            }
            StreamOutcome::Failed { message } => message,
            StreamOutcome::Overflow => {
                // The source keeps producing although its stream is gone, so stop it too.
                remote.source_finished = false;
                "Copy source exceeded the stream flow-control credit".to_string()
            }
        };
        if canceling {
            self.acknowledge_copy_cancel(flow_id, end.request_id);
        } else {
            self.end_copy(flow_id, Some(failure));
        }
    }

    /// Settles a copy from the final response of its destination, local or delete command.
    pub(crate) fn finish_copy_response(&mut self, flow_id: FlowId, response: RouteResponse) {
        let Some(Flow::Copy(flow)) = self.flows.get(flow_id) else {
            return;
        };
        let request_id = response.request_id;
        let agent_id = &response.agent_id;
        match &flow.phase {
            CopyPhase::Remote(remote)
                if remote.dest_id() == request_id && &flow.dest_agent_id == agent_id =>
            {
                self.finish_remote_destination(flow_id, response.result)
            }
            CopyPhase::Remote(remote)
                if remote.source_id() == request_id && &flow.source_agent_id == agent_id =>
            {
                // A source that answers on the control lane failed before it could stream.
                let message = match response.result {
                    CommandResult::Error { message, .. } => message,
                    other => format!("Unexpected copy source response: {}", other.summary()),
                };
                if let Some(Flow::Copy(flow)) = self.flows.get_mut(flow_id)
                    && let CopyPhase::Remote(remote) = &mut flow.phase
                {
                    remote.source_finished = true;
                    if matches!(remote.stage, RemoteStage::Canceling { .. }) {
                        self.acknowledge_copy_cancel(flow_id, request_id);
                        return;
                    }
                }
                self.end_copy(flow_id, Some(message));
            }
            CopyPhase::Local { request_id: local }
                if *local == request_id && &flow.source_agent_id == agent_id =>
            {
                self.finish_local_copy(flow_id, response.result)
            }
            CopyPhase::DeletingSource { request_id: delete }
                if *delete == request_id && &flow.source_agent_id == agent_id =>
            {
                self.finish_source_deletion(flow_id, response.result)
            }
            _ => log!(
                Level::Warning,
                "Unexpected copy response: agent_id={}, request_id={}",
                agent_id,
                request_id
            ),
        }
    }

    /// Settles a remote copy from its destination's final response.
    fn finish_remote_destination(&mut self, flow_id: FlowId, result: CommandResult) {
        let Some(Flow::Copy(flow)) = self.flows.get(flow_id) else {
            return;
        };
        let CopyPhase::Remote(remote) = &flow.phase else {
            return;
        };
        let canceling = matches!(remote.stage, RemoteStage::Canceling { .. });
        let published = flow.content_kind.completion_matches(&result);
        let dest_id = remote.dest_id();
        let progress_id = flow.progress_id;
        if flow.is_move() && published && !canceling {
            self.begin_source_deletion(flow_id);
            return;
        }
        match result {
            _ if flow.is_move() && published => {
                log!(
                    Level::Debug,
                    "Smart move destination completed while canceling; preserving source: public_request_id={}, request_id={}",
                    progress_id,
                    dest_id
                );
                // Cancellation before source deletion preserves the source even if the
                // destination publication won.
                self.acknowledge_copy_cancel(flow_id, dest_id);
            }
            CommandResult::Error { .. } if canceling => {
                self.acknowledge_copy_cancel(flow_id, dest_id);
            }
            CommandResult::Error { message, .. } => self.end_copy(flow_id, Some(message)),
            _ if published => {
                // Tar copies have no known size up front; the forwarded bytes are the total.
                let total = match flow.content_kind {
                    CopyContentKind::TarDirectory => self.transferred_bytes(progress_id),
                    CopyContentKind::RawFile => None,
                };
                self.end_copy(flow_id, None);
                self.mark_copy_transfer_completed(progress_id, total);
            }
            other => {
                log!(
                    Level::Warning,
                    "Unexpected copy completion response: request_id={}, result={:?}",
                    dest_id,
                    other
                );
                self.end_copy(
                    flow_id,
                    Some("Unexpected copy completion response".to_string()),
                );
            }
        }
    }

    /// Settles an agent-local copy or move from its single command result.
    fn finish_local_copy(&mut self, flow_id: FlowId, result: CommandResult) {
        let Some(Flow::Copy(flow)) = self.flows.remove(flow_id) else {
            return;
        };
        let progress_id = flow.progress_id;
        let canceling = matches!(
            self.progress
                .entries
                .get(&progress_id)
                .map(|entry| &entry.state),
            Some(TransferProgressState::Canceling)
        );
        // Directory copies have no known size up front; the counted bytes are the total.
        let tar_total = match flow.content_kind {
            CopyContentKind::TarDirectory => self.transferred_bytes(progress_id),
            CopyContentKind::RawFile => None,
        };
        match result {
            CommandResult::Error { .. } if canceling => self.mark_transfer_canceled(progress_id),
            CommandResult::Error { message, .. } => {
                if flow.is_move() {
                    log!(
                        Level::Debug,
                        "Smart move command failed: public_request_id={}, message={}",
                        progress_id,
                        message
                    );
                }
                self.mark_transfer_errored(progress_id, message);
            }
            CommandResult::LocalMove { atomic } if flow.is_move() => {
                log!(
                    Level::Debug,
                    "Smart move local completion: public_request_id={}, atomic={}",
                    progress_id,
                    atomic
                );
                if let Some(entry) = self.progress.entries.get_mut(&progress_id) {
                    // Copy/delete and cross-agent completions never take this arm.
                    entry.atomic = atomic;
                }
                self.mark_copy_transfer_completed(progress_id, tar_total);
            }
            CommandResult::LocalCopyFile
                if !flow.is_move() && flow.content_kind == CopyContentKind::RawFile =>
            {
                self.mark_copy_transfer_completed(progress_id, None);
            }
            CommandResult::LocalCopyDirectory
                if !flow.is_move() && flow.content_kind == CopyContentKind::TarDirectory =>
            {
                self.mark_copy_transfer_completed(progress_id, tar_total);
            }
            other => {
                log!(
                    Level::Warning,
                    "Unexpected local copy completion response: public_request_id={}, result={:?}",
                    progress_id,
                    other
                );
                self.mark_transfer_errored(
                    progress_id,
                    "Unexpected copy completion response".to_string(),
                );
            }
        }
    }

    /// Records one side's cleanup acknowledgement of a canceled remote copy; the copy settles
    /// as canceled only once both temp-owning workers released their staging.
    pub(crate) fn acknowledge_copy_cancel(&mut self, flow_id: FlowId, request_id: RequestId) {
        let Some(Flow::Copy(flow)) = self.flows.get_mut(flow_id) else {
            return;
        };
        let CopyPhase::Remote(remote) = &mut flow.phase else {
            return;
        };
        let (source_id, dest_id) = (remote.source_id(), remote.dest_id());
        let RemoteStage::Canceling {
            source_acked,
            dest_acked,
        } = &mut remote.stage
        else {
            return;
        };
        if request_id == source_id {
            *source_acked = true;
        } else if request_id == dest_id {
            *dest_acked = true;
        }
        if *source_acked && *dest_acked {
            let progress_id = flow.progress_id;
            self.end_copy(flow_id, None);
            self.mark_transfer_canceled(progress_id);
        }
    }

    /// Sends cancellation to every worker of a copy after its row became `Canceling`.
    pub(crate) fn cancel_copy(&mut self, flow_id: FlowId) -> Result<(), CancelPublicTransferError> {
        let Some(Flow::Copy(flow)) = self.flows.get_mut(flow_id) else {
            return Err(CancelPublicTransferError::NotFound);
        };
        let targets = match &mut flow.phase {
            CopyPhase::Remote(remote) => {
                let mut targets = vec![(flow.dest_agent_id.clone(), remote.dest_id())];
                // No source worker exists before readiness, and a finished source already
                // released its worker, so cancellation itself is their cleanup.
                let source_acked = remote.source_finished
                    || matches!(remote.stage, RemoteStage::AwaitingDestination { .. });
                if !source_acked {
                    targets.push((flow.source_agent_id.clone(), remote.source_id()));
                }
                remote.stage = RemoteStage::Canceling {
                    source_acked,
                    dest_acked: false,
                };
                targets
            }
            CopyPhase::Local { request_id } => vec![(flow.source_agent_id.clone(), *request_id)],
            CopyPhase::DeletingSource { .. } => {
                return Err(CancelPublicTransferError::NotCancelable);
            }
        };
        for (agent_id, request_id) in targets {
            if let Some(connection) = self.agents.by_id.get(&agent_id) {
                // Falling back to the command lane is slower but must not lose cleanup.
                connection
                    .lanes
                    .try_priority_or_command(ServerToAgent::CancelTransfer { request_id });
            }
        }
        Ok(())
    }

    /// Applies agent-reported progress of an agent-local copy.
    pub(crate) fn update_copy_progress(
        &mut self,
        flow_id: FlowId,
        request: TransferProgressUpdateRequest,
    ) {
        let Some(Flow::Copy(flow)) = self.flows.get(flow_id) else {
            return;
        };
        let CopyPhase::Local { request_id } = flow.phase else {
            return;
        };
        if flow.source_agent_id != request.agent_id || request_id != request.request_id {
            log!(
                Level::Warning,
                "Copy progress update agent mismatch: request_id={}, expected_agent_id={}, actual_agent_id={}",
                flow.progress_id,
                flow.source_agent_id,
                request.agent_id
            );
            return;
        }
        let progress_id = flow.progress_id;
        self.set_copy_progress(progress_id, request.transferred_bytes, request.total_bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::streams::{AgentLink, ConnectionStreams};
    use super::*;
    use crate::actors::lanes::ControlLanes;
    use redoor_protocol::wire::stream::STREAM_CREDIT_WINDOW_FRAMES;
    use tokio::sync::mpsc;

    /// Builds one credited agent session with an observable transfer lane.
    fn agent(name: &str) -> (ConnectionStreams, AgentLink, mpsc::Receiver<StreamChunk>) {
        let (transfer, lane) = mpsc::channel(64);
        let agent = AgentId::from(name);
        (
            ConnectionStreams::new(agent.clone(), true),
            AgentLink::new(ControlLanes::bounded(16, 16).0, Some(transfer), true),
            lane,
        )
    }

    /// Builds one source data frame.
    fn frame(request_id: RequestId, is_last: bool) -> StreamChunk {
        StreamChunk {
            request_id,
            chunk_index: ChunkIndex::new(0),
            is_last,
            is_error: false,
            payload_kind: StreamPayloadKind::RawFile,
            data: vec![3; 8],
        }
    }

    /// A stalled destination stalls only its own copy: the source agent's reader keeps
    /// accepting frames for other streams while the copy waits for destination credit.
    #[tokio::test]
    async fn slow_destination_does_not_block_source_reader() {
        let (source_streams, source_link, _source_lane) = agent("source");
        let (dest_streams, dest_link, mut dest_lane) = agent("dest");
        let source = StreamLease::new(RequestId::new(1), source_link.clone(), 0);
        let receiver = source_streams.open_inbound(source.clone());
        let dest = OutboundStream::new(
            StreamLease::new(RequestId::new(2), dest_link, 0),
            dest_streams.open_outbound(RequestId::new(2)),
        );
        tokio::spawn(
            CopyForwarder {
                source: receiver,
                dest: dest.clone(),
                payload_kind: StreamPayloadKind::RawFile,
                next_index: ChunkIndex::new(0),
            }
            .run(),
        );
        let mut other =
            source_streams.open_inbound(StreamLease::new(RequestId::new(3), source_link, 0));
        // The destination never grants credit, so only its window can be forwarded.
        for _ in 0..STREAM_CREDIT_WINDOW_FRAMES * 2 {
            assert!(
                source_streams
                    .deliver(frame(RequestId::new(1), false))
                    .await
                    .is_none()
            );
            tokio::task::yield_now().await;
        }
        // Unrelated streams of the source agent are still served by the same reader.
        source_streams
            .deliver(frame(RequestId::new(3), false))
            .await;
        assert_eq!(other.recv().await.unwrap().request_id, RequestId::new(3));
        let mut forwarded = 0;
        while dest_lane.try_recv().is_ok() {
            forwarded += 1;
        }
        // The destination's credit window bounds what the forwarder pushed to it.
        assert_eq!(forwarded, STREAM_CREDIT_WINDOW_FRAMES as usize);
        // Failing the copy makes the forwarder abort the destination worker.
        dest.lease().fail("Agent disconnected: source".to_string());
        let abort = dest_lane.recv().await.unwrap();
        assert!(abort.is_error && abort.data == b"Agent disconnected: source");
    }

    /// The forwarder relays the final frame and stops.
    #[tokio::test]
    async fn forwarder_relays_last_frame() {
        let (source_streams, source_link, _source_lane) = agent("source");
        let (dest_streams, dest_link, mut dest_lane) = agent("dest");
        let receiver =
            source_streams.open_inbound(StreamLease::new(RequestId::new(1), source_link, 0));
        let dest = OutboundStream::new(
            StreamLease::new(RequestId::new(2), dest_link, 0),
            dest_streams.open_outbound(RequestId::new(2)),
        );
        let forwarder = tokio::spawn(
            CopyForwarder {
                source: receiver,
                dest: dest.clone(),
                payload_kind: StreamPayloadKind::RawFile,
                next_index: ChunkIndex::new(0),
            }
            .run(),
        );
        source_streams
            .deliver(frame(RequestId::new(1), false))
            .await;
        let end = source_streams.deliver(frame(RequestId::new(1), true)).await;
        // The router learns the source finished from the reader, independent of forwarding.
        assert!(end.is_some_and(|end| end.delivered));
        forwarder.await.unwrap();
        let mut frames = Vec::new();
        while let Ok(frame) = dest_lane.try_recv() {
            frames.push(frame);
        }
        // Frames are renumbered for the destination stream and the last one stays terminal.
        assert_eq!(frames.len(), 2);
        assert!(
            frames
                .iter()
                .all(|frame| frame.request_id == RequestId::new(2))
        );
        assert!(frames[1].is_last && frames[1].chunk_index == ChunkIndex::new(1));
        // Forwarded bytes are the copy's progress.
        assert_eq!(dest.lease().bytes(), 16);
    }

    /// A copy canceled after its source already finished must settle once the destination
    /// acknowledges, instead of waiting for a source acknowledgement that can never come.
    #[tokio::test]
    async fn cancel_after_source_finished_settles_on_destination_ack() {
        let mut state = RouterState::new(
            tokio::spawn(async {}),
            crate::terminal_registry::TerminalRegistry::new(),
            crate::log_registry::LogRegistry::new(),
        );
        let flow_id = RequestId::new(70);
        let (dest_id, source_id) = (RequestId::new(71), RequestId::new(72));
        state.record_copy_start(CopyStartContext {
            request_id: flow_id.as_transfer_id(),
            source_agent_id: AgentId::from("source"),
            source_path: "/source".into(),
            dest_agent_id: AgentId::from("dest"),
            dest_path: "/dest".into(),
            total_bytes: 8,
            direction: TransferDirection::Copy,
        });
        let detached = |id| {
            StreamLease::new(
                id,
                AgentLink::new(ControlLanes::bounded(1, 1).0, None, false),
                0,
            )
        };
        state.flows.insert(
            flow_id,
            Flow::Copy(Box::new(CopyFlow {
                progress_id: flow_id.as_transfer_id(),
                source_agent_id: AgentId::from("source"),
                dest_agent_id: AgentId::from("dest"),
                content_kind: CopyContentKind::RawFile,
                then: None,
                phase: CopyPhase::Remote(Box::new(RemoteCopy {
                    source: detached(source_id),
                    dest: OutboundStream::new(detached(dest_id), None),
                    stage: RemoteStage::Streaming,
                    source_finished: true,
                })),
            })),
        );
        assert!(
            state
                .cancel_public_transfer(flow_id.as_transfer_id())
                .is_ok()
        );
        state.finish_copy_response(
            flow_id,
            RouteResponse {
                agent_id: AgentId::from("dest"),
                request_id: dest_id,
                result: CommandResult::error(
                    redoor_protocol::wire::command::CommandErrorKind::InvalidInput,
                    "Upload canceled by server",
                ),
            },
        );
        // The destination's cleanup is the last acknowledgement a finished source needs.
        assert!(matches!(
            state.progress.entries[&flow_id.as_transfer_id()].state,
            TransferProgressState::Canceled
        ));
        assert!(state.flows.id_for(dest_id).is_none());
        state.ui.refresh_check_task.abort();
    }
}
