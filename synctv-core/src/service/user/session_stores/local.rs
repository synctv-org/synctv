use std::time::Duration;

use crate::Result;

use super::{
    MfaSession, MfaSessionStore, OpaqueLoginSession, OpaqueLoginSessionStore,
    OpaqueRegistrationSession, OpaqueRegistrationSessionStore, SensitiveVerificationSession,
    SensitiveVerificationSessionStore,
};
use crate::service::user::session_types::{
    MFA_SESSION_CAPACITY, OPAQUE_LOGIN_SESSION_CAPACITY, OPAQUE_REGISTRATION_SESSION_CAPACITY,
    SENSITIVE_VERIFICATION_SESSION_CAPACITY,
};

#[derive(Clone)]
struct InMemorySessionEntry<T> {
    session: T,
    ttl: Duration,
}

struct InMemorySessionExpiry;

impl<T> moka::Expiry<String, InMemorySessionEntry<T>> for InMemorySessionExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &InMemorySessionEntry<T>,
        _now: std::time::Instant,
    ) -> Option<Duration> {
        Some(value.ttl)
    }
}

struct InMemorySessionStore<T> {
    entries: moka::sync::Cache<String, InMemorySessionEntry<T>>,
}

impl<T> InMemorySessionStore<T>
where
    T: Clone + Send + Sync + 'static,
{
    fn new(capacity: u64) -> Self {
        Self {
            entries: moka::sync::Cache::builder()
                .max_capacity(capacity)
                .expire_after(InMemorySessionExpiry)
                .build(),
        }
    }

    fn store(&self, session_id: &str, session: &T, ttl: Duration) {
        self.entries.insert(
            session_id.to_string(),
            InMemorySessionEntry {
                session: session.clone(),
                ttl,
            },
        );
    }

    fn get(&self, session_id: &str) -> Option<T> {
        self.entries.get(session_id).map(|entry| entry.session)
    }

    fn consume(&self, session_id: &str) -> Option<T> {
        self.entries.get(session_id)?;
        self.entries.remove(session_id).map(|entry| entry.session)
    }
}

pub struct InMemoryOpaqueLoginSessionStore {
    store: InMemorySessionStore<OpaqueLoginSession>,
}

pub struct InMemoryOpaqueRegistrationSessionStore {
    store: InMemorySessionStore<OpaqueRegistrationSession>,
}

pub struct InMemoryMfaSessionStore {
    store: InMemorySessionStore<MfaSession>,
}

pub struct InMemorySensitiveVerificationSessionStore {
    store: InMemorySessionStore<SensitiveVerificationSession>,
}

impl InMemoryOpaqueLoginSessionStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            store: InMemorySessionStore::new(OPAQUE_LOGIN_SESSION_CAPACITY),
        }
    }
}

impl InMemoryOpaqueRegistrationSessionStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            store: InMemorySessionStore::new(OPAQUE_REGISTRATION_SESSION_CAPACITY),
        }
    }
}

impl InMemoryMfaSessionStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            store: InMemorySessionStore::new(MFA_SESSION_CAPACITY),
        }
    }
}

impl InMemorySensitiveVerificationSessionStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            store: InMemorySessionStore::new(SENSITIVE_VERIFICATION_SESSION_CAPACITY),
        }
    }
}

impl Default for InMemoryOpaqueLoginSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for InMemoryOpaqueRegistrationSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for InMemoryMfaSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for InMemorySensitiveVerificationSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl OpaqueLoginSessionStore for InMemoryOpaqueLoginSessionStore {
    async fn store(
        &self,
        session_id: &str,
        session: &OpaqueLoginSession,
        ttl: Duration,
    ) -> Result<()> {
        self.store.store(session_id, session, ttl);
        Ok(())
    }

    async fn consume(&self, session_id: &str) -> Result<Option<OpaqueLoginSession>> {
        Ok(self.store.consume(session_id))
    }

    fn supports_cross_node_single_use(&self) -> bool {
        false
    }
}

#[async_trait::async_trait]
impl OpaqueRegistrationSessionStore for InMemoryOpaqueRegistrationSessionStore {
    async fn store(
        &self,
        session_id: &str,
        session: &OpaqueRegistrationSession,
        ttl: Duration,
    ) -> Result<()> {
        self.store.store(session_id, session, ttl);
        Ok(())
    }

    async fn consume(&self, session_id: &str) -> Result<Option<OpaqueRegistrationSession>> {
        Ok(self.store.consume(session_id))
    }

    fn supports_cross_node_single_use(&self) -> bool {
        false
    }
}

#[async_trait::async_trait]
impl MfaSessionStore for InMemoryMfaSessionStore {
    async fn store(&self, session_id: &str, session: &MfaSession, ttl: Duration) -> Result<()> {
        self.store.store(session_id, session, ttl);
        Ok(())
    }

    async fn get(&self, session_id: &str) -> Result<Option<MfaSession>> {
        Ok(self.store.get(session_id))
    }

    async fn consume(&self, session_id: &str) -> Result<Option<MfaSession>> {
        Ok(self.store.consume(session_id))
    }

    fn supports_cross_node_single_use(&self) -> bool {
        false
    }
}

#[async_trait::async_trait]
impl SensitiveVerificationSessionStore for InMemorySensitiveVerificationSessionStore {
    async fn store(
        &self,
        session_id: &str,
        session: &SensitiveVerificationSession,
        ttl: Duration,
    ) -> Result<()> {
        self.store.store(session_id, session, ttl);
        Ok(())
    }

    async fn get(&self, session_id: &str) -> Result<Option<SensitiveVerificationSession>> {
        Ok(self.store.get(session_id))
    }

    async fn consume(&self, session_id: &str) -> Result<Option<SensitiveVerificationSession>> {
        Ok(self.store.consume(session_id))
    }

    fn supports_cross_node_single_use(&self) -> bool {
        false
    }
}
