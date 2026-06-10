use std::sync::Arc;
use std::time::Duration as StdDuration;

use serde::{Deserialize, Serialize};

use crate::{
    models::{RoomId, UserId},
    service::session_store::RedisJsonSessionStore,
    RedisConnectionRuntime, Result, SharedStateMode, SharedStateProfile,
};

pub const ROOM_OPAQUE_REGISTRATION_SESSION_TTL_SECS: u64 = 300;
pub const ROOM_OPAQUE_LOGIN_SESSION_TTL_SECS: u64 = 300;
const ROOM_OPAQUE_SESSION_CAPACITY: u64 = 10_000;
const ROOM_OPAQUE_REGISTRATION_SESSION_REDIS_NAMESPACE: &str = "room:opaque:password_registration";
const ROOM_OPAQUE_LOGIN_SESSION_REDIS_NAMESPACE: &str = "room:opaque:password_login";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomOpaquePasswordRegistrationSession {
    pub(super) room_id: RoomId,
    pub(super) user_id: UserId,
    pub(super) credential_identifier: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomOpaquePasswordLoginSession {
    pub(super) room_id: RoomId,
    pub(super) user_id: UserId,
    pub(super) expected_password_version: i32,
    pub(super) server_login_state: Vec<u8>,
    pub(super) brute_force_subject_key: String,
}

#[derive(Debug, Clone)]
pub struct RoomOpaqueRegistrationStartChallenge {
    pub session_id: String,
    pub registration_response: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct RoomOpaqueLoginStartChallenge {
    pub session_id: String,
    pub credential_response: Vec<u8>,
}

#[async_trait::async_trait]
pub trait RoomOpaquePasswordRegistrationSessionStore: Send + Sync {
    async fn store(
        &self,
        session_id: &str,
        session: &RoomOpaquePasswordRegistrationSession,
        ttl: StdDuration,
    ) -> Result<()>;

    async fn consume(
        &self,
        session_id: &str,
    ) -> Result<Option<RoomOpaquePasswordRegistrationSession>>;
}

#[async_trait::async_trait]
pub trait RoomOpaquePasswordLoginSessionStore: Send + Sync {
    async fn store(
        &self,
        session_id: &str,
        session: &RoomOpaquePasswordLoginSession,
        ttl: StdDuration,
    ) -> Result<()>;

    async fn consume(&self, session_id: &str) -> Result<Option<RoomOpaquePasswordLoginSession>>;
}

#[derive(Clone)]
struct RoomOpaqueRegistrationSessionEntry {
    session: RoomOpaquePasswordRegistrationSession,
    ttl: StdDuration,
}

#[derive(Clone)]
struct RoomOpaqueLoginSessionEntry {
    session: RoomOpaquePasswordLoginSession,
    ttl: StdDuration,
}

struct RoomOpaqueRegistrationSessionExpiry;

impl moka::Expiry<String, RoomOpaqueRegistrationSessionEntry>
    for RoomOpaqueRegistrationSessionExpiry
{
    fn expire_after_create(
        &self,
        _key: &String,
        value: &RoomOpaqueRegistrationSessionEntry,
        _now: std::time::Instant,
    ) -> Option<StdDuration> {
        Some(value.ttl)
    }
}

struct RoomOpaqueLoginSessionExpiry;

impl moka::Expiry<String, RoomOpaqueLoginSessionEntry> for RoomOpaqueLoginSessionExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &RoomOpaqueLoginSessionEntry,
        _now: std::time::Instant,
    ) -> Option<StdDuration> {
        Some(value.ttl)
    }
}

pub struct InMemoryRoomOpaquePasswordRegistrationSessionStore {
    entries: moka::sync::Cache<String, RoomOpaqueRegistrationSessionEntry>,
}

pub struct InMemoryRoomOpaquePasswordLoginSessionStore {
    entries: moka::sync::Cache<String, RoomOpaqueLoginSessionEntry>,
}

impl InMemoryRoomOpaquePasswordRegistrationSessionStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: moka::sync::Cache::builder()
                .max_capacity(ROOM_OPAQUE_SESSION_CAPACITY)
                .expire_after(RoomOpaqueRegistrationSessionExpiry)
                .build(),
        }
    }
}

impl Default for InMemoryRoomOpaquePasswordRegistrationSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryRoomOpaquePasswordLoginSessionStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: moka::sync::Cache::builder()
                .max_capacity(ROOM_OPAQUE_SESSION_CAPACITY)
                .expire_after(RoomOpaqueLoginSessionExpiry)
                .build(),
        }
    }
}

impl Default for InMemoryRoomOpaquePasswordLoginSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl RoomOpaquePasswordRegistrationSessionStore
    for InMemoryRoomOpaquePasswordRegistrationSessionStore
{
    async fn store(
        &self,
        session_id: &str,
        session: &RoomOpaquePasswordRegistrationSession,
        ttl: StdDuration,
    ) -> Result<()> {
        self.entries.insert(
            session_id.to_string(),
            RoomOpaqueRegistrationSessionEntry {
                session: session.clone(),
                ttl,
            },
        );
        Ok(())
    }

    async fn consume(
        &self,
        session_id: &str,
    ) -> Result<Option<RoomOpaquePasswordRegistrationSession>> {
        if self.entries.get(session_id).is_none() {
            return Ok(None);
        }
        Ok(self.entries.remove(session_id).map(|entry| entry.session))
    }
}

#[async_trait::async_trait]
impl RoomOpaquePasswordLoginSessionStore for InMemoryRoomOpaquePasswordLoginSessionStore {
    async fn store(
        &self,
        session_id: &str,
        session: &RoomOpaquePasswordLoginSession,
        ttl: StdDuration,
    ) -> Result<()> {
        self.entries.insert(
            session_id.to_string(),
            RoomOpaqueLoginSessionEntry {
                session: session.clone(),
                ttl,
            },
        );
        Ok(())
    }

    async fn consume(&self, session_id: &str) -> Result<Option<RoomOpaquePasswordLoginSession>> {
        if self.entries.get(session_id).is_none() {
            return Ok(None);
        }
        Ok(self.entries.remove(session_id).map(|entry| entry.session))
    }
}

struct RedisRoomOpaquePasswordRegistrationSessionStore {
    store: RedisJsonSessionStore,
}

struct RedisRoomOpaquePasswordLoginSessionStore {
    store: RedisJsonSessionStore,
}

impl RedisRoomOpaquePasswordRegistrationSessionStore {
    #[must_use]
    fn from_runtime(
        runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            store: RedisJsonSessionStore::new(runtime, key_prefix),
        }
    }
}

impl RedisRoomOpaquePasswordLoginSessionStore {
    #[must_use]
    fn from_runtime(
        runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            store: RedisJsonSessionStore::new(runtime, key_prefix),
        }
    }
}

#[async_trait::async_trait]
impl RoomOpaquePasswordRegistrationSessionStore
    for RedisRoomOpaquePasswordRegistrationSessionStore
{
    async fn store(
        &self,
        session_id: &str,
        session: &RoomOpaquePasswordRegistrationSession,
        ttl: StdDuration,
    ) -> Result<()> {
        self.store
            .store(
                ROOM_OPAQUE_REGISTRATION_SESSION_REDIS_NAMESPACE,
                session_id,
                session,
                ttl,
                "Failed to serialize room OPAQUE registration session",
                "store room OPAQUE registration session in Redis",
            )
            .await
    }

    async fn consume(
        &self,
        session_id: &str,
    ) -> Result<Option<RoomOpaquePasswordRegistrationSession>> {
        self.store
            .consume(
                ROOM_OPAQUE_REGISTRATION_SESSION_REDIS_NAMESPACE,
                session_id,
                "Failed to deserialize room OPAQUE registration session",
                "consume room OPAQUE registration session from Redis",
            )
            .await
    }
}

#[async_trait::async_trait]
impl RoomOpaquePasswordLoginSessionStore for RedisRoomOpaquePasswordLoginSessionStore {
    async fn store(
        &self,
        session_id: &str,
        session: &RoomOpaquePasswordLoginSession,
        ttl: StdDuration,
    ) -> Result<()> {
        self.store
            .store(
                ROOM_OPAQUE_LOGIN_SESSION_REDIS_NAMESPACE,
                session_id,
                session,
                ttl,
                "Failed to serialize room OPAQUE login session",
                "store room OPAQUE login session in Redis",
            )
            .await
    }

    async fn consume(&self, session_id: &str) -> Result<Option<RoomOpaquePasswordLoginSession>> {
        self.store
            .consume(
                ROOM_OPAQUE_LOGIN_SESSION_REDIS_NAMESPACE,
                session_id,
                "Failed to deserialize room OPAQUE login session",
                "consume room OPAQUE login session from Redis",
            )
            .await
    }
}

#[must_use]
pub(crate) fn local_room_opaque_password_registration_session_store(
) -> Arc<dyn RoomOpaquePasswordRegistrationSessionStore> {
    Arc::new(InMemoryRoomOpaquePasswordRegistrationSessionStore::new())
}

#[must_use]
pub(crate) fn local_room_opaque_password_login_session_store(
) -> Arc<dyn RoomOpaquePasswordLoginSessionStore> {
    Arc::new(InMemoryRoomOpaquePasswordLoginSessionStore::new())
}

fn shared_room_opaque_password_registration_session_store(
    runtime: Arc<dyn RedisConnectionRuntime>,
    key_prefix: impl Into<String>,
) -> Arc<dyn RoomOpaquePasswordRegistrationSessionStore> {
    Arc::new(RedisRoomOpaquePasswordRegistrationSessionStore::from_runtime(runtime, key_prefix))
}

fn shared_room_opaque_password_login_session_store(
    runtime: Arc<dyn RedisConnectionRuntime>,
    key_prefix: impl Into<String>,
) -> Arc<dyn RoomOpaquePasswordLoginSessionStore> {
    Arc::new(RedisRoomOpaquePasswordLoginSessionStore::from_runtime(
        runtime, key_prefix,
    ))
}

pub(crate) fn room_opaque_password_registration_session_store_from_shared_state_profile(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn RoomOpaquePasswordRegistrationSessionStore>> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            let runtime = profile.require_shared_runtime(
                "single-use room OPAQUE password registration session storage",
            )?;
            Ok(shared_room_opaque_password_registration_session_store(
                runtime,
                profile.key_prefix().to_string(),
            ))
        }
        SharedStateMode::SharedBestEffort => {
            Ok(shared_room_opaque_password_registration_session_store(
                profile.best_effort_shared_runtime(
                    "single-use room OPAQUE password registration session storage",
                )?,
                profile.key_prefix().to_string(),
            ))
        }
        SharedStateMode::LocalOnly => Ok(local_room_opaque_password_registration_session_store()),
    }
}

pub(crate) fn room_opaque_password_login_session_store_from_shared_state_profile(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn RoomOpaquePasswordLoginSessionStore>> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            let runtime = profile
                .require_shared_runtime("single-use room OPAQUE password login session storage")?;
            Ok(shared_room_opaque_password_login_session_store(
                runtime,
                profile.key_prefix().to_string(),
            ))
        }
        SharedStateMode::SharedBestEffort => Ok(shared_room_opaque_password_login_session_store(
            profile.best_effort_shared_runtime(
                "single-use room OPAQUE password login session storage",
            )?,
            profile.key_prefix().to_string(),
        )),
        SharedStateMode::LocalOnly => Ok(local_room_opaque_password_login_session_store()),
    }
}
