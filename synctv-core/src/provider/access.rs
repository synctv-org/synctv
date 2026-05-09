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
use chrono::Utc;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::models::{ProviderCredential, UserId, UserProviderCredential};
use crate::repository::UserProviderCredentialRepository;
use crate::service::CredentialEncryption;

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
pub struct BilibiliAccess {
    pub cookies: HashMap<String, String>,
    pub credential_cache_partition: String,
    pub authenticated: bool,
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
}

#[derive(Clone)]
pub struct CachedProviderAccessService {
    credential_repo: Arc<UserProviderCredentialRepository>,
    store: Option<Arc<dyn ProviderStore>>,
    credential_encryption: Option<CredentialEncryption>,
    alist_provider: Arc<AlistProvider>,
}

impl CachedProviderAccessService {
    #[must_use]
    pub fn new(
        credential_repo: Arc<UserProviderCredentialRepository>,
        alist_provider: Arc<AlistProvider>,
    ) -> Self {
        Self {
            credential_repo,
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
        let remaining = (expires_at - Utc::now()).num_seconds();
        if remaining <= 0 {
            Duration::from_secs(1)
        } else {
            BINDING_CACHE_TTL.min(Duration::from_secs(remaining.cast_unsigned()))
        }
    }

    fn encode_sensitive<T: Serialize>(
        &self,
        value: &T,
    ) -> Result<SensitiveCacheEnvelope, ProviderError> {
        let value = serde_json::to_value(value).map_err(|error| {
            ProviderError::Internal(format!(
                "Failed to serialize credential cache value: {error}"
            ))
        })?;

        if let Some(encryption) = &self.credential_encryption {
            Ok(SensitiveCacheEnvelope {
                encrypted: true,
                data: encryption.encrypt_to_value(&value).map_err(|error| {
                    ProviderError::Internal(format!(
                        "Failed to encrypt credential cache value: {error}"
                    ))
                })?,
            })
        } else {
            Ok(SensitiveCacheEnvelope {
                encrypted: false,
                data: value,
            })
        }
    }

    fn decode_sensitive<T: DeserializeOwned>(
        &self,
        envelope: SensitiveCacheEnvelope,
    ) -> Result<T, ProviderError> {
        let value = if envelope.encrypted {
            let encryption = self.credential_encryption.as_ref().ok_or_else(|| {
                ProviderError::Internal(
                    "Credential cache entry is encrypted but credential encryption is unavailable"
                        .to_string(),
                )
            })?;
            encryption.decrypt_value(&envelope.data).map_err(|error| {
                ProviderError::Internal(format!(
                    "Failed to decrypt credential cache value: {error}"
                ))
            })?
        } else {
            envelope.data
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
        let envelope = store
            .get::<SensitiveCacheEnvelope>(&key)
            .await
            .ok()
            .flatten()?;
        match self.decode_sensitive::<UserProviderCredential>(envelope) {
            Ok(record) if !record.is_expired() => Some(record),
            Ok(_) => {
                let _ = store.delete(&key).await;
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
                let _ = store.delete(&key).await;
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
        self.credential_repo
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
            if store
                .get::<CachedMissing>(&missing_key)
                .await
                .ok()
                .flatten()
                .is_some()
            {
                return Ok(None);
            }

            let _lock = store
                .lock(
                    &Self::binding_lock_key(provider, user_id, server_id),
                    BINDING_LOCK_TTL,
                )
                .await
                .ok();

            if let Some(record) = self.cached_record(provider, user_id, server_id).await {
                return Ok(Some(record));
            }

            let record = self
                .fetch_record_from_db(provider, user_id, server_id)
                .await?;
            match &record {
                Some(record) if !record.is_expired() => {
                    let envelope = self.encode_sensitive(record)?;
                    let _ = store
                        .set(
                            &Self::binding_key(provider, user_id, server_id),
                            &envelope,
                            Self::credential_ttl(record),
                        )
                        .await;
                }
                None => {
                    let _ = store
                        .set(&missing_key, &CachedMissing {}, MISSING_CACHE_TTL)
                        .await;
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

        let credential = record.get_credential().map_err(|error| {
            ProviderError::Internal(format!(
                "Failed to parse {} credential data: {error}",
                record.provider
            ))
        })?;

        Ok(ResolvedProviderCredential {
            credential,
            id: record.id.to_string(),
            updated_at: record.updated_at,
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
        let envelope = store
            .get::<SensitiveCacheEnvelope>(key)
            .await
            .ok()
            .flatten()?;
        match self.decode_sensitive::<CachedAlistSession>(envelope) {
            Ok(session) => Some(session),
            Err(error) => {
                tracing::warn!(error = %error, "Discarding unreadable Alist session cache entry");
                let _ = store.delete(key).await;
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
            let envelope = self.encode_sensitive(session)?;
            let _ = store.set(key, &envelope, ALIST_SESSION_CACHE_TTL).await;
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
        match credential {
            ProviderCredential::Alist { host, .. } => Ok(AlistBinding {
                host,
                server_id: server_id.to_string(),
                credential_owner_id: user_id.to_string(),
                credential_revision: revision,
                provider_instance_name,
            }),
            _ => Err(ProviderError::InvalidCredentialType),
        }
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
        let ProviderCredential::Alist {
            host,
            username,
            password,
            otp_secret,
        } = credential
        else {
            return Err(ProviderError::InvalidCredentialType);
        };

        let session_key = Self::alist_session_key(
            user_id,
            server_id,
            &revision,
            provider_instance_name.as_deref(),
        );
        if let Some(session) = self.cached_alist_session(&session_key).await {
            return Ok(AlistAccess {
                host: session.host,
                token: session.token,
                server_id: server_id.to_string(),
                credential_owner_id: user_id.to_string(),
                credential_revision: revision,
                provider_instance_name,
            });
        }

        let _lock = if let Some(store) = &self.store {
            store
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
                .ok()
        } else {
            None
        };

        if let Some(session) = self.cached_alist_session(&session_key).await {
            return Ok(AlistAccess {
                host: session.host,
                token: session.token,
                server_id: server_id.to_string(),
                credential_owner_id: user_id.to_string(),
                credential_revision: revision,
                provider_instance_name,
            });
        }

        let otp_code = otp_secret.as_deref().map_or_else(
            || Ok(String::new()),
            |secret| {
                ProviderCredential::current_alist_otp_code(secret)
                    .map_err(ProviderError::InvalidConfig)
            },
        )?;
        let login_req = synctv_media_providers::grpc::alist::LoginReq {
            host: host.clone(),
            username,
            credential: Some(
                synctv_media_providers::grpc::alist::login_req::Credential::HashedPassword(
                    password,
                ),
            ),
            otp_code,
        };
        let token = self
            .alist_provider
            .login_with_context(
                login_req,
                provider_instance_name.as_deref(),
                request_context,
            )
            .await?;

        let session = CachedAlistSession {
            host: host.clone(),
            token: token.clone(),
        };
        self.cache_alist_session(&session_key, &session).await?;

        Ok(AlistAccess {
            host,
            token,
            server_id: server_id.to_string(),
            credential_owner_id: user_id.to_string(),
            credential_revision: revision,
            provider_instance_name,
        })
    }

    async fn bilibili_access(
        &self,
        user_id: UserId,
        request_context: Option<&ExecutionControl>,
    ) -> Result<BilibiliAccess, ProviderError> {
        let server_id = UserProviderCredential::bilibili_server_id();
        let Some(record) = self
            .load_record_optional(BilibiliProvider::NAME, user_id, &server_id, request_context)
            .await?
        else {
            return Ok(BilibiliAccess {
                cookies: HashMap::new(),
                credential_cache_partition: "anon".to_string(),
                authenticated: false,
            });
        };
        if record.is_expired() {
            return Ok(BilibiliAccess {
                cookies: HashMap::new(),
                credential_cache_partition: "anon".to_string(),
                authenticated: false,
            });
        }

        let resolved = Self::resolved_credential(&record)?;
        match resolved.credential {
            ProviderCredential::Bilibili { cookies } => Ok(BilibiliAccess {
                cookies,
                credential_cache_partition: format!(
                    "auth:{user_id}:{server_id}:{}",
                    resolved.revision
                ),
                authenticated: true,
            }),
            _ => Err(ProviderError::InvalidCredentialType),
        }
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
        match resolved.credential {
            ProviderCredential::Emby {
                host,
                api_key,
                emby_user_id,
            } => Ok(EmbyAccess {
                host,
                api_key,
                emby_user_id,
                server_id: server_id.to_string(),
                credential_owner_id: user_id.to_string(),
                credential_revision: resolved.revision,
                provider_instance_name,
            }),
            _ => Err(ProviderError::InvalidCredentialType),
        }
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
    use crate::provider::store::InMemoryProviderStore;
    use crate::repository::ProviderInstanceRepository;
    use crate::service::RemoteProviderManager;
    use chrono::Utc;

    fn test_service(store: Arc<dyn ProviderStore>) -> CachedProviderAccessService {
        let credential_pool = sqlx::PgPool::connect_lazy("postgresql://fake").expect("lazy pool");
        let credential_repo = Arc::new(UserProviderCredentialRepository::new(credential_pool));
        let provider_pool = sqlx::PgPool::connect_lazy("postgresql://fake").expect("lazy pool");
        let provider_instance_repo = Arc::new(ProviderInstanceRepository::new(provider_pool));
        let provider_instance_manager =
            Arc::new(RemoteProviderManager::new(provider_instance_repo));
        let alist_provider = Arc::new(AlistProvider::new(provider_instance_manager));

        CachedProviderAccessService::new(credential_repo, alist_provider).with_store(store)
    }

    fn credential_record(
        provider: &str,
        user_id: UserId,
        server_id: &str,
        provider_instance_name: Option<&str>,
        credential: ProviderCredential,
    ) -> UserProviderCredential {
        let now = Utc::now();
        UserProviderCredential {
            id: 42,
            user_id,
            provider: provider.to_string(),
            server_id: server_id.to_string(),
            provider_instance_name: provider_instance_name.map(std::string::ToString::to_string),
            credential_data: serde_json::to_value(credential).expect("credential serializes"),
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
        let envelope = service
            .encode_sensitive(record)
            .expect("credential cache entry encodes");
        store
            .set(
                &CachedProviderAccessService::binding_key(
                    &record.provider,
                    record.user_id,
                    &record.server_id,
                ),
                &envelope,
                Duration::from_secs(60),
            )
            .await
            .expect("credential cache write succeeds");
    }

    #[tokio::test]
    async fn alist_access_uses_bound_provider_instance_for_session_cache() {
        let store = Arc::new(InMemoryProviderStore::new(16));
        let service = test_service(store.clone());
        let user_id = UserId::from(7);
        let server_id = "alist-main";
        let record = credential_record(
            AlistProvider::NAME,
            user_id,
            server_id,
            Some("credential-instance"),
            ProviderCredential::alist(
                "https://alist.example.test".to_string(),
                "alice".to_string(),
                "hashed-password".to_string(),
                None,
            ),
        );
        cache_record(&service, store.as_ref(), &record).await;

        let revision = credential_revision(record.id, record.updated_at);
        let stale_session = service
            .encode_sensitive(&CachedAlistSession {
                host: "https://stale.example.test".to_string(),
                token: "stale-token".to_string(),
            })
            .expect("stale session encodes");
        store
            .set(
                &CachedProviderAccessService::alist_session_key(
                    user_id,
                    server_id,
                    &revision,
                    Some("credential-instance"),
                ),
                &stale_session,
                Duration::from_secs(60),
            )
            .await
            .expect("stale session cache write succeeds");
        let bound_session = service
            .encode_sensitive(&CachedAlistSession {
                host: "https://bound.example.test".to_string(),
                token: "bound-token".to_string(),
            })
            .expect("bound session encodes");
        store
            .set(
                &CachedProviderAccessService::alist_session_key(
                    user_id,
                    server_id,
                    &revision,
                    Some("bound-instance"),
                ),
                &bound_session,
                Duration::from_secs(60),
            )
            .await
            .expect("bound session cache write succeeds");

        let access = service
            .alist_access(user_id, server_id, Some("bound-instance"), None)
            .await
            .expect("alist access resolves");

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
        let user_id = UserId::from(7);
        let server_id = "emby-main";
        let record = credential_record(
            EmbyProvider::NAME,
            user_id,
            server_id,
            Some("credential-instance"),
            ProviderCredential::emby(
                "https://emby.example.test".to_string(),
                "api-key".to_string(),
                "emby-user".to_string(),
            ),
        );
        cache_record(&service, store.as_ref(), &record).await;

        let access = service
            .emby_access(user_id, server_id, Some("bound-instance"), None)
            .await
            .expect("emby access resolves");

        assert_eq!(
            access.provider_instance_name.as_deref(),
            Some("bound-instance")
        );
    }
}
