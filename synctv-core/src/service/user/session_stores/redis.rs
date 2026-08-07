use std::{marker::PhantomData, sync::Arc, time::Duration};

use serde::{de::DeserializeOwned, Serialize};

use crate::{service::session_store::RedisJsonSessionStore, RedisConnectionRuntime, Result};

use super::{
    LoginSession, LoginSessionStore, MfaSession, MfaSessionStore, OpaqueRegistrationSession,
    OpaqueRegistrationSessionStore, SensitiveVerificationSession,
    SensitiveVerificationSessionStore,
};

const LOGIN_SESSION_REDIS_NAMESPACE: &str = "auth:login";
const OPAQUE_REGISTRATION_SESSION_REDIS_NAMESPACE: &str = "auth:opaque:registration";
const MFA_SESSION_REDIS_NAMESPACE: &str = "auth:mfa";
const SENSITIVE_VERIFICATION_SESSION_REDIS_NAMESPACE: &str = "auth:sensitive:verification";

#[derive(Clone, Copy)]
struct RedisSessionSpec {
    namespace: &'static str,
    serialize_context: &'static str,
    deserialize_context: &'static str,
    store_operation: &'static str,
    get_operation: &'static str,
    consume_operation: &'static str,
}

struct RedisTypedSessionStore<T> {
    store: RedisJsonSessionStore,
    spec: RedisSessionSpec,
    _session: PhantomData<T>,
}

impl<T> RedisTypedSessionStore<T> {
    fn from_runtime(
        runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: impl Into<String>,
        spec: RedisSessionSpec,
    ) -> Self {
        Self {
            store: RedisJsonSessionStore::new(runtime, key_prefix),
            spec,
            _session: PhantomData,
        }
    }
}

impl<T> RedisTypedSessionStore<T>
where
    T: Serialize + DeserializeOwned,
{
    async fn store(&self, session_id: &str, session: &T, ttl: Duration) -> Result<()> {
        self.store
            .store(
                self.spec.namespace,
                session_id,
                session,
                ttl,
                self.spec.serialize_context,
                self.spec.store_operation,
            )
            .await
    }

    async fn get(&self, session_id: &str) -> Result<Option<T>> {
        self.store
            .get(
                self.spec.namespace,
                session_id,
                self.spec.deserialize_context,
                self.spec.get_operation,
            )
            .await
    }

    async fn consume(&self, session_id: &str) -> Result<Option<T>> {
        self.store
            .consume(
                self.spec.namespace,
                session_id,
                self.spec.deserialize_context,
                self.spec.consume_operation,
            )
            .await
    }
}

pub(super) struct RedisLoginSessionStore {
    store: RedisTypedSessionStore<LoginSession>,
}

pub(super) struct RedisOpaqueRegistrationSessionStore {
    store: RedisTypedSessionStore<OpaqueRegistrationSession>,
}

pub(super) struct RedisMfaSessionStore {
    store: RedisTypedSessionStore<MfaSession>,
}

pub(super) struct RedisSensitiveVerificationSessionStore {
    store: RedisTypedSessionStore<SensitiveVerificationSession>,
}

impl RedisLoginSessionStore {
    #[must_use]
    pub(super) fn from_runtime(
        runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            store: RedisTypedSessionStore::from_runtime(
                runtime,
                key_prefix,
                RedisSessionSpec {
                    namespace: LOGIN_SESSION_REDIS_NAMESPACE,
                    serialize_context: "Failed to serialize login session",
                    deserialize_context: "Failed to deserialize login session",
                    store_operation: "store login session in Redis",
                    get_operation: "get login session from Redis",
                    consume_operation: "consume login session from Redis",
                },
            ),
        }
    }
}

impl RedisOpaqueRegistrationSessionStore {
    #[must_use]
    pub(super) fn from_runtime(
        runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            store: RedisTypedSessionStore::from_runtime(
                runtime,
                key_prefix,
                RedisSessionSpec {
                    namespace: OPAQUE_REGISTRATION_SESSION_REDIS_NAMESPACE,
                    serialize_context: "Failed to serialize OPAQUE registration session",
                    deserialize_context: "Failed to deserialize OPAQUE registration session",
                    store_operation: "store OPAQUE registration session in Redis",
                    get_operation: "get OPAQUE registration session from Redis",
                    consume_operation: "consume OPAQUE registration session from Redis",
                },
            ),
        }
    }
}

impl RedisMfaSessionStore {
    #[must_use]
    pub(super) fn from_runtime(
        runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            store: RedisTypedSessionStore::from_runtime(
                runtime,
                key_prefix,
                RedisSessionSpec {
                    namespace: MFA_SESSION_REDIS_NAMESPACE,
                    serialize_context: "Failed to serialize MFA session",
                    deserialize_context: "Failed to deserialize MFA session",
                    store_operation: "store MFA session in Redis",
                    get_operation: "get MFA session from Redis",
                    consume_operation: "consume MFA session from Redis",
                },
            ),
        }
    }
}

impl RedisSensitiveVerificationSessionStore {
    #[must_use]
    pub(super) fn from_runtime(
        runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            store: RedisTypedSessionStore::from_runtime(
                runtime,
                key_prefix,
                RedisSessionSpec {
                    namespace: SENSITIVE_VERIFICATION_SESSION_REDIS_NAMESPACE,
                    serialize_context: "Failed to serialize sensitive verification session",
                    deserialize_context: "Failed to deserialize sensitive verification session",
                    store_operation: "store sensitive verification session in Redis",
                    get_operation: "get sensitive verification session from Redis",
                    consume_operation: "consume sensitive verification session from Redis",
                },
            ),
        }
    }
}

#[async_trait::async_trait]
impl LoginSessionStore for RedisLoginSessionStore {
    async fn store(&self, session_id: &str, session: &LoginSession, ttl: Duration) -> Result<()> {
        self.store.store(session_id, session, ttl).await
    }

    async fn get(&self, session_id: &str) -> Result<Option<LoginSession>> {
        self.store.get(session_id).await
    }

    async fn consume(&self, session_id: &str) -> Result<Option<LoginSession>> {
        self.store.consume(session_id).await
    }

    fn supports_cross_node_single_use(&self) -> bool {
        true
    }
}

#[async_trait::async_trait]
impl OpaqueRegistrationSessionStore for RedisOpaqueRegistrationSessionStore {
    async fn store(
        &self,
        session_id: &str,
        session: &OpaqueRegistrationSession,
        ttl: Duration,
    ) -> Result<()> {
        self.store.store(session_id, session, ttl).await
    }

    async fn consume(&self, session_id: &str) -> Result<Option<OpaqueRegistrationSession>> {
        self.store.consume(session_id).await
    }

    fn supports_cross_node_single_use(&self) -> bool {
        true
    }
}

#[async_trait::async_trait]
impl MfaSessionStore for RedisMfaSessionStore {
    async fn store(&self, session_id: &str, session: &MfaSession, ttl: Duration) -> Result<()> {
        self.store.store(session_id, session, ttl).await
    }

    async fn get(&self, session_id: &str) -> Result<Option<MfaSession>> {
        self.store.get(session_id).await
    }

    async fn consume(&self, session_id: &str) -> Result<Option<MfaSession>> {
        self.store.consume(session_id).await
    }

    fn supports_cross_node_single_use(&self) -> bool {
        true
    }
}

#[async_trait::async_trait]
impl SensitiveVerificationSessionStore for RedisSensitiveVerificationSessionStore {
    async fn store(
        &self,
        session_id: &str,
        session: &SensitiveVerificationSession,
        ttl: Duration,
    ) -> Result<()> {
        self.store.store(session_id, session, ttl).await
    }

    async fn get(&self, session_id: &str) -> Result<Option<SensitiveVerificationSession>> {
        self.store.get(session_id).await
    }

    async fn consume(&self, session_id: &str) -> Result<Option<SensitiveVerificationSession>> {
        self.store.consume(session_id).await
    }

    fn supports_cross_node_single_use(&self) -> bool {
        true
    }
}
