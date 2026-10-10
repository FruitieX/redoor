//! Bounded memory of agents that failed protocol negotiation.
//!
//! An agent whose protocol version differs from the server's is refused during the websocket
//! upgrade and never registers, so without this list it would be invisible everywhere except
//! the server log.

use redoor_protocol::ids::UnixTimestampMillis;
use redoor_protocol::wire::handshake::ProtocolMismatchAttempt;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

/// How many recent attempts are kept; a misconfigured agent retries forever, so the list must
/// not grow with it.
const MAX_ATTEMPTS: usize = 20;

/// Recent protocol-mismatched connection attempts, shared by the upgrade handlers and REST.
#[derive(Clone, Default)]
pub(crate) struct ProtocolMismatches {
    attempts: Arc<Mutex<VecDeque<ProtocolMismatchAttempt>>>,
}

impl ProtocolMismatches {
    /// Records one rejected attempt.
    ///
    /// A repeat from the same address and version replaces the older entry, so one agent's
    /// retry loop does not push every other agent out of the list.
    pub(crate) fn record(
        &self,
        remote_address: String,
        agent_version: Option<String>,
        offered_protocols: Vec<String>,
    ) {
        let mut attempts = self.attempts.lock().unwrap_or_else(PoisonError::into_inner);
        attempts.retain(|attempt| {
            attempt.remote_address != remote_address || attempt.agent_version != agent_version
        });
        attempts.push_front(ProtocolMismatchAttempt {
            at: UnixTimestampMillis::now(),
            remote_address,
            agent_version,
            offered_protocols,
        });
        attempts.truncate(MAX_ATTEMPTS);
    }

    /// The recorded attempts, newest first.
    pub(crate) fn list(&self) -> Vec<ProtocolMismatchAttempt> {
        self.attempts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the list stays bounded and a retrying agent occupies a single entry.
    #[test]
    fn list_is_bounded_and_deduplicates_retries() {
        let mismatches = ProtocolMismatches::default();
        for _ in 0..3 {
            mismatches.record("10.0.0.1:1".into(), Some("1.0.0".into()), Vec::new());
        }
        // A reconnect loop from one agent must not flood the list.
        assert_eq!(mismatches.list().len(), 1);
        for index in 0..(MAX_ATTEMPTS + 5) {
            mismatches.record(format!("10.0.1.{index}:1"), None, Vec::new());
        }
        let attempts = mismatches.list();
        // Unbounded growth would let any client exhaust server memory.
        assert_eq!(attempts.len(), MAX_ATTEMPTS);
        // The newest attempt is shown first.
        assert_eq!(
            attempts[0].remote_address,
            format!("10.0.1.{}:1", MAX_ATTEMPTS + 4)
        );
    }
}
