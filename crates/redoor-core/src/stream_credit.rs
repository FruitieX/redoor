//! Per-stream flow-control credit shared by the agent and server transfer paths.
//!
//! Many streams share one agent connection. Without per-stream credit, a receiver that cannot
//! hand a frame to a slow consumer has to stop reading the socket, which freezes every other
//! stream on it. With credit, a sender may only have a bounded number of non-terminal frames
//! outstanding per stream, so the receiver can always accept a frame into that stream's own
//! bounded queue and keep reading the socket.

use redoor_protocol::wire::stream::{STREAM_CREDIT_BATCH_FRAMES, STREAM_CREDIT_WINDOW_FRAMES};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Sender side of one credited stream: the frames it may still send before waiting.
///
/// Cloned between the producer that spends credit and the control-lane handler that returns
/// it, so grants never have to pass through an actor mailbox.
#[derive(Clone, Debug)]
pub struct CreditGate {
    permits: Arc<Semaphore>,
    /// Largest balance the receiver may grant, so a misbehaving peer cannot inflate it.
    window: usize,
}

impl Default for CreditGate {
    /// A fresh request stream starts with the full window so it can begin sending immediately.
    fn default() -> Self {
        Self::with_window(STREAM_CREDIT_WINDOW_FRAMES)
    }
}

impl CreditGate {
    /// A gate whose stream may hold up to `window` frames but starts with none, for streams
    /// whose receiver grants the first credit once it is ready (uploads).
    pub fn awaiting_grant(window: u32) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(0)),
            window: window as usize,
        }
    }

    /// A gate whose stream starts with, and may never hold more than, `window` frames.
    pub fn with_window(window: u32) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(window as usize)),
            window: window as usize,
        }
    }

    /// Waits for one frame of credit and spends it.
    ///
    /// Returns `false` once the gate was closed because the stream ended, so a producer blocked
    /// on credit is released instead of waiting for grants that can never arrive.
    pub async fn acquire(&self) -> bool {
        match self.permits.acquire().await {
            Ok(permit) => {
                permit.forget();
                true
            }
            Err(_) => false,
        }
    }

    /// Returns credit from the receiver, never raising the balance above the window.
    ///
    /// An honest receiver only returns frames it drained, so the clamp only matters for a
    /// misbehaving peer, which must not be able to make this side buffer without bound.
    pub fn grant(&self, frames: u32) {
        let room = self.window.saturating_sub(self.permits.available_permits());
        let granted = (frames as usize).min(room);
        if granted > 0 {
            self.permits.add_permits(granted);
        }
    }

    /// Releases any producer waiting for credit because the stream will never get more.
    pub fn close(&self) {
        self.permits.close();
    }

    /// Credit currently available, for tests and diagnostics.
    pub fn available(&self) -> usize {
        self.permits.available_permits()
    }
}

/// Receiver side of one credited stream: counts drained frames and batches the grants.
///
/// Returning credit for every frame would double control traffic; batching keeps grants rare
/// while the batch size (at most the window) guarantees a drained receiver never withholds
/// all of the sender's credit.
#[derive(Debug, Default)]
pub struct CreditLedger {
    owed: u32,
}

impl CreditLedger {
    /// Records one drained frame and returns the number of frames to grant when a batch is due.
    ///
    /// Terminal frames never consumed credit, so draining them owes nothing.
    pub fn drained(&mut self, consumed_credit: bool) -> Option<u32> {
        if !consumed_credit {
            return None;
        }
        self.owed += 1;
        if self.owed < STREAM_CREDIT_BATCH_FRAMES {
            return None;
        }
        Some(std::mem::take(&mut self.owed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spends one frame of credit without waiting, reporting whether credit was available.
    fn try_spend(gate: &CreditGate) -> bool {
        gate.permits
            .try_acquire()
            .map(tokio::sync::SemaphorePermit::forget)
            .is_ok()
    }

    /// The window bounds frames in flight and grants release exactly the returned frames.
    #[tokio::test]
    async fn gate_blocks_after_window_until_credit_returns() {
        let gate = CreditGate::default();
        for _ in 0..STREAM_CREDIT_WINDOW_FRAMES {
            // The initial window lets a new stream start without a round trip.
            assert!(gate.acquire().await);
        }
        // An exhausted window must make the producer wait instead of sending more.
        assert!(!try_spend(&gate));
        gate.grant(1);
        // One returned frame admits exactly one more send.
        assert!(try_spend(&gate));
        assert!(!try_spend(&gate));
    }

    /// A peer cannot inflate the window by over-granting.
    #[test]
    fn grants_are_clamped_to_the_window() {
        let gate = CreditGate::default();
        gate.grant(1000);
        // Over-granting would let a misbehaving receiver force unbounded buffering here.
        assert_eq!(gate.available(), STREAM_CREDIT_WINDOW_FRAMES as usize);
    }

    /// Larger windows (terminals) clamp grants to their own size.
    #[test]
    fn custom_windows_clamp_to_their_size() {
        let gate = CreditGate::with_window(16);
        // A terminal stream starts with its whole window.
        assert_eq!(gate.available(), 16);
        gate.grant(100);
        // Over-granting cannot exceed the stream's own window.
        assert_eq!(gate.available(), 16);
    }

    /// Closing the gate releases a producer that would otherwise wait forever.
    #[tokio::test]
    async fn close_releases_waiting_producer() {
        let gate = CreditGate::default();
        for _ in 0..STREAM_CREDIT_WINDOW_FRAMES {
            gate.acquire().await;
        }
        let waiter = tokio::spawn({
            let gate = gate.clone();
            async move { gate.acquire().await }
        });
        gate.close();
        // A closed stream reports failure so the producer stops instead of hanging.
        assert!(!waiter.await.unwrap());
    }

    /// Grants are batched and terminal frames never owe credit.
    #[test]
    fn ledger_batches_grants_and_ignores_terminal_frames() {
        let mut ledger = CreditLedger::default();
        // Terminal frames were exempt from credit, so draining them must not return any.
        assert_eq!(ledger.drained(false), None);
        // A single drained frame waits for the batch to fill.
        assert_eq!(ledger.drained(true), None);
        // The batch is returned in one grant once enough frames were drained.
        assert_eq!(ledger.drained(true), Some(STREAM_CREDIT_BATCH_FRAMES));
        assert_eq!(ledger.drained(true), None);
    }
}
