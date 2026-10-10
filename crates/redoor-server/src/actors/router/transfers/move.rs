//! The phase that turns a cross-agent copy into a move: deleting the verified source.

use super::super::flows::{CopyPhase, Flow, FlowId};
use super::super::state::{CopyContentKind, RouterState};
use super::copy::CopyRelease;
use redoor_core::log;
use redoor_protocol::wire::ServerToAgent;
use redoor_protocol::wire::command::{Command, CommandResult};
use redoor_protocol::wire::log::Level;

impl RouterState {
    /// Replaces the finished streams of a published remote move with one control command that
    /// deletes the source, which is the destructive commit phase and cannot be canceled.
    pub(crate) fn begin_source_deletion(&mut self, flow_id: FlowId) {
        let delete_id = self.next_id();
        let previous = self.flows.request_ids(flow_id);
        let Some(Flow::Copy(flow)) = self.flows.get_mut(flow_id) else {
            return;
        };
        let Some(delete) = &flow.then else {
            return;
        };
        let command = Command::DeleteMoveSource {
            path: delete.path.clone(),
            expected_identity: delete.identity.clone(),
        };
        let source_agent_id = flow.source_agent_id.clone();
        let progress_id = flow.progress_id;
        let content_kind = flow.content_kind;
        let finished = std::mem::replace(
            &mut flow.phase,
            CopyPhase::DeletingSource {
                request_id: delete_id,
            },
        );
        let dest_agent_id = flow.dest_agent_id.clone();
        self.flows.reindex(flow_id, &previous);
        if let CopyPhase::Remote(remote) = &finished {
            // Both streams finished; only their router-side state remains to release.
            self.release_remote_copy(&source_agent_id, &dest_agent_id, remote, CopyRelease::Quiet);
        }
        // Tar moves have no known size up front; the forwarded bytes are the total.
        let final_total = match content_kind {
            CopyContentKind::TarDirectory => self.transferred_bytes(progress_id),
            CopyContentKind::RawFile => None,
        };
        if let Some(entry) = self.progress.entries.get_mut(&progress_id) {
            entry.cancelable = false;
            if let Some(total) = final_total {
                entry.total_bytes = total;
            }
        }
        log!(
            Level::Debug,
            "Smart move queuing source deletion: public_request_id={}, source_agent_id={}, delete_request_id={}, command={:?}",
            progress_id,
            source_agent_id,
            delete_id,
            command
        );
        let queued = self
            .agents
            .by_id
            .get(&source_agent_id)
            .is_some_and(|source| {
                source.send_message(ServerToAgent::Command {
                    id: delete_id,
                    command,
                })
            });
        if !queued {
            log!(
                Level::Debug,
                "Smart move source deletion not queued; source agent unavailable: public_request_id={}, source_agent_id={}",
                progress_id,
                source_agent_id
            );
            self.flows.remove(flow_id);
            self.mark_transfer_errored(
                progress_id,
                "Source agent unavailable before move deletion".to_string(),
            );
        }
    }

    /// Completes a move once its source deletion finished.
    pub(crate) fn finish_source_deletion(&mut self, flow_id: FlowId, result: CommandResult) {
        let Some(Flow::Copy(flow)) = self.flows.remove(flow_id) else {
            return;
        };
        match result {
            CommandResult::RawDelete => {
                log!(
                    Level::Debug,
                    "Smart move source deletion completed: public_request_id={}",
                    flow.progress_id
                );
                self.mark_copy_transfer_completed(flow.progress_id, None);
            }
            CommandResult::Error { message, .. } => {
                log!(
                    Level::Debug,
                    "Smart move source deletion failed: public_request_id={}, message={}",
                    flow.progress_id,
                    message
                );
                self.mark_transfer_errored(flow.progress_id, message);
            }
            other => {
                log!(
                    Level::Warning,
                    "Unexpected move completion response: public_request_id={}, result={:?}",
                    flow.progress_id,
                    other
                );
                self.mark_transfer_errored(
                    flow.progress_id,
                    "Unexpected move completion response".to_string(),
                );
            }
        }
    }
}
