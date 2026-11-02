//! Transport-agnostic registry for lease-owned realtime sessions.
//!
//! Socket transports (WebSocket, gRPC, WebTransport) tie session lifetime to
//! their stream. The HTTP realtime session (SSE downlink + REST uplink) has
//! no socket, so its `StreamMessageHandler` is kept alive by a lease that the
//! SSE attach and incoming message posts refresh. This registry tracks those
//! lease-owned sessions for endpoint routing and expiry sweeps.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Default lease window: the client heartbeat interval (25s) plus slack.
pub const REALTIME_SESSION_LEASE_TTL: Duration = Duration::from_secs(30);

/// A lease-owned realtime session the registry can manage.
pub trait ManagedRealtimeSession: Send + Sync {
    /// Marks the session as recently used, extending its lease.
    fn refresh_lease(&self);

    /// True when the session may be reclaimed (no attachment and the lease
    /// window elapsed, or the underlying handler already stopped).
    fn lease_expired(&self) -> bool;

    /// Cancels the session's handler (presence leave, cleanup).
    fn cancel(&self);

    /// Downcast support so the owning transport can recover its concrete
    /// session type from the registry.
    fn into_any_arc(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync>;
}

/// Shared lease cell used by both the session and its registry entry.
///
/// Cloning shares the deadline, so refreshes from the transport (message
/// posts, SSE attaches) and sweeps from the registry see the same state.
#[derive(Debug, Clone)]
pub struct SessionLease {
    deadline: Arc<Mutex<Instant>>,
}

impl Default for SessionLease {
    fn default() -> Self {
        Self {
            deadline: Arc::new(Mutex::new(Instant::now() + REALTIME_SESSION_LEASE_TTL)),
        }
    }
}

impl SessionLease {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn refresh(&self) {
        *self.deadline.lock() = Instant::now() + REALTIME_SESSION_LEASE_TTL;
    }

    #[must_use]
    pub fn expired(&self) -> bool {
        *self.deadline.lock() < Instant::now()
    }

    #[cfg(test)]
    fn expire_immediately(&self) {
        let past = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("now is always after 1s past the epoch");
        *self.deadline.lock() = past;
    }
}

/// One registry entry: the session, the lease it refreshes, and whether that
/// lease is shared with the session itself (attachment-aware expiry lives in
/// the session) or registry-owned (the lease is the only expiry signal).
type RegistryEntry = (Arc<dyn ManagedRealtimeSession>, SessionLease, bool);

/// Registry of live lease-owned sessions keyed by session id.
#[derive(Default)]
pub struct RealtimeSessionRegistry {
    sessions: Mutex<HashMap<String, RegistryEntry>>,
}

/// True when an entry may be reclaimed. Shared leases defer to the session:
/// its `lease_expired` knows whether a live attachment suspends expiry, and
/// overruling it here would cancel attached-but-quiet sessions.
fn entry_expired(
    session: &dyn ManagedRealtimeSession,
    lease: &SessionLease,
    shared_lease: bool,
) -> bool {
    if shared_lease {
        session.lease_expired()
    } else {
        lease.expired() || session.lease_expired()
    }
}

impl RealtimeSessionRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a session and opportunistically reaps expired entries so
    /// registries without a watchdog (tests, short-lived processes) still
    /// release sessions eventually. Reaped entries are cancelled outside the
    /// registry lock so their cleanup can never deadlock against a re-entrant
    /// registry call.
    pub fn insert(&self, session_id: String, session: Arc<dyn ManagedRealtimeSession>) {
        let reaped = {
            let mut sessions = self.sessions.lock();
            let mut reaped = Vec::new();
            sessions.retain(|_, (existing, lease, shared)| {
                if entry_expired(existing.as_ref(), lease, *shared) {
                    reaped.push(Arc::clone(existing));
                    false
                } else {
                    true
                }
            });
            sessions.insert(session_id, (session, SessionLease::new(), false));
            reaped
        };
        for session in reaped {
            session.cancel();
        }
    }

    /// Registers a session with an externally shared lease (refreshed by the
    /// transport itself), then reaps expired entries.
    pub fn insert_with_shared_lease(
        &self,
        session_id: String,
        session: Arc<dyn ManagedRealtimeSession>,
        lease: SessionLease,
    ) {
        let reaped = {
            let mut sessions = self.sessions.lock();
            let mut reaped = Vec::new();
            sessions.retain(|_, (existing, existing_lease, shared)| {
                if entry_expired(existing.as_ref(), existing_lease, *shared) {
                    reaped.push(Arc::clone(existing));
                    false
                } else {
                    true
                }
            });
            sessions.insert(session_id, (session, lease, true));
            reaped
        };
        for session in reaped {
            session.cancel();
        }
    }

    #[must_use]
    pub fn get(&self, session_id: &str) -> Option<Arc<dyn ManagedRealtimeSession>> {
        self.sessions
            .lock()
            .get(session_id)
            .map(|(session, ..)| Arc::clone(session))
    }

    pub fn remove(&self, session_id: &str) -> bool {
        self.sessions.lock().remove(session_id).is_some()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.lock().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.lock().is_empty()
    }

    /// Ends sessions whose lease expired without a refresh and returns how
    /// many were reaped.
    fn sweep_once(&self) -> usize {
        let expired: Vec<String> = {
            let sessions = self.sessions.lock();
            sessions
                .iter()
                .filter(|(_, (session, lease, shared))| {
                    entry_expired(session.as_ref(), lease, *shared)
                })
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in expired {
            if let Some((session, ..)) = self.sessions.lock().remove(&id) {
                session.cancel();
            }
        }
        self.len()
    }

    /// Spawns the periodic sweep; cancels every remaining session when
    /// `shutdown` fires (graceful server shutdown).
    pub fn spawn_watchdog(self: &Arc<Self>, shutdown: tokio_util::sync::CancellationToken) {
        let registry = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(5));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _instant = ticker.tick() => {
                        registry.sweep_once();
                    }
                    () = shutdown.cancelled() => {
                        let sessions: Vec<Arc<dyn ManagedRealtimeSession>> = {
                            let mut guard = registry.sessions.lock();
                            let drained = guard.drain().map(|(_, (session, ..))| session).collect();
                            drained
                        };
                        for session in sessions {
                            session.cancel();
                        }
                        return;
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Models the HTTP realtime session's expiry semantics: attachment
    /// suspends lease expiry, detachment re-arms it.
    struct FakeSession {
        cancelled: AtomicBool,
        refreshes: AtomicUsize,
        attached: AtomicBool,
        lease: SessionLease,
    }

    impl ManagedRealtimeSession for FakeSession {
        fn refresh_lease(&self) {
            self.refreshes.fetch_add(1, Ordering::Relaxed);
            self.lease.refresh();
        }

        fn lease_expired(&self) -> bool {
            !self.attached.load(Ordering::Relaxed) && self.lease.expired()
        }

        fn cancel(&self) {
            self.cancelled.store(true, Ordering::Relaxed);
        }

        fn into_any_arc(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
            self
        }
    }

    fn fake_session() -> Arc<FakeSession> {
        Arc::new(FakeSession {
            cancelled: AtomicBool::new(false),
            refreshes: AtomicUsize::new(0),
            attached: AtomicBool::new(true),
            lease: SessionLease::new(),
        })
    }

    #[test]
    fn insert_registers_session() {
        let registry = RealtimeSessionRegistry::new();
        registry.insert("s1".to_string(), fake_session());
        assert_eq!(registry.len(), 1);
        assert!(registry.get("s1").is_some());
        assert!(registry.get("missing").is_none());
    }

    #[test]
    fn sweep_cancels_expired_leases() {
        let registry = RealtimeSessionRegistry::new();
        let session = fake_session();
        session.attached.store(false, Ordering::Relaxed);
        session.lease.expire_immediately();
        // The shared lease is the session's own: the registry entry and the
        // session see the same deadline.
        registry.insert_with_shared_lease(
            "s1".to_string(),
            Arc::clone(&session) as Arc<dyn ManagedRealtimeSession>,
            session.lease.clone(),
        );

        registry.sweep_once();

        assert!(session.cancelled.load(Ordering::Relaxed));
        assert!(registry.is_empty());
    }

    #[test]
    fn sweep_keeps_fresh_leases() {
        let registry = RealtimeSessionRegistry::new();
        let session = fake_session();
        session.attached.store(false, Ordering::Relaxed);
        session.lease.refresh();
        registry.insert_with_shared_lease(
            "s1".to_string(),
            Arc::clone(&session) as Arc<dyn ManagedRealtimeSession>,
            session.lease.clone(),
        );
        registry.sweep_once();
        assert_eq!(registry.len(), 1, "fresh leases must survive the sweep");
    }

    #[test]
    fn sweep_keeps_attached_sessions_with_expired_leases() {
        // Regression (sweep-cancelled-attached-session): an attached SSE
        // downlink suspends lease expiry — the unconditional registry-side
        // lease check used to cancel attached-but-quiet sessions.
        let registry = RealtimeSessionRegistry::new();
        let session = fake_session();
        session.lease.expire_immediately();
        registry.insert_with_shared_lease(
            "s1".to_string(),
            Arc::clone(&session) as Arc<dyn ManagedRealtimeSession>,
            session.lease.clone(),
        );

        registry.sweep_once();

        assert!(
            !session.cancelled.load(Ordering::Relaxed),
            "attachment must suspend lease expiry"
        );
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn insert_cancels_entries_it_reaps() {
        // Regression (insert-reap-orphaned-session): the insert-time reap
        // used to drop expired entries without cancelling them, orphaning
        // their handlers (presence, room membership) until process exit.
        let registry = RealtimeSessionRegistry::new();
        let stale = fake_session();
        stale.attached.store(false, Ordering::Relaxed);
        stale.lease.expire_immediately();
        registry.insert_with_shared_lease(
            "s1".to_string(),
            Arc::clone(&stale) as Arc<dyn ManagedRealtimeSession>,
            stale.lease.clone(),
        );

        registry.insert_with_shared_lease(
            "s2".to_string(),
            fake_session() as Arc<_>,
            SessionLease::new(),
        );

        assert!(
            stale.cancelled.load(Ordering::Relaxed),
            "the reaped entry must be cancelled, not just dropped"
        );
        assert!(registry.get("s1").is_none());
        assert!(registry.get("s2").is_some());
    }
}
