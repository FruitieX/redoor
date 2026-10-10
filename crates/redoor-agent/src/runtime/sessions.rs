//! Cancellable work owned by the current connection: requests, terminals and log streams.

use super::cancel::{CancelRegistry, RegisterError, Registration};
use crate::transfers::upload::UploadRoutes;
use redoor_core::log;
use redoor_protocol::{ids::RequestId, wire::log::Level};
use std::collections::{BTreeMap, HashMap};
use tokio_util::sync::CancellationToken;

/// Bounds remembered early cancels. Cancels for requests that already finished also land
/// here and are never consumed, so without a cap they would accumulate for the whole
/// connection. A genuine early cancel is consumed within moments, long before 1024 newer
/// tombstones could push it out.
const MAX_EARLY_CANCELS: usize = 1024;

/// Bounds terminal streams and their PTYs per connection.
const MAX_TERMINALS: usize = 8;
/// Bounds log streams per connection.
const MAX_LOG_STREAMS: usize = 8;

/// Registries of everything a server may cancel or a disconnect must stop.
pub(crate) struct Sessions {
    /// Every request-cancelable command (uploads, downloads, exec, copies, moves, restores).
    pub(crate) requests: CancelRegistry<RequestId>,
    /// Cancels that overtook their command on the independent priority lane.
    pending_cancellations: EarlyCancels,
    /// Chunk and publication routing for upload workers.
    upload_routes: UploadRoutes,
    /// Terminal streams; duplicates and excess sessions are rejected.
    pub(crate) terminals: CancelRegistry<RequestId>,
    /// Log streams; duplicates and excess sessions are rejected.
    pub(crate) log_streams: CancelRegistry<RequestId>,
}

impl Sessions {
    /// Creates empty registries; `upload_routes` is shared with the connection reader.
    pub(crate) fn new(upload_routes: UploadRoutes) -> Self {
        Self {
            requests: CancelRegistry::new(None),
            pending_cancellations: EarlyCancels::default(),
            upload_routes,
            terminals: CancelRegistry::new(Some(MAX_TERMINALS)),
            log_streams: CancelRegistry::new(Some(MAX_LOG_STREAMS)),
        }
    }

    /// Ties future registrations to a new control connection's cancellation scope.
    pub(crate) fn rebind(&mut self, scope: &CancellationToken) {
        self.requests.rebind(scope.clone());
        self.terminals.rebind(scope.clone());
        self.log_streams.rebind(scope.clone());
        self.pending_cancellations.clear();
        self.upload_routes.clear();
    }

    /// Stops everything owned by the connection that just ended.
    pub(crate) fn invalidate(&mut self) {
        self.requests.revoke_all();
        self.upload_routes.clear();
        self.pending_cancellations.clear();
        self.terminals.revoke_all();
        self.log_streams.revoke_all();
    }

    /// Consumes a cancel that arrived before its command, if any.
    pub(crate) fn take_pending_cancel(&mut self, request_id: RequestId) -> bool {
        self.pending_cancellations.take(request_id)
    }

    /// Registers a request-cancelable command; an early cancel applies immediately.
    pub(crate) fn register_request(
        &mut self,
        request_id: RequestId,
        canceled_early: bool,
    ) -> Result<Registration<RequestId>, RegisterError> {
        let registration = self.requests.register(request_id)?;
        if canceled_early {
            registration.cancel();
        }
        Ok(registration)
    }

    /// Applies `cancel`, remembering it when the command has not registered yet.
    ///
    /// Terminals and log streams share the id space with requests, so a cancel for one of
    /// them stops it the same way.
    pub(crate) fn cancel_request(&mut self, request_id: RequestId) {
        if self.terminals.cancel(&request_id) || self.log_streams.cancel(&request_id) {
            return;
        }
        if self.requests.cancel(&request_id) {
            log!(
                Level::Info,
                "Received transfer cancel from server: request_id={}",
                request_id
            );
            return;
        }
        // Priority traffic can overtake the command lane. Keep a tombstone so the worker
        // starts canceled instead of losing the request.
        self.pending_cancellations.remember(request_id);
        log!(
            Level::Info,
            "Retained transfer cancel until command registration: request_id={}",
            request_id
        );
    }
}

/// Insertion-ordered tombstones of cancels that arrived before their command, capped at
/// `MAX_EARLY_CANCELS` by forgetting the oldest.
#[derive(Default)]
struct EarlyCancels {
    /// Tombstone -> insertion sequence, for lookup.
    by_request: HashMap<RequestId, u64>,
    /// Insertion sequence -> tombstone, so the oldest is found without a scan.
    by_age: BTreeMap<u64, RequestId>,
    next_sequence: u64,
}

impl EarlyCancels {
    /// Remembers a cancel, evicting the oldest tombstone once the cap is reached.
    fn remember(&mut self, request_id: RequestId) {
        if self.by_request.contains_key(&request_id) {
            return;
        }
        if self.by_request.len() >= MAX_EARLY_CANCELS
            && let Some((_, oldest)) = self.by_age.pop_first()
        {
            self.by_request.remove(&oldest);
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.by_request.insert(request_id, sequence);
        self.by_age.insert(sequence, request_id);
    }

    /// Consumes the tombstone of `request_id`; returns whether it was present.
    fn take(&mut self, request_id: RequestId) -> bool {
        match self.by_request.remove(&request_id) {
            Some(sequence) => {
                self.by_age.remove(&sequence);
                true
            }
            None => false,
        }
    }

    /// Forgets every tombstone when the connection that issued them ends.
    fn clear(&mut self) {
        self.by_request.clear();
        self.by_age.clear();
    }

    /// Number of remembered tombstones.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.by_request.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cancels for already finished requests must not grow the tombstones without bound,
    /// while the newest (the ones that can still meet their command) are kept.
    #[test]
    fn early_cancel_tombstones_are_bounded() {
        let mut sessions = Sessions::new(UploadRoutes::default());
        let total = MAX_EARLY_CANCELS as u64 + 10;
        for id in 0..total {
            sessions.cancel_request(RequestId::new(id));
        }
        // The cap holds no matter how many unmatched cancels arrive.
        assert_eq!(sessions.pending_cancellations.len(), MAX_EARLY_CANCELS);
        // The oldest tombstones were evicted first.
        assert!(!sessions.take_pending_cancel(RequestId::new(0)));
        // A recent early cancel still applies when its command arrives.
        assert!(sessions.take_pending_cancel(RequestId::new(total - 1)));
        // Consuming a tombstone frees its slot.
        assert_eq!(sessions.pending_cancellations.len(), MAX_EARLY_CANCELS - 1);
    }

    /// Verifies the early-cancel tombstone survives until the command registers.
    #[tokio::test]
    async fn early_cancel_is_remembered_until_registration() {
        let mut sessions = Sessions::new(UploadRoutes::default());

        sessions.cancel_request(RequestId::new(3));
        let canceled_early = sessions.take_pending_cancel(RequestId::new(3));
        let registration = sessions
            .register_request(RequestId::new(3), canceled_early)
            .unwrap();

        // A cancel that overtook its command must still stop the command once it registers.
        assert!(registration.token().is_cancelled());
        // The tombstone is consumed so a later request id reuse starts clean.
        assert!(!sessions.take_pending_cancel(RequestId::new(3)));
    }

    /// Verifies a cancel reaches terminals, which share the request id space.
    #[test]
    fn cancel_reaches_terminals_and_connection_loss_stops_everything() {
        let mut sessions = Sessions::new(UploadRoutes::default());
        let request = sessions.register_request(RequestId::new(1), false).unwrap();
        let terminal = sessions.terminals.register(RequestId::new(2)).unwrap();

        sessions.cancel_request(RequestId::new(2));

        // The server closes a terminal stream with the same cancel frame it uses for requests.
        assert!(terminal.token().is_cancelled());
        // Other streams are untouched.
        assert!(!request.token().is_cancelled());

        sessions.invalidate();
        // Connection loss stops everything on the connection.
        assert!(request.token().is_cancelled());
    }

    /// Verifies log streams are unique, bounded, and freed by RAII.
    #[test]
    fn log_streams_are_bounded_and_unique() {
        let sessions = Sessions::new(UploadRoutes::default());
        let first = sessions.log_streams.register(RequestId::new(1)).unwrap();

        // A duplicate bootstrap must not start a second task for the same viewer.
        assert_eq!(
            sessions.log_streams.register(RequestId::new(1)).err(),
            Some(RegisterError::Duplicate)
        );
        let _others: Vec<_> = (2..=MAX_LOG_STREAMS as u128)
            .map(|id| {
                sessions
                    .log_streams
                    .register(RequestId::new(id as u64))
                    .unwrap()
            })
            .collect();
        // The cap bounds dedicated sockets per agent.
        assert_eq!(
            sessions.log_streams.register(RequestId::new(99)).err(),
            Some(RegisterError::Full)
        );
        drop(first);
        // Completion frees capacity without explicit bookkeeping.
        assert!(sessions.log_streams.register(RequestId::new(99)).is_ok());
    }
}
