//! Typed provider credential/access cache.
//!
//! This layer intentionally exposes provider-specific access objects instead of a
//! generic credential enum. Callers ask for the exact shape they need while this
//! module centralizes DB caching, credential revisioning, and dynamic session
//! caching such as AList tokens.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::credential_encryption::CredentialEncryption;
use crate::error::Result as CoreResult;
use crate::models::{ProviderCredential, UserId, UserProviderCredential};
use crate::repository::UserProviderCredentialRepository;

use super::credential_resolver::{credential_revision, ResolvedProviderCredential};
use super::store::{ProviderStore, ProviderStoreExt};
use super::{AlistProvider, BilibiliProvider, EmbyProvider, ExecutionControl, ProviderError};

const BINDING_CACHE_TTL: Duration = Duration::from_mins(10);
const MISSING_CACHE_TTL: Duration = Duration::from_secs(15);
const BINDING_LOCK_TTL: Duration = Duration::from_secs(5);
const ALIST_SESSION_CACHE_TTL: Duration = Duration::from_mins(15);
const ALIST_SESSION_LOCK_TTL: Duration = Duration::from_secs(20);

#[derive(Debug, Clone)]
pub struct AlistBinding {
    pub host: String,
    pub server_id: String,
    pub credential_owner_id: String,
    pub credential_revision: String,
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AlistAccess {
    pub host: String,
    pub token: String,
    pub server_id: String,
    pub credential_owner_id: String,
    pub credential_revision: String,
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Clone)]
pub enum BilibiliAccess {
    Anonymous {
        credential_cache_partition: String,
        provider_instance_name: Option<String>,
    },
    Authenticated {
        cookies: HashMap<String, String>,
        credential_cache_partition: String,
        provider_instance_name: Option<String>,
    },
}

impl BilibiliAccess {
    #[must_use]
    pub fn anonymous(
        credential_cache_partition: impl Into<String>,
        provider_instance_name: Option<String>,
    ) -> Self {
        Self::Anonymous {
            credential_cache_partition: credential_cache_partition.into(),
            provider_instance_name,
        }
    }

    #[must_use]
    pub fn authenticated(
        cookies: HashMap<String, String>,
        credential_cache_partition: impl Into<String>,
        provider_instance_name: Option<String>,
    ) -> Self {
        Self::Authenticated {
            cookies,
            credential_cache_partition: credential_cache_partition.into(),
            provider_instance_name,
        }
    }

    #[must_use]
    pub const fn is_authenticated(&self) -> bool {
        matches!(self, Self::Authenticated { .. })
    }

    #[must_use]
    pub fn into_cookies(self) -> HashMap<String, String> {
        match self {
            Self::Anonymous { .. } => HashMap::new(),
            Self::Authenticated { cookies, .. } => cookies,
        }
    }

    #[must_use]
    pub fn into_cookies_and_partition(self) -> (HashMap<String, String>, String) {
        match self {
            Self::Anonymous {
                credential_cache_partition,
                ..
            } => (HashMap::new(), credential_cache_partition),
            Self::Authenticated {
                cookies,
                credential_cache_partition,
                ..
            } => (cookies, credential_cache_partition),
        }
    }

    #[must_use]
    pub fn into_authenticated(self) -> Option<(HashMap<String, String>, Option<String>)> {
        match self {
            Self::Anonymous { .. } => None,
            Self::Authenticated {
                cookies,
                provider_instance_name,
                ..
            } => Some((cookies, provider_instance_name)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EmbyAccess {
    pub host: String,
    pub api_key: String,
    pub emby_user_id: String,
    pub server_id: String,
    pub credential_owner_id: String,
    pub credential_revision: String,
    pub provider_instance_name: Option<String>,
}

#[async_trait]
pub trait ProviderAccessService: Send + Sync {
    async fn alist_binding(
        &self,
        user_id: UserId,
        server_id: &str,
        provider_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<AlistBinding, ProviderError>;

    async fn alist_access(
        &self,
        user_id: UserId,
        server_id: &str,
        provider_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<AlistAccess, ProviderError>;

    async fn bilibili_access(
        &self,
        user_id: UserId,
        request_context: Option<&ExecutionControl>,
    ) -> Result<BilibiliAccess, ProviderError>;

    async fn emby_access(
        &self,
        user_id: UserId,
        server_id: &str,
        provider_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<EmbyAccess, ProviderError>;

    async fn invalidate(
        &self,
        user_id: UserId,
        provider: &str,
        server_id: &str,
    ) -> Result<(), ProviderError>;

    async fn invalidate_alist_access(
        &self,
        user_id: UserId,
        server_id: &str,
        credential_revision: &str,
        provider_instance_name: Option<&str>,
    ) -> Result<(), ProviderError> {
        let _ = (credential_revision, provider_instance_name);
        self.invalidate(user_id, AlistProvider::NAME, server_id)
            .await
    }
}

#[async_trait]
pub trait ProviderCredentialReader: Send + Sync {
    async fn get_by_provider_and_server(
        &self,
        user_id: UserId,
        provider: &str,
        server_id: &str,
    ) -> CoreResult<Option<UserProviderCredential>>;
}

#[async_trait]
impl ProviderCredentialReader for UserProviderCredentialRepository {
    async fn get_by_provider_and_server(
        &self,
        user_id: UserId,
        provider: &str,
        server_id: &str,
    ) -> CoreResult<Option<UserProviderCredential>> {
        UserProviderCredentialRepository::get_by_provider_and_server(
            self, user_id, provider, server_id,
        )
        .await
    }
}

#[derive(Clone)]
pub struct CachedProviderAccessService {
    credential_reader: Arc<dyn ProviderCredentialReader>,
    store: Option<Arc<dyn ProviderStore>>,
    credential_encryption: Option<CredentialEncryption>,
    alist_provider: Arc<AlistProvider>,
}

impl CachedProviderAccessService {
    #[must_use]
    pub fn new(
        credential_reader: Arc<dyn ProviderCredentialReader>,
        alist_provider: Arc<AlistProvider>,
    ) -> Self {
        Self {
            credential_reader,
            store: None,
            credential_encryption: None,
            alist_provider,
        }
    }

    #[must_use]
    pub fn with_store(mut self, store: Arc<dyn ProviderStore>) -> Self {
        self.store = Some(store);
        self
    }

    #[must_use]
    pub fn with_credential_encryption(mut self, encryption: Option<CredentialEncryption>) -> Self {
        self.credential_encryption = encryption;
        self
    }

    fn binding_key(provider: &str, user_id: UserId, server_id: &str) -> String {
        format!("binding:{provider}:{user_id}:{server_id}")
    }

    fn missing_key(provider: &str, user_id: UserId, server_id: &str) -> String {
        format!("missing:{provider}:{user_id}:{server_id}")
    }

    fn binding_lock_key(provider: &str, user_id: UserId, server_id: &str) -> String {
        format!("lock:binding:{provider}:{user_id}:{server_id}")
    }

    fn alist_session_key(
        user_id: UserId,
        server_id: &str,
        revision: &str,
        provider_instance_name: Option<&str>,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(provider_instance_name.unwrap_or("").as_bytes());
        let instance_hash: String = hex::encode(hasher.finalize()).chars().take(12).collect();
        format!("session:alist:{user_id}:{server_id}:{revision}:{instance_hash}")
    }

    fn alist_session_lock_key(
        user_id: UserId,
        server_id: &str,
        revision: &str,
        provider_instance_name: Option<&str>,
    ) -> String {
        format!(
            "lock:{}",
            Self::alist_session_key(user_id, server_id, revision, provider_instance_name)
        )
    }

    async fn delete_cache_entry_best_effort(
        store: &Arc<dyn ProviderStore>,
        key: &str,
        reason: &'static str,
    ) {
        if let Err(error) = store.delete(key).await {
            tracing::warn!(
                cache_key = key,
                reason,
                error = %error,
                "Failed to delete provider access cache entry"
            );
        }
    }

    async fn set_cache_entry_best_effort<T: Serialize + Send + Sync>(
        store: &Arc<dyn ProviderStore>,
        key: &str,
        value: &T,
        ttl: Duration,
        reason: &'static str,
    ) {
        if let Err(error) = store.set(key, value, ttl).await {
            tracing::warn!(
                cache_key = key,
                reason,
                error = %error,
                "Failed to write provider access cache entry"
            );
        }
    }

    fn check_active(request_context: Option<&ExecutionControl>) -> Result<(), ProviderError> {
        if let Some(request_context) = request_context {
            request_context
                .check_active()
                .map_err(|err| ProviderError::NetworkError(err.to_string()))?;
        }
        Ok(())
    }

    fn credential_ttl(credential: &UserProviderCredential) -> Duration {
        let Some(expires_at) = credential.expires_at else {
            return BINDING_CACHE_TTL;
        };
        let remaining = (expires_at - crate::SystemClock.now()).num_seconds();
        if remaining <= 0 {
            Duration::from_secs(1)
        } else {
            BINDING_CACHE_TTL.min(Duration::from_secs(remaining.cast_unsigned()))
        }
    }

    fn encode_sensitive<T: Serialize>(
        &self,
        value: &T,
        cache_key: &str,
    ) -> Result<SensitiveCacheEnvelope, ProviderError> {
        let value = serde_json::to_value(value).map_err(|error| {
            ProviderError::Internal(format!(
                "Failed to serialize credential cache value: {error}"
            ))
        })?;

        let encryption = self.credential_encryption.as_ref().ok_or_else(|| {
            ProviderError::Internal(
                "Credential encryption must be configured before caching provider secrets"
                    .to_string(),
            )
        })?;

        Ok(SensitiveCacheEnvelope {
            encrypted: true,
            data: encryption
                .encrypt_to_value_with_context(&value, cache_key.as_bytes())
                .map_err(|error| {
                    ProviderError::Internal(format!(
                        "Failed to encrypt credential cache value: {error}"
                    ))
                })?,
        })
    }

    fn decode_sensitive<T: DeserializeOwned>(
        &self,
        envelope: &SensitiveCacheEnvelope,
        cache_key: &str,
    ) -> Result<T, ProviderError> {
        let value = if envelope.encrypted {
            let encryption = self.credential_encryption.as_ref().ok_or_else(|| {
                ProviderError::Internal(
                    "Credential cache entry is encrypted but credential encryption is unavailable"
                        .to_string(),
                )
            })?;
            encryption
                .decrypt_value_with_context(&envelope.data, cache_key.as_bytes())
                .map_err(|error| {
                    ProviderError::Internal(format!(
                        "Failed to decrypt credential cache value: {error}"
                    ))
                })?
        } else {
            return Err(ProviderError::Internal(
                "Credential cache entry is not encrypted".to_string(),
            ));
        };

        serde_json::from_value(value).map_err(|error| {
            ProviderError::Internal(format!(
                "Failed to deserialize credential cache value: {error}"
            ))
        })
    }

    async fn cached_record(
        &self,
        provider: &str,
        user_id: UserId,
        server_id: &str,
    ) -> Option<UserProviderCredential> {
        let store = self.store.as_ref()?;
        let key = Self::binding_key(provider, user_id, server_id);
        let envelope = match store.get::<SensitiveCacheEnvelope>(&key).await {
            Ok(Some(envelope)) => envelope,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!(
                    provider,
                    user_id = %user_id,
                    server_id,
                    error = %error,
                    "Failed to read provider credential cache entry"
                );
                return None;
            }
        };
        match self.decode_sensitive::<UserProviderCredential>(&envelope, &key) {
            Ok(record) if !record.is_expired() => Some(record),
            Ok(_) => {
                Self::delete_cache_entry_best_effort(store, &key, "expired_credential").await;
                None
            }
            Err(error) => {
                tracing::warn!(
                    provider,
                    user_id = %user_id,
                    server_id,
                    error = %error,
                    "Discarding unreadable provider credential cache entry"
                );
                Self::delete_cache_entry_best_effort(store, &key, "unreadable_credential").await;
                None
            }
        }
    }

    async fn fetch_record_from_db(
        &self,
        provider: &str,
        user_id: UserId,
        server_id: &str,
    ) -> Result<Option<UserProviderCredential>, ProviderError> {
        self.credential_reader
            .get_by_provider_and_server(user_id, provider, server_id)
            .await
            .map_err(|error| {
                ProviderError::Internal(format!(
                    "Failed to query {provider} credential from database: {error}"
                ))
            })
    }

    async fn load_record_optional(
        &self,
        provider: &str,
        user_id: UserId,
        server_id: &str,
        request_context: Option<&ExecutionControl>,
    ) -> Result<Option<UserProviderCredential>, ProviderError> {
        Self::check_active(request_context)?;

        if let Some(record) = self.cached_record(provider, user_id, server_id).await {
            return Ok(Some(record));
        }

        if let Some(store) = &self.store {
            let missing_key = Self::missing_key(provider, user_id, server_id);
            match store.get::<CachedMissing>(&missing_key).await {
                Ok(Some(_)) => return Ok(None),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        provider,
                        user_id = %user_id,
                        server_id,
                        error = %error,
                        "Failed to read provider credential missing-cache entry"
                    );
                }
            }

            let _lock = match store
                .lock(
                    &Self::binding_lock_key(provider, user_id, server_id),
                    BINDING_LOCK_TTL,
                )
                .await
            {
                Ok(lock) => Some(lock),
                Err(error) => {
                    tracing::debug!(
                        provider,
                        user_id = %user_id,
                        server_id,
                        error = %error,
                        "Failed to acquire provider credential cache lock"
                    );
                    None
                }
            };

            if let Some(record) = self.cached_record(provider, user_id, server_id).await {
                return Ok(Some(record));
            }

            let record = self
                .fetch_record_from_db(provider, user_id, server_id)
                .await?;
            match &record {
                Some(record) if !record.is_expired() => {
                    let key = Self::binding_key(provider, user_id, server_id);
                    let envelope = self.encode_sensitive(record, &key)?;
                    if let Err(error) = store
                        .set(&key, &envelope, Self::credential_ttl(record))
                        .await
                    {
                        tracing::warn!(
                            provider,
                            user_id = %user_id,
                            server_id,
                            error = %error,
                            "Failed to cache provider credential binding"
                        );
                    }
                }
                None => {
                    if let Err(error) = store
                        .set(&missing_key, &CachedMissing {}, MISSING_CACHE_TTL)
                        .await
                    {
                        tracing::warn!(
                            provider,
                            user_id = %user_id,
                            server_id,
                            error = %error,
                            "Failed to cache missing provider credential binding"
                        );
                    }
                }
                Some(_) => {}
            }
            return Ok(record);
        }

        self.fetch_record_from_db(provider, user_id, server_id)
            .await
    }

    async fn load_record(
        &self,
        provider: &str,
        user_id: UserId,
        server_id: &str,
        request_context: Option<&ExecutionControl>,
    ) -> Result<UserProviderCredential, ProviderError> {
        match self
            .load_record_optional(provider, user_id, server_id, request_context)
            .await?
        {
            Some(record) => Ok(record),
            None => Err(ProviderError::CredentialNotFound(format!(
                "No {provider} credential found for user '{user_id}' with server_id '{server_id}'"
            ))),
        }
    }

    fn resolved_credential(
        record: &UserProviderCredential,
    ) -> Result<ResolvedProviderCredential, ProviderError> {
        if record.is_expired() {
            return Err(ProviderError::CredentialExpired(format!(
                "{} credential for user '{}' has expired",
                record.provider, record.user_id
            )));
        }

        let credential = record.credential_data.clone();

        Ok(ResolvedProviderCredential {
            credential,
            revision: credential_revision(record.id, record.updated_at),
        })
    }

    async fn alist_record(
        &self,
        user_id: UserId,
        server_id: &str,
        request_context: Option<&ExecutionControl>,
    ) -> Result<(UserProviderCredential, ProviderCredential, String), ProviderError> {
        let record = self
            .load_record(AlistProvider::NAME, user_id, server_id, request_context)
            .await?;
        let resolved = Self::resolved_credential(&record)?;
        Ok((record, resolved.credential, resolved.revision))
    }

    async fn cached_alist_session(&self, key: &str) -> Option<CachedAlistSession> {
        let store = self.store.as_ref()?;
        let envelope = match store.get::<SensitiveCacheEnvelope>(key).await {
            Ok(Some(envelope)) => envelope,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!(
                    cache_key = key,
                    error = %error,
                    "Failed to read Alist session cache entry"
                );
                return None;
            }
        };
        match self.decode_sensitive::<CachedAlistSession>(&envelope, key) {
            Ok(session) => Some(session),
            Err(error) => {
                tracing::warn!(error = %error, "Discarding unreadable Alist session cache entry");
                Self::delete_cache_entry_best_effort(store, key, "unreadable_alist_session").await;
                None
            }
        }
    }

    async fn cache_alist_session(
        &self,
        key: &str,
        session: &CachedAlistSession,
    ) -> Result<(), ProviderError> {
        if let Some(store) = &self.store {
            let envelope = self.encode_sensitive(session, key)?;
            Self::set_cache_entry_best_effort(
                store,
                key,
                &envelope,
                ALIST_SESSION_CACHE_TTL,
                "alist_session",
            )
            .await;
        }
        Ok(())
    }
}

#[async_trait]
impl ProviderAccessService for CachedProviderAccessService {
    async fn alist_binding(
        &self,
        user_id: UserId,
        server_id: &str,
        provider_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<AlistBinding, ProviderError> {
        let (record, credential, revision) = self
            .alist_record(user_id, server_id, request_context)
            .await?;
        let provider_instance_name = provider_instance_name
            .map(std::string::ToString::to_string)
            .or(record.provider_instance_name);
        AlistProvider::binding_from_stored_credential(
            user_id,
            server_id,
            credential,
            revision,
            provider_instance_name,
            None,
        )
    }

    async fn alist_access(
        &self,
        user_id: UserId,
        server_id: &str,
        provider_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<AlistAccess, ProviderError> {
        let (record, credential, revision) = self
            .alist_record(user_id, server_id, request_context)
            .await?;
        let provider_instance_name = provider_instance_name
            .map(std::string::ToString::to_string)
            .or(record.provider_instance_name);

        let session_key = Self::alist_session_key(
            user_id,
            server_id,
            &revision,
            provider_instance_name.as_deref(),
        );
        if let Some(session) = self.cached_alist_session(&session_key).await {
            return Ok(AlistProvider::access_from_session(
                user_id,
                server_id,
                revision,
                provider_instance_name,
                session.host,
                session.token,
            ));
        }

        let _lock = if let Some(store) = &self.store {
            match store
                .lock(
                    &Self::alist_session_lock_key(
                        user_id,
                        server_id,
                        &revision,
                        provider_instance_name.as_deref(),
                    ),
                    ALIST_SESSION_LOCK_TTL,
                )
                .await
            {
                Ok(lock) => Some(lock),
                Err(error) => {
                    tracing::debug!(
                        user_id = %user_id,
                        server_id,
                        provider_instance_name = provider_instance_name.as_deref().unwrap_or(""),
                        error = %error,
                        "Failed to acquire Alist session cache lock"
                    );
                    None
                }
            }
        } else {
            None
        };

        if let Some(session) = self.cached_alist_session(&session_key).await {
            return Ok(AlistProvider::access_from_session(
                user_id,
                server_id,
                revision,
                provider_instance_name,
                session.host,
                session.token,
            ));
        }

        let access = self
            .alist_provider
            .login_access_from_stored_credential(
                user_id,
                server_id,
                credential,
                revision,
                provider_instance_name,
                request_context,
            )
            .await?;

        let session = CachedAlistSession {
            host: access.host.clone(),
            token: access.token.clone(),
        };
        self.cache_alist_session(&session_key, &session).await?;

        Ok(access)
    }

    async fn bilibili_access(
        &self,
        user_id: UserId,
        request_context: Option<&ExecutionControl>,
    ) -> Result<BilibiliAccess, ProviderError> {
        let server_id = BilibiliProvider::credential_server_id();
        let Some(record) = self
            .load_record_optional(BilibiliProvider::NAME, user_id, &server_id, request_context)
            .await?
        else {
            return Ok(BilibiliProvider::anonymous_access());
        };
        if record.is_expired() {
            return Ok(BilibiliProvider::anonymous_access());
        }

        let resolved = Self::resolved_credential(&record)?;
        BilibiliProvider::access_from_stored_credential(
            user_id,
            &server_id,
            resolved.credential,
            &resolved.revision,
            record.provider_instance_name,
        )
    }

    async fn emby_access(
        &self,
        user_id: UserId,
        server_id: &str,
        provider_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<EmbyAccess, ProviderError> {
        let record = self
            .load_record(EmbyProvider::NAME, user_id, server_id, request_context)
            .await?;
        let provider_instance_name = provider_instance_name
            .map(std::string::ToString::to_string)
            .or_else(|| record.provider_instance_name.clone());
        let resolved = Self::resolved_credential(&record)?;
        EmbyProvider::access_from_stored_credential(
            user_id,
            server_id,
            resolved.credential,
            resolved.revision,
            provider_instance_name,
            None,
        )
    }

    async fn invalidate(
        &self,
        user_id: UserId,
        provider: &str,
        server_id: &str,
    ) -> Result<(), ProviderError> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let alist_session_key = if provider == AlistProvider::NAME {
            self.alist_record(user_id, server_id, None)
                .await
                .ok()
                .map(|(record, _, revision)| {
                    Self::alist_session_key(
                        user_id,
                        server_id,
                        &revision,
                        record.provider_instance_name.as_deref(),
                    )
                })
        } else {
            None
        };
        let binding_key = Self::binding_key(provider, user_id, server_id);
        let missing_key = Self::missing_key(provider, user_id, server_id);

        if let Err(error) = store.delete(&binding_key).await {
            tracing::warn!(
                provider,
                user_id = %user_id,
                server_id,
                key = %binding_key,
                error = %error,
                "Failed to invalidate provider access cache entry"
            );
        }
        if let Err(error) = store.delete(&missing_key).await {
            tracing::warn!(
                provider,
                user_id = %user_id,
                server_id,
                key = %missing_key,
                error = %error,
                "Failed to invalidate provider access cache entry"
            );
        }
        if let Some(session_key) = alist_session_key {
            if let Err(error) = store.delete(&session_key).await {
                tracing::warn!(
                    provider,
                    user_id = %user_id,
                    server_id,
                    key = %session_key,
                    error = %error,
                    "Failed to invalidate provider access session"
                );
            }
        }

        Ok(())
    }

    async fn invalidate_alist_access(
        &self,
        user_id: UserId,
        server_id: &str,
        credential_revision: &str,
        provider_instance_name: Option<&str>,
    ) -> Result<(), ProviderError> {
        self.invalidate(user_id, AlistProvider::NAME, server_id)
            .await?;
        let Some(store) = &self.store else {
            return Ok(());
        };
        let session_key = Self::alist_session_key(
            user_id,
            server_id,
            credential_revision,
            provider_instance_name,
        );
        Self::delete_cache_entry_best_effort(store, &session_key, "alist_auth_rejected").await;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SensitiveCacheEnvelope {
    encrypted: bool,
    data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedMissing {}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedAlistSession {
    host: String,
    token: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::InMemoryProviderStore;
    use crate::test_helpers::TestResultExt;
    use std::sync::Mutex;

    #[derive(Default)]
    struct InMemoryCredentialReader {
        records: Mutex<Vec<UserProviderCredential>>,
    }

    impl InMemoryCredentialReader {
        fn empty() -> Arc<Self> {
            Arc::new(Self::default())
        }

        fn with_records(records: Vec<UserProviderCredential>) -> Arc<Self> {
            Arc::new(Self {
                records: Mutex::new(records),
            })
        }
    }

    #[async_trait]
    impl ProviderCredentialReader for InMemoryCredentialReader {
        async fn get_by_provider_and_server(
            &self,
            user_id: UserId,
            provider: &str,
            server_id: &str,
        ) -> CoreResult<Option<UserProviderCredential>> {
            Ok(self
                .records
                .lock()
                .checked("credential reader lock should be available")
                .iter()
                .find(|record| {
                    record.user_id == user_id
                        && record.provider == provider
                        && record.server_id == server_id
                })
                .cloned())
        }
    }

    fn test_encryption() -> CredentialEncryption {
        CredentialEncryption::new(&[0x42; 32]).checked("test encryption key should be valid")
    }

    fn test_service(store: Arc<dyn ProviderStore>) -> CachedProviderAccessService {
        test_service_with_reader(store, InMemoryCredentialReader::empty())
    }

    fn test_service_with_reader(
        store: Arc<dyn ProviderStore>,
        credential_reader: Arc<dyn ProviderCredentialReader>,
    ) -> CachedProviderAccessService {
        let alist_provider =
            Arc::new(AlistProvider::new_local_only().checked("provider should build"));

        CachedProviderAccessService::new(credential_reader, alist_provider)
            .with_store(store)
            .with_credential_encryption(Some(test_encryption()))
    }

    fn credential_record(
        provider: &str,
        user_id: UserId,
        server_id: &str,
        provider_instance_name: Option<&str>,
        credential: ProviderCredential,
    ) -> UserProviderCredential {
        let now = crate::SystemClock.now();
        UserProviderCredential {
            id: 42,
            user_id,
            provider: provider.to_string(),
            server_id: server_id.to_string(),
            provider_instance_name: provider_instance_name.map(std::string::ToString::to_string),
            credential_data: credential,
            expires_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    async fn cache_record(
        service: &CachedProviderAccessService,
        store: &dyn ProviderStore,
        record: &UserProviderCredential,
    ) {
        let key = CachedProviderAccessService::binding_key(
            &record.provider,
            record.user_id,
            &record.server_id,
        );
        let envelope = service
            .encode_sensitive(record, &key)
            .checked("credential cache entry encodes");
        store
            .set(&key, &envelope, Duration::from_mins(1))
            .await
            .checked("credential cache write succeeds");
    }

    #[tokio::test]
    async fn encode_sensitive_requires_credential_encryption() {
        let store = Arc::new(InMemoryProviderStore::new(16));
        let alist_provider =
            Arc::new(AlistProvider::new_local_only().checked("provider should build"));
        let service =
            CachedProviderAccessService::new(InMemoryCredentialReader::empty(), alist_provider)
                .with_store(store);

        let error = service
            .encode_sensitive(
                &CachedAlistSession {
                    host: "https://alist.example.test".to_string(),
                    token: "token".to_string(),
                },
                "test-cache-key",
            )
            .failed("sensitive cache writes require encryption");

        assert!(error
            .to_string()
            .contains("Credential encryption must be configured"));
    }

    #[tokio::test]
    async fn bilibili_access_loads_record_from_reader_and_caches_binding() {
        let store = Arc::new(InMemoryProviderStore::new(16));
        let user_id = UserId::expect_positive(7);
        let server_id = BilibiliProvider::credential_server_id();
        let record = credential_record(
            BilibiliProvider::NAME,
            user_id,
            &server_id,
            None,
            ProviderCredential::Bilibili {
                cookies: HashMap::from([("SESSDATA".to_string(), "cookie".to_string())]),
            },
        );
        let service = test_service_with_reader(
            store.clone(),
            InMemoryCredentialReader::with_records(vec![record]),
        );

        let access = service
            .bilibili_access(user_id, None)
            .await
            .checked("bilibili access resolves from credential reader");

        assert!(access.is_authenticated());
        let cookies = access.into_cookies();
        assert_eq!(cookies.get("SESSDATA").map(String::as_str), Some("cookie"));
        assert!(store
            .get_raw(&CachedProviderAccessService::binding_key(
                BilibiliProvider::NAME,
                user_id,
                &server_id,
            ))
            .await
            .checked("binding cache read succeeds")
            .is_some());
    }

    #[tokio::test]
    async fn alist_access_uses_bound_provider_instance_for_session_cache() {
        let store = Arc::new(InMemoryProviderStore::new(16));
        let service = test_service(store.clone());
        let user_id = UserId::expect_positive(7);
        let server_id = "alist-main";
        let record = credential_record(
            AlistProvider::NAME,
            user_id,
            server_id,
            Some("credential-instance"),
            ProviderCredential::Alist {
                host: "https://alist.example.test".to_string(),
                username: "alice".to_string(),
                password: "hashed-password".to_string(),
                otp_secret: None,
            },
        );
        cache_record(&service, store.as_ref(), &record).await;

        let revision = credential_revision(record.id, record.updated_at);
        let stale_key = CachedProviderAccessService::alist_session_key(
            user_id,
            server_id,
            &revision,
            Some("credential-instance"),
        );
        let stale_session = service
            .encode_sensitive(
                &CachedAlistSession {
                    host: "https://stale.example.test".to_string(),
                    token: "stale-token".to_string(),
                },
                &stale_key,
            )
            .checked("stale session encodes");
        store
            .set(&stale_key, &stale_session, Duration::from_mins(1))
            .await
            .checked("stale session cache write succeeds");
        let bound_key = CachedProviderAccessService::alist_session_key(
            user_id,
            server_id,
            &revision,
            Some("bound-instance"),
        );
        let bound_session = service
            .encode_sensitive(
                &CachedAlistSession {
                    host: "https://bound.example.test".to_string(),
                    token: "bound-token".to_string(),
                },
                &bound_key,
            )
            .checked("bound session encodes");
        store
            .set(&bound_key, &bound_session, Duration::from_mins(1))
            .await
            .checked("bound session cache write succeeds");

        let access = service
            .alist_access(user_id, server_id, Some("bound-instance"), None)
            .await
            .checked("alist access resolves");

        assert_eq!(access.host, "https://bound.example.test");
        assert_eq!(access.token, "bound-token");
        assert_eq!(
            access.provider_instance_name.as_deref(),
            Some("bound-instance")
        );
    }

    #[tokio::test]
    async fn emby_access_prefers_bound_provider_instance() {
        let store = Arc::new(InMemoryProviderStore::new(16));
        let service = test_service(store.clone());
        let user_id = UserId::expect_positive(7);
        let server_id = "emby-main";
        let record = credential_record(
            EmbyProvider::NAME,
            user_id,
            server_id,
            Some("credential-instance"),
            ProviderCredential::Emby {
                host: "https://emby.example.test".to_string(),
                api_key: "api-key".to_string(),
                emby_user_id: "emby-user".to_string(),
            },
        );
        cache_record(&service, store.as_ref(), &record).await;

        let access = service
            .emby_access(user_id, server_id, Some("bound-instance"), None)
            .await
            .checked("emby access resolves");

        assert_eq!(
            access.provider_instance_name.as_deref(),
            Some("bound-instance")
        );
    }

    #[tokio::test]
    async fn invalidate_removes_cached_binding_and_missing_entries_for_all_providers() {
        let store = Arc::new(InMemoryProviderStore::new(16));
        let service = test_service(store.clone());
        let user_id = UserId::expect_positive(7);

        for (provider, server_id, credential) in [
            (
                AlistProvider::NAME,
                "alist-main",
                ProviderCredential::Alist {
                    host: "https://alist.example.test".to_string(),
                    username: "alice".to_string(),
                    password: "hashed-password".to_string(),
                    otp_secret: None,
                },
            ),
            (
                BilibiliProvider::NAME,
                &BilibiliProvider::credential_server_id(),
                ProviderCredential::Bilibili {
                    cookies: HashMap::from([("SESSDATA".to_string(), "cookie".to_string())]),
                },
            ),
            (
                EmbyProvider::NAME,
                "emby-main",
                ProviderCredential::Emby {
                    host: "https://emby.example.test".to_string(),
                    api_key: "api-key".to_string(),
                    emby_user_id: "emby-user".to_string(),
                },
            ),
        ] {
            let record = credential_record(provider, user_id, server_id, None, credential);
            cache_record(&service, store.as_ref(), &record).await;
            store
                .set(
                    &CachedProviderAccessService::missing_key(provider, user_id, server_id),
                    &CachedMissing {},
                    Duration::from_mins(1),
                )
                .await
                .checked("missing cache write succeeds");

            service
                .invalidate(user_id, provider, server_id)
                .await
                .checked("provider access cache invalidates");

            assert!(
                store
                    .get_raw(&CachedProviderAccessService::binding_key(
                        provider, user_id, server_id
                    ))
                    .await
                    .checked("binding cache read succeeds")
                    .is_none(),
                "{provider} binding cache should be removed"
            );
            assert!(
                store
                    .get_raw(&CachedProviderAccessService::missing_key(
                        provider, user_id, server_id
                    ))
                    .await
                    .checked("missing cache read succeeds")
                    .is_none(),
                "{provider} missing cache should be removed"
            );
        }
    }
}
