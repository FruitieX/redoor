//! Keyed cancellation registry shared by request, terminal and log-stream workers.
//!
//! Replaces several hand-rolled `Arc<Mutex<HashMap<_, watch::Sender<bool>>>>` maps whose
//! insert/remove/clear bookkeeping was repeated per feature and easy to leak on early returns.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex, Weak};
use tokio_util::sync::CancellationToken;

/// Why a key could not be registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegisterError {
    /// Another live worker already owns the key.
    Duplicate,
    /// The registry's bounded capacity is exhausted.
    Full,
}

impl RegisterError {
    /// Short reason for rejection logs, so operators can tell a replayed bootstrap from a
    /// session limit.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Duplicate => "already running",
            Self::Full => "session limit reached",
        }
    }
}

/// Mutable registry contents guarded by one mutex so registration and revocation are atomic.
struct RegistryState<K> {
    /// Connection-scoped parent; cancelling it stops every generation created from it.
    scope: CancellationToken,
    /// Parent of all current registrations; replaced whenever the registry is revoked.
    generation: CancellationToken,
    /// Live registrations keyed by their public identifier.
    entries: HashMap<K, RegistryEntry>,
    /// Distinguishes a re-registered key from a stale guard that is still being dropped.
    next_serial: u64,
}

/// One registered worker's cancellation handle.
struct RegistryEntry {
    serial: u64,
    token: CancellationToken,
}

/// Registry of cancellable workers keyed by request, terminal or log-stream id.
#[derive(Clone)]
pub(crate) struct CancelRegistry<K> {
    shared: Arc<Mutex<RegistryState<K>>>,
    /// Bounds concurrently registered workers where each one owns external resources.
    capacity: Option<usize>,
}

impl<K: Eq + Hash + Clone> CancelRegistry<K> {
    /// Creates an empty registry, optionally bounded to `capacity` live entries.
    pub(crate) fn new(capacity: Option<usize>) -> Self {
        let scope = CancellationToken::new();
        let generation = scope.child_token();
        Self {
            shared: Arc::new(Mutex::new(RegistryState {
                scope,
                generation,
                entries: HashMap::new(),
                next_serial: 0,
            })),
            capacity,
        }
    }

    /// Locks the state, recovering from poison because entries stay consistent across panics.
    fn state(&self) -> std::sync::MutexGuard<'_, RegistryState<K>> {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Registers `key` and returns the RAII guard that unregisters it on drop.
    pub(crate) fn register(&self, key: K) -> Result<Registration<K>, RegisterError> {
        let mut state = self.state();
        if state.entries.contains_key(&key) {
            return Err(RegisterError::Duplicate);
        }
        if self
            .capacity
            .is_some_and(|capacity| state.entries.len() >= capacity)
        {
            return Err(RegisterError::Full);
        }
        state.next_serial = state.next_serial.wrapping_add(1);
        let serial = state.next_serial;
        let token = state.generation.child_token();
        state.entries.insert(
            key.clone(),
            RegistryEntry {
                serial,
                token: token.clone(),
            },
        );
        Ok(Registration {
            key,
            serial,
            token,
            registry: Arc::downgrade(&self.shared),
        })
    }

    /// Cancels one live worker; returns `false` when no worker owns `key`.
    pub(crate) fn cancel(&self, key: &K) -> bool {
        match self.state().entries.get(key) {
            Some(entry) => {
                entry.token.cancel();
                true
            }
            None => false,
        }
    }

    /// Cancels one live worker and frees its key at once, before the worker has finished;
    /// returns `false` when no worker owns `key`.
    ///
    /// For streams the server has already forgotten (closed terminals and log streams), so a
    /// new stream can take the slot while the old one is still tearing down, exactly as the
    /// server's own slot accounting expects. The old worker's guard finds a different (or no)
    /// entry and leaves it alone.
    pub(crate) fn revoke(&self, key: &K) -> bool {
        match self.state().entries.remove(key) {
            Some(entry) => {
                entry.token.cancel();
                true
            }
            None => false,
        }
    }

    /// Cancels every current worker and forgets them so a new generation starts empty.
    pub(crate) fn revoke_all(&self) {
        let mut state = self.state();
        state.generation.cancel();
        state.entries.clear();
        state.generation = state.scope.child_token();
    }

    /// Revokes everything and attaches future registrations to a new connection scope.
    pub(crate) fn rebind(&self, scope: CancellationToken) {
        let mut state = self.state();
        state.generation.cancel();
        state.entries.clear();
        state.generation = scope.child_token();
        state.scope = scope;
    }

    /// Reports whether `key` is currently registered.
    #[cfg(test)]
    pub(crate) fn contains(&self, key: &K) -> bool {
        self.state().entries.contains_key(key)
    }

    /// Counts live registrations for capacity and cleanup tests.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.state().entries.len()
    }
}

/// Ownership of one registered key; dropping it unregisters the key.
pub(crate) struct Registration<K: Eq + Hash> {
    key: K,
    serial: u64,
    /// Cancelled by an explicit cancel, a revocation or connection loss.
    token: CancellationToken,
    /// Weak so a registration outliving its registry cannot keep the map alive.
    registry: Weak<Mutex<RegistryState<K>>>,
}

impl<K: Eq + Hash> Registration<K> {
    /// Token the worker observes for cooperative cancellation.
    pub(crate) fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// Cancels this registration directly, for tests that model a cancel arriving.
    #[cfg(test)]
    pub(crate) fn cancel(&self) {
        self.token.cancel();
    }

    /// Registered key.
    pub(crate) fn key(&self) -> &K {
        &self.key
    }
}

impl<K: Eq + Hash> Drop for Registration<K> {
    /// Removes the key unless a newer registration has already replaced it.
    fn drop(&mut self) {
        let Some(shared) = self.registry.upgrade() else {
            return;
        };
        let mut state = shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .entries
            .get(&self.key)
            .is_some_and(|entry| entry.serial == self.serial)
        {
            state.entries.remove(&self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies RAII unregistration so early returns cannot leak cancel handles.
    #[test]
    fn dropping_a_registration_unregisters_its_key() {
        let registry = CancelRegistry::new(None);
        let registration = registry.register(1).unwrap();

        // A live worker is reachable for cancellation.
        assert!(registry.contains(&1));
        drop(registration);
        // Early returns and panics release the key without explicit bookkeeping.
        assert!(!registry.contains(&1));
        // A later cancel for the finished request finds nothing to signal.
        assert!(!registry.cancel(&1));
    }

    /// Verifies explicit cancellation is request-scoped rather than generation-wide.
    #[test]
    fn cancel_targets_one_key_and_leaves_siblings_running() {
        let registry = CancelRegistry::new(None);
        let first = registry.register(1).unwrap();
        let second = registry.register(2).unwrap();

        // A known key accepts cancellation synchronously.
        assert!(registry.cancel(&1));
        // The selected worker observes its own cancellation.
        assert!(first.token().is_cancelled());
        // Unrelated workers keep running.
        assert!(!second.token().is_cancelled());
    }

    /// Verifies key uniqueness and the optional capacity bound.
    #[test]
    fn duplicates_and_capacity_are_rejected() {
        let registry = CancelRegistry::new(Some(1));
        let _first = registry.register(1).unwrap();

        // One worker per key prevents two tasks from sharing one cancellation identity.
        assert_eq!(registry.register(1).err(), Some(RegisterError::Duplicate));
        // Bounded registries reject excess workers instead of growing without limit.
        assert_eq!(registry.register(2).err(), Some(RegisterError::Full));
    }

    /// Verifies teardown cancels every worker and frees capacity immediately.
    #[test]
    fn revoke_all_cancels_and_forgets_every_worker() {
        let registry = CancelRegistry::new(None);
        let first = registry.register(1).unwrap();
        let second = registry.register(2).unwrap();

        registry.revoke_all();

        // Every worker of the torn-down generation must stop.
        assert!(first.token().is_cancelled() && second.token().is_cancelled());
        // Capacity is released immediately rather than when tasks finish.
        assert_eq!(registry.len(), 0);
        // A new generation starts uncancelled.
        assert!(!registry.register(1).unwrap().token().is_cancelled());
    }

    /// Verifies a late guard drop cannot unregister a replacement worker.
    #[test]
    fn stale_guard_does_not_remove_a_reregistered_key() {
        let registry = CancelRegistry::new(None);
        let stale = registry.register(1).unwrap();
        registry.revoke_all();
        let fresh = registry.register(1).unwrap();

        drop(stale);

        // The old guard's late drop must not unregister the replacement worker.
        assert!(registry.contains(&1));
        drop(fresh);
        assert!(!registry.contains(&1));
    }

    /// Verifies registrations inherit cancellation from their connection scope.
    #[test]
    fn cancelling_the_connection_scope_reaches_registered_workers() {
        let registry = CancelRegistry::new(None);
        let scope = CancellationToken::new();
        registry.rebind(scope.clone());
        let registration = registry.register(1).unwrap();

        scope.cancel();

        // Connection loss alone must stop workers even before the registry is revoked.
        assert!(registration.token().is_cancelled());
    }

    /// Revoking frees the key at once, and the revoked worker's guard leaves a newer
    /// registration of the same key alone.
    #[test]
    fn revoke_frees_the_key_before_the_worker_finishes() {
        let registry = CancelRegistry::new(Some(1));
        let old = registry.register(1).unwrap();
        assert!(registry.revoke(&1));
        // The revoked worker is told to stop.
        assert!(old.token().is_cancelled());
        // The slot is free for a replacement while the old worker is still tearing down.
        let new = registry.register(1).unwrap();
        drop(old);
        // The old guard does not remove the replacement.
        assert!(registry.contains(&1));
        drop(new);
        assert!(!registry.contains(&1));
    }
}
