//! Typed outbound lanes to one agent connection.
//!
//! The router, the stream data path and the session queue typed [`ServerToAgent`] and
//! [`StreamChunk`] frames; only the session writer turns them into websocket messages. JSON is
//! therefore produced in exactly one place, and tests observe the frames an agent would
//! receive as values instead of parsing text.

use redoor_protocol::wire::{ServerToAgent, stream::StreamChunk};
use tokio::sync::mpsc;

/// Queue depth of the interactive (terminal input) lane.
const INTERACTIVE_QUEUE_CAPACITY: usize = 8;
/// Queue depth of the bulk lane.
///
/// One slot means producers wait in the lane's fair queue, so streams with data and credit
/// take turns frame by frame, and a control frame waits behind at most one bulk frame.
const BULK_QUEUE_CAPACITY: usize = 1;

/// The bounded outbound lanes of one agent connection, drained by one writer in strict order:
/// priority, commands, interactive, bulk.
///
/// Ordinary commands use the command lane, whose fullness is admission control. Cancellation,
/// credit grants and lifecycle frames use the reserved priority lane, so they never queue
/// behind a command backlog or stream data. Terminal input uses the interactive lane and every
/// other stream the bulk lane; since a data frame is at most 64 KiB, a control frame never
/// waits behind more than one of them.
#[derive(Clone, Debug)]
pub struct ControlLanes {
    commands: mpsc::Sender<ServerToAgent>,
    priority: mpsc::Sender<ServerToAgent>,
    interactive: mpsc::Sender<StreamChunk>,
    bulk: mpsc::Sender<StreamChunk>,
}

/// Receiving ends of [`ControlLanes`], owned by the session writer.
pub struct ControlLaneReceivers {
    pub commands: mpsc::Receiver<ServerToAgent>,
    pub priority: mpsc::Receiver<ServerToAgent>,
    pub interactive: mpsc::Receiver<StreamChunk>,
    pub bulk: mpsc::Receiver<StreamChunk>,
}

/// The agent session behind a lane has ended, so nothing queued there will be delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("agent control lane is closed")]
pub struct LaneClosed;

impl ControlLanes {
    /// Creates the lanes with the given control capacities.
    pub fn bounded(commands: usize, priority: usize) -> (Self, ControlLaneReceivers) {
        let (commands, command_receiver) = mpsc::channel(commands);
        let (priority, priority_receiver) = mpsc::channel(priority);
        let (interactive, interactive_receiver) = mpsc::channel(INTERACTIVE_QUEUE_CAPACITY);
        let (bulk, bulk_receiver) = mpsc::channel(BULK_QUEUE_CAPACITY);
        (
            Self {
                commands,
                priority,
                interactive,
                bulk,
            },
            ControlLaneReceivers {
                commands: command_receiver,
                priority: priority_receiver,
                interactive: interactive_receiver,
                bulk: bulk_receiver,
            },
        )
    }

    /// Lane for stream data of the given class.
    pub(crate) fn data(&self, interactive: bool) -> &mpsc::Sender<StreamChunk> {
        if interactive {
            &self.interactive
        } else {
            &self.bulk
        }
    }

    /// Admits one ordinary frame without waiting; `false` when the lane is full or closed.
    pub fn try_command(&self, frame: ServerToAgent) -> bool {
        self.commands.try_send(frame).is_ok()
    }

    /// Admits one priority frame without waiting; `false` when the lane is full or closed.
    pub fn try_priority(&self, frame: ServerToAgent) -> bool {
        self.priority.try_send(frame).is_ok()
    }

    /// Admits a frame on the priority lane, falling back to the command lane when the priority
    /// lane is momentarily full, for frames that must not be lost (cancels, publication
    /// decisions); the slower lane still beats dropping the frame.
    pub fn try_priority_or_command(&self, frame: ServerToAgent) -> bool {
        match self.priority.try_send(frame) {
            Ok(()) => true,
            Err(
                mpsc::error::TrySendError::Full(frame) | mpsc::error::TrySendError::Closed(frame),
            ) => self.commands.try_send(frame).is_ok(),
        }
    }

    /// Queues a priority frame from synchronous code without ever dropping it: a full lane
    /// hands the frame to a task that waits for capacity (returned so the owner can abort it
    /// once the frame became pointless).
    pub fn queue_priority(
        &self,
        frame: ServerToAgent,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, LaneClosed> {
        match self.priority.try_send(frame) {
            Ok(()) => Ok(None),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(LaneClosed),
            Err(mpsc::error::TrySendError::Full(frame)) => {
                let priority = self.priority.clone();
                Ok(Some(tokio::spawn(async move {
                    // Closure after admission is settled by connection teardown.
                    let _ = priority.send(frame).await;
                })))
            }
        }
    }

    /// Sends a priority frame from async code, waiting for capacity, for frames whose loss
    /// would corrupt protocol state (a lost credit grant shrinks a stream's window for good).
    pub async fn send_priority(&self, frame: ServerToAgent) -> Result<(), LaneClosed> {
        self.priority.send(frame).await.map_err(|_| LaneClosed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redoor_protocol::ids::RequestId;

    /// A full priority lane retains a queued frame instead of dropping it.
    #[tokio::test]
    async fn queue_priority_retains_frames_on_a_full_lane() {
        let (lanes, mut receivers) = ControlLanes::bounded(1, 1);
        assert!(lanes.try_priority(ServerToAgent::Evicted {
            message: "occupied".to_string()
        }));
        let retained = lanes
            .queue_priority(ServerToAgent::Cancel {
                id: RequestId::new(3),
            })
            .unwrap();
        // The frame waits in a task rather than being lost to momentary saturation.
        assert!(retained.is_some());
        assert!(matches!(
            receivers.priority.recv().await,
            Some(ServerToAgent::Evicted { .. })
        ));
        // Draining one slot delivers the retained cancel, as a typed frame.
        assert!(matches!(
            receivers.priority.recv().await,
            Some(ServerToAgent::Cancel { id: request_id }) if request_id == RequestId::new(3)
        ));
        drop(receivers);
        // A closed lane is reported so callers can keep their state retryable.
        assert!(matches!(
            lanes.queue_priority(ServerToAgent::Evicted {
                message: "late".to_string()
            }),
            Err(LaneClosed)
        ));
    }

    /// Must-deliver frames use the command lane when the priority lane is saturated.
    #[tokio::test]
    async fn priority_frames_fall_back_to_the_command_lane() {
        let (lanes, mut receivers) = ControlLanes::bounded(1, 1);
        for id in [1, 2] {
            assert!(lanes.try_priority_or_command(ServerToAgent::Cancel {
                id: RequestId::new(id),
            }));
        }
        // The first cancel took the priority lane.
        assert!(matches!(
            receivers.priority.try_recv(),
            Ok(ServerToAgent::Cancel { id: request_id }) if request_id == RequestId::new(1)
        ));
        // The second still reached the agent through the command lane.
        assert!(matches!(
            receivers.commands.try_recv(),
            Ok(ServerToAgent::Cancel { id: request_id }) if request_id == RequestId::new(2)
        ));
    }
}
