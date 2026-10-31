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

/// One registry entry: the session plus the lease it refreshes.
type RegistryEntry = (Arc<dyn ManagedRealtimeSession>, SessionLease);

/// Registry of live lease-owned sessions keyed by session id.
#[derive(Default)]
pub struct RealtimeSessionRegistry {
    sessions: Mutex<HashMap<String, RegistryEntry>>,
}

impl RealtimeSessionRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a session and opportunistically reaps expired entries so
    /// registries without a watchdog (tests, short-lived processes) still
    /// release sessions eventually.
    pub fn insert(&self, session_id: String, session: Arc<dyn ManagedRealtimeSession>) {
        {
            let mut sessions = self.sessions.lock();
            sessions.retain(|_, (_, lease)| !lease.expired());
            sessions.insert(session_id, (session, SessionLease::new()));
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
        {
            let mut sessions = self.sessions.lock();
            sessions.retain(|_, (_, existing_lease)| !existing_lease.expired());
            sessions.insert(session_id, (session, lease));
        }
    }

    #[must_use]
    pub fn get(&self, session_id: &str) -> Option<Arc<dyn ManagedRealtimeSession>> {
        self.sessions
            .lock()
            .get(session_id)
            .map(|(session, _)| Arc::clone(session))
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
                .filter(|(_, (session, lease))| lease.expired() || session.lease_expired())
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in expired {
            if let Some((session, _)) = self.sessions.lock().remove(&id) {
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
                            let drained = guard.drain().map(|(_, (session, _))| session).collect();
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

    struct FakeSession {
        cancelled: AtomicBool,
        refreshes: AtomicUsize,
    }

    impl ManagedRealtimeSession for FakeSession {
        fn refresh_lease(&self) {
            self.refreshes.fetch_add(1, Ordering::Relaxed);
        }

        fn lease_expired(&self) -> bool {
            false
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
        let lease = SessionLease::new();
        lease.expire_immediately();
        let session = fake_session();
        registry.insert_with_shared_lease(
            "s1".to_string(),
            Arc::clone(&session) as Arc<dyn ManagedRealtimeSession>,
            lease,
        );

        registry.sweep_once();

        assert!(session.cancelled.load(Ordering::Relaxed));
        assert!(registry.is_empty());
    }

    #[test]
    fn sweep_keeps_fresh_leases() {
        let registry = RealtimeSessionRegistry::new();
        let lease = SessionLease::new();
        lease.refresh();
        registry.insert_with_shared_lease("s1".to_string(), fake_session(), lease);
        registry.sweep_once();
        assert_eq!(registry.len(), 1, "fresh leases must survive the sweep");
    }
}
