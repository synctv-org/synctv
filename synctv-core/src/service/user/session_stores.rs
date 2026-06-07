use std::sync::Arc;
use std::time::Duration;

use crate::{RedisConnectionRuntime, Result, SharedStateMode, SharedStateProfile};

use super::session_types::{
    MfaSession, OpaqueLoginSession, OpaqueRegistrationSession, SensitiveVerificationSession,
};

mod local;
mod redis;

pub use local::{
    InMemoryMfaSessionStore, InMemoryOpaqueLoginSessionStore,
    InMemoryOpaqueRegistrationSessionStore, InMemorySensitiveVerificationSessionStore,
};
pub use redis::{
    RedisMfaSessionStore, RedisOpaqueLoginSessionStore, RedisOpaqueRegistrationSessionStore,
    RedisSensitiveVerificationSessionStore,
};

#[async_trait::async_trait]
pub trait OpaqueLoginSessionStore: Send + Sync {
    async fn store(
        &self,
        session_id: &str,
        session: &OpaqueLoginSession,
        ttl: Duration,
    ) -> Result<()>;

    async fn consume(&self, session_id: &str) -> Result<Option<OpaqueLoginSession>>;

    fn supports_cross_node_single_use(&self) -> bool;
}

#[async_trait::async_trait]
pub trait OpaqueRegistrationSessionStore: Send + Sync {
    async fn store(
        &self,
        session_id: &str,
        session: &OpaqueRegistrationSession,
        ttl: Duration,
    ) -> Result<()>;

    async fn consume(&self, session_id: &str) -> Result<Option<OpaqueRegistrationSession>>;

    fn supports_cross_node_single_use(&self) -> bool;
}

#[async_trait::async_trait]
pub trait MfaSessionStore: Send + Sync {
    async fn store(&self, session_id: &str, session: &MfaSession, ttl: Duration) -> Result<()>;

    async fn get(&self, session_id: &str) -> Result<Option<MfaSession>>;

    async fn consume(&self, session_id: &str) -> Result<Option<MfaSession>>;

    fn supports_cross_node_single_use(&self) -> bool;
}

#[async_trait::async_trait]
pub trait SensitiveVerificationSessionStore: Send + Sync {
    async fn store(
        &self,
        session_id: &str,
        session: &SensitiveVerificationSession,
        ttl: Duration,
    ) -> Result<()>;

    async fn get(&self, session_id: &str) -> Result<Option<SensitiveVerificationSession>>;

    async fn consume(&self, session_id: &str) -> Result<Option<SensitiveVerificationSession>>;

    fn supports_cross_node_single_use(&self) -> bool;
}

#[must_use]
pub fn local_opaque_login_session_store() -> Arc<dyn OpaqueLoginSessionStore> {
    Arc::new(InMemoryOpaqueLoginSessionStore::new())
}

#[must_use]
pub fn local_opaque_registration_session_store() -> Arc<dyn OpaqueRegistrationSessionStore> {
    Arc::new(InMemoryOpaqueRegistrationSessionStore::new())
}

#[must_use]
pub fn local_mfa_session_store() -> Arc<dyn MfaSessionStore> {
    Arc::new(InMemoryMfaSessionStore::new())
}

#[must_use]
pub fn local_sensitive_verification_session_store() -> Arc<dyn SensitiveVerificationSessionStore> {
    Arc::new(InMemorySensitiveVerificationSessionStore::new())
}

#[must_use]
pub fn shared_opaque_login_session_store(
    runtime: Arc<dyn RedisConnectionRuntime>,
    key_prefix: impl Into<String>,
) -> Arc<dyn OpaqueLoginSessionStore> {
    Arc::new(RedisOpaqueLoginSessionStore::from_runtime(
        runtime, key_prefix,
    ))
}

#[must_use]
pub fn shared_opaque_registration_session_store(
    runtime: Arc<dyn RedisConnectionRuntime>,
    key_prefix: impl Into<String>,
) -> Arc<dyn OpaqueRegistrationSessionStore> {
    Arc::new(RedisOpaqueRegistrationSessionStore::from_runtime(
        runtime, key_prefix,
    ))
}

#[must_use]
pub fn shared_mfa_session_store(
    runtime: Arc<dyn RedisConnectionRuntime>,
    key_prefix: impl Into<String>,
) -> Arc<dyn MfaSessionStore> {
    Arc::new(RedisMfaSessionStore::from_runtime(runtime, key_prefix))
}

#[must_use]
pub fn shared_sensitive_verification_session_store(
    runtime: Arc<dyn RedisConnectionRuntime>,
    key_prefix: impl Into<String>,
) -> Arc<dyn SensitiveVerificationSessionStore> {
    Arc::new(RedisSensitiveVerificationSessionStore::from_runtime(
        runtime, key_prefix,
    ))
}

pub fn opaque_login_session_store_from_shared_state_profile(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn OpaqueLoginSessionStore>> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            let runtime =
                profile.require_shared_runtime("single-use OPAQUE login session storage")?;
            Ok(shared_opaque_login_session_store(
                runtime,
                profile.key_prefix().to_string(),
            ))
        }
        SharedStateMode::SharedBestEffort => Ok(shared_opaque_login_session_store(
            profile.best_effort_shared_runtime("single-use OPAQUE login session storage")?,
            profile.key_prefix().to_string(),
        )),
        SharedStateMode::LocalOnly => Ok(local_opaque_login_session_store()),
    }
}

pub fn opaque_registration_session_store_from_shared_state_profile(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn OpaqueRegistrationSessionStore>> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            let runtime =
                profile.require_shared_runtime("single-use OPAQUE registration session storage")?;
            Ok(shared_opaque_registration_session_store(
                runtime,
                profile.key_prefix().to_string(),
            ))
        }
        SharedStateMode::SharedBestEffort => Ok(shared_opaque_registration_session_store(
            profile.best_effort_shared_runtime("single-use OPAQUE registration session storage")?,
            profile.key_prefix().to_string(),
        )),
        SharedStateMode::LocalOnly => Ok(local_opaque_registration_session_store()),
    }
}

pub fn mfa_session_store_from_shared_state_profile(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn MfaSessionStore>> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            let runtime = profile.require_shared_runtime("single-use MFA session storage")?;
            Ok(shared_mfa_session_store(
                runtime,
                profile.key_prefix().to_string(),
            ))
        }
        SharedStateMode::SharedBestEffort => Ok(shared_mfa_session_store(
            profile.best_effort_shared_runtime("single-use MFA session storage")?,
            profile.key_prefix().to_string(),
        )),
        SharedStateMode::LocalOnly => Ok(local_mfa_session_store()),
    }
}

pub fn sensitive_verification_session_store_from_shared_state_profile(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn SensitiveVerificationSessionStore>> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            let runtime = profile
                .require_shared_runtime("single-use sensitive verification session storage")?;
            Ok(shared_sensitive_verification_session_store(
                runtime,
                profile.key_prefix().to_string(),
            ))
        }
        SharedStateMode::SharedBestEffort => Ok(shared_sensitive_verification_session_store(
            profile
                .best_effort_shared_runtime("single-use sensitive verification session storage")?,
            profile.key_prefix().to_string(),
        )),
        SharedStateMode::LocalOnly => Ok(local_sensitive_verification_session_store()),
    }
}
