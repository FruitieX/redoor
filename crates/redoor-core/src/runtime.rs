//! Process-wide settings resolved once in `main` and handed to every role.

use crate::paths::AppPaths;
use crate::websocket::WebSocketTimeouts;

/// Startup choices every role needs, passed down instead of read from globals.
///
/// Built once from the root CLI flags so the server, agent, relay, CLI and service
/// installers observe the same namespace and timings, and so tests can construct an
/// isolated context (temporary home, short timeouts) without process-wide state.
#[derive(Clone, Debug)]
pub struct RuntimeContext {
    /// Application namespace and the directories derived from it.
    pub paths: AppPaths,
    /// Idle-socket timings; tests shrink them via CLI flags or environment.
    pub ws_timeouts: WebSocketTimeouts,
}

impl RuntimeContext {
    /// Context for unit tests: a throwaway home and production websocket timings.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(home: impl Into<std::path::PathBuf>) -> Self {
        Self {
            paths: AppPaths::for_home("redoor", home),
            ws_timeouts: WebSocketTimeouts::production(),
        }
    }
}
