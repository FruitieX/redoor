//! Cancellable work owned by the current connection: requests, terminals and log streams.

use super::cancel::{CancelRegistry, RegisterError, Registration};
use crate::transfers::upload::UploadRoutes;
use redoor_core::log;
use redoor_protocol::{
    ids::RequestId,
    wire::{log::Level, log::LogStreamId, terminal::TerminalId},
};
use std::collections::HashSet;
use tokio_util::sync::CancellationToken;

/// Bounds dedicated terminal sockets and PTYs per agent.
const MAX_TERMINALS: usize = 8;
/// Bounds dedicated log-stream sockets per agent.
const MAX_LOG_STREAMS: usize = 8;

/// Registries of everything a server may cancel or a disconnect must stop.
pub(crate) struct Sessions {
    /// Every request-cancelable command (uploads, downloads, exec, copies, moves, restores).
    pub(crate) requests: CancelRegistry<RequestId>,
    /// Cancels that overtook their command on the independent priority lane.
    pending_cancellations: HashSet<RequestId>,
    /// Chunk and publication routing for upload workers.
    upload_routes: UploadRoutes,
    /// Dedicated terminal sockets; duplicates and excess sessions are rejected.
    pub(crate) terminals: CancelRegistry<TerminalId>,
    /// Dedicated log-stream sockets; duplicates and excess sessions are rejected.
    pub(crate) log_streams: CancelRegistry<LogStreamId>,
}

impl Sessions {
    /// Creates empty registries; `upload_routes` is shared with the transfer socket reader.
    pub(crate) fn new(upload_routes: UploadRoutes) -> Self {
        Self {
            requests: CancelRegistry::new(None),
            pending_cancellations: HashSet::new(),
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

    /// Stops request work after the transfer socket was lost; terminals and logs keep running
    /// because they use their own dedicated sockets.
    pub(crate) fn invalidate_transfers(&mut self) {
        self.requests.revoke_all();
        self.upload_routes.clear();
        self.pending_cancellations.clear();
    }

    /// Stops everything owned by the control connection that just ended.
    pub(crate) fn invalidate(&mut self) {
        self.invalidate_transfers();
        self.terminals.revoke_all();
        self.log_streams.revoke_all();
    }

    /// Consumes a cancel that arrived before its command, if any.
    pub(crate) fn take_pending_cancel(&mut self, request_id: RequestId) -> bool {
        self.pending_cancellations.remove(&request_id)
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

    /// Applies `cancel_transfer`, remembering it when the command has not registered yet.
    pub(crate) fn cancel_request(&mut self, request_id: RequestId) {
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
        self.pending_cancellations.insert(request_id);
        log!(
            Level::Info,
            "Retained transfer cancel until command registration: request_id={}",
            request_id
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// Verifies the early-cancel tombstone survives until the command registers.
    #[tokio::test]
    async fn early_cancel_is_remembered_until_registration() {
        redoor_core::logging::init(None).await.unwrap();
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

    /// Verifies transfer loss stops request work but not dedicated sockets.
    #[test]
    fn transfer_loss_keeps_dedicated_sockets_running() {
        let mut sessions = Sessions::new(UploadRoutes::default());
        let request = sessions.register_request(RequestId::new(1), false).unwrap();
        let terminal = sessions
            .terminals
            .register(TerminalId(Uuid::from_u128(1)))
            .unwrap();

        sessions.invalidate_transfers();

        // Request workers depend on the lost transfer socket and must stop.
        assert!(request.token().is_cancelled());
        // Terminals use their own sockets and survive payload-socket loss.
        assert!(!terminal.token().is_cancelled());

        sessions.invalidate();
        // Control loss stops everything, including dedicated sockets.
        assert!(terminal.token().is_cancelled());
    }

    /// Verifies log streams are unique, bounded, and freed by RAII.
    #[test]
    fn log_streams_are_bounded_and_unique() {
        let sessions = Sessions::new(UploadRoutes::default());
        let first = sessions
            .log_streams
            .register(LogStreamId(Uuid::from_u128(1)))
            .unwrap();

        // A duplicate bootstrap must not start a second task for the same viewer.
        assert_eq!(
            sessions
                .log_streams
                .register(LogStreamId(Uuid::from_u128(1)))
                .err(),
            Some(RegisterError::Duplicate)
        );
        let _others: Vec<_> = (2..=MAX_LOG_STREAMS as u128)
            .map(|id| {
                sessions
                    .log_streams
                    .register(LogStreamId(Uuid::from_u128(id)))
                    .unwrap()
            })
            .collect();
        // The cap bounds dedicated sockets per agent.
        assert_eq!(
            sessions
                .log_streams
                .register(LogStreamId(Uuid::from_u128(99)))
                .err(),
            Some(RegisterError::Full)
        );
        drop(first);
        // Completion frees capacity without explicit bookkeeping.
        assert!(
            sessions
                .log_streams
                .register(LogStreamId(Uuid::from_u128(99)))
                .is_ok()
        );
    }
}
