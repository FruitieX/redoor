//! Cancellable work owned by the current connection: requests, terminals and log streams.

use super::cancel::CancelRegistry;
use crate::transfers::upload::UploadRoutes;
use redoor_core::log;
use redoor_protocol::{ids::RequestId, wire::log::Level};
use tokio_util::sync::CancellationToken;

/// Bounds terminal streams and their PTYs per connection.
const MAX_TERMINALS: usize = 8;
/// Bounds log streams per connection.
const MAX_LOG_STREAMS: usize = 8;

/// Registries of everything a server may cancel or a disconnect must stop.
pub(crate) struct Sessions {
    /// Every request-cancelable command (uploads, downloads, exec, copies, moves, restores).
    pub(crate) requests: CancelRegistry<RequestId>,
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
            upload_routes,
            terminals: CancelRegistry::new(Some(MAX_TERMINALS)),
            log_streams: CancelRegistry::new(Some(MAX_LOG_STREAMS)),
        }
    }

    /// Ties future registrations to a new connection's cancellation scope.
    pub(crate) fn rebind(&mut self, scope: &CancellationToken) {
        self.requests.rebind(scope.clone());
        self.terminals.rebind(scope.clone());
        self.log_streams.rebind(scope.clone());
        self.upload_routes.clear();
    }

    /// Stops everything owned by the connection that just ended.
    pub(crate) fn invalidate(&mut self) {
        self.requests.revoke_all();
        self.upload_routes.clear();
        self.terminals.revoke_all();
        self.log_streams.revoke_all();
    }

    /// Applies `cancel` to a request, terminal or log stream (they share one id space).
    ///
    /// The server only cancels commands it has already written, and the connection is
    /// ordered, so the command is always known here; a cancel for an id that already finished
    /// is simply ignored, with no state kept. Terminals and log streams give up their slot at
    /// once, because the server frees its own slot when it sends the cancel and may open a
    /// replacement right away.
    /// Workers need the disposition before their cancellation token wakes.
    pub(crate) fn cancel_request_with_reason(
        &mut self,
        request_id: RequestId,
        reason: redoor_protocol::wire::partials::CancelReason,
    ) {
        if self.terminals.revoke(&request_id)
            || self.log_streams.revoke(&request_id)
            || self.requests.cancel_with_reason(&request_id, reason)
        {
            log!(
                Level::Info,
                "Received transfer cancel from server: request_id={}",
                request_id
            );
        } else {
            log!(
                Level::Debug,
                "Ignoring cancel of a request that already finished: request_id={}",
                request_id
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::cancel::RegisterError;

    /// Verifies a cancel reaches terminals, which share the request id space.
    #[test]
    fn cancel_reaches_terminals_and_connection_loss_stops_everything() {
        let mut sessions = Sessions::new(UploadRoutes::default());
        let request = sessions.requests.register(RequestId::new(1)).unwrap();
        let terminal = sessions.terminals.register(RequestId::new(2)).unwrap();

        sessions.cancel_request_with_reason(
            RequestId::new(2),
            redoor_protocol::wire::partials::CancelReason::Canceled,
        );

        // The server closes a terminal stream with the same cancel frame it uses for requests.
        assert!(terminal.token().is_cancelled());
        // Other streams are untouched.
        assert!(!request.token().is_cancelled());

        sessions.invalidate();
        // Connection loss stops everything on the connection.
        assert!(request.token().is_cancelled());
    }

    /// Verifies a cancel for a finished request leaves no state behind.
    #[test]
    fn cancel_of_a_finished_request_is_ignored() {
        let mut sessions = Sessions::new(UploadRoutes::default());
        sessions.cancel_request_with_reason(
            RequestId::new(5),
            redoor_protocol::wire::partials::CancelReason::Canceled,
        );
        let registration = sessions.requests.register(RequestId::new(5)).unwrap();
        // No tombstone exists, so a later command with the same id is not pre-canceled.
        assert!(!registration.token().is_cancelled());
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
