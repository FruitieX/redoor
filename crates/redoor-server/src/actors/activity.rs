//! Inbound-traffic clock of one agent session, shared between the session and the router.
//!
//! The session already tracks when the agent last spoke to decide when a socket is stale. The
//! router needs the same fact when a new `hello` arrives for an id that is still connected: a
//! real reconnect replaces a dead or dying connection, while a cloned machine replaces one
//! that is still answering. Sharing the clock through an atomic lets the router read it
//! without a round trip to the session and without adding a frame per heartbeat to the
//! router mailbox.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::time::{Duration, Instant};

/// When one session last received a frame from its agent, and how recent that must be for
/// the connection to count as healthy.
///
/// Cloning shares the clock, so the session's updates are visible to the router's copy.
/// Uses tokio's clock so router tests can drive it with paused time.
#[derive(Clone, Debug)]
pub struct ConnectionActivity {
    /// The shared clock state.
    inner: Arc<ActivityClock>,
}

/// State behind [`ConnectionActivity`]; the timestamp is an offset from `origin` so it fits
/// in an atomic instead of needing a lock on every inbound frame.
#[derive(Debug)]
struct ActivityClock {
    /// Fixed reference point the stored offset is measured from.
    origin: Instant,
    /// Milliseconds from `origin` to the latest inbound frame.
    last_inbound_millis: AtomicU64,
    /// Longest silence a healthy connection shows; see [`ConnectionActivity::healthy_window`].
    healthy_within: Duration,
}

impl ConnectionActivity {
    /// Starts the clock as if a frame arrived just now, because the session creates it when
    /// the socket opens and the agent's `hello` follows immediately.
    pub fn new(healthy_within: Duration) -> Self {
        Self {
            inner: Arc::new(ActivityClock {
                origin: Instant::now(),
                last_inbound_millis: AtomicU64::new(0),
                healthy_within,
            }),
        }
    }

    /// Longest silence of a healthy connection under these websocket timings.
    ///
    /// Both ends ping every `keepalive` and answer the other side's pings, so a working
    /// connection delivers an inbound frame at least once per keepalive period. Two periods
    /// leave room for one late or lost heartbeat without calling a live connection dead,
    /// and the window never exceeds the stale timeout, after which the session closes the
    /// socket anyway.
    pub fn healthy_window(timeouts: &redoor_core::websocket::WebSocketTimeouts) -> Duration {
        timeouts
            .keepalive
            .saturating_mul(2)
            .min(timeouts.stale_timeout)
    }

    /// Records that a frame from the agent arrived just now.
    pub fn touch(&self) {
        let millis = u64::try_from(self.inner.origin.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.inner
            .last_inbound_millis
            .store(millis, Ordering::Relaxed);
    }

    /// How long the agent has been silent.
    pub fn idle(&self) -> Duration {
        let last = self.inner.origin
            + Duration::from_millis(self.inner.last_inbound_millis.load(Ordering::Relaxed));
        Instant::now().saturating_duration_since(last)
    }

    /// Whether the agent's heartbeat is fresh, i.e. the connection is still in use by a live
    /// process rather than dead or dying.
    pub fn is_healthy(&self) -> bool {
        self.idle() <= self.inner.healthy_within
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The router's duplicate-id detection depends on silence turning a connection unhealthy
    /// and on any frame restoring it.
    #[tokio::test(start_paused = true)]
    async fn silence_past_the_window_makes_a_connection_unhealthy() {
        let activity = ConnectionActivity::new(Duration::from_secs(20));
        let router_view = activity.clone();
        // A freshly opened connection is healthy before it has said anything else.
        assert!(router_view.is_healthy());
        tokio::time::advance(Duration::from_secs(21)).await;
        // Missing two heartbeats means the connection is dead or dying.
        assert!(!router_view.is_healthy());
        activity.touch();
        // A frame seen by the session is visible through the router's clone.
        assert!(router_view.is_healthy());
    }

    /// Production timings allow two missed keepalives but never more than the stale timeout.
    #[test]
    fn healthy_window_is_two_keepalives_capped_by_the_stale_timeout() {
        let production = redoor_core::websocket::WebSocketTimeouts::production();
        // 10 s keepalive gives a 20 s window, below the 30 s stale timeout.
        assert_eq!(
            ConnectionActivity::healthy_window(&production),
            Duration::from_secs(20)
        );
        let short_stale = redoor_core::websocket::WebSocketTimeouts {
            stale_timeout: Duration::from_secs(15),
            ..production
        };
        // A connection the session would already close cannot count as healthy.
        assert_eq!(
            ConnectionActivity::healthy_window(&short_stale),
            Duration::from_secs(15)
        );
    }
}
