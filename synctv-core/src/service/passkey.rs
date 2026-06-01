use std::{sync::Arc, time::Duration};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use webauthn_rs::prelude::{
    CredentialID, DiscoverableAuthentication, DiscoverableKey, Passkey, PasskeyAuthentication,
    PasskeyRegistration, PublicKeyCredential, RegisterPublicKeyCredential, Url, Webauthn,
    WebauthnBuilder,
};

use crate::{
    config::WebAuthnConfig,
    models::{SignupMethod, User, UserId},
    repository::{WebAuthnCredential, WebAuthnCredentialRepository},
    service::{session_store::RedisJsonSessionStore, RegistrationMode},
    Error, InternalExt, RedisConnectionRuntime, Result, SharedStateMode, SharedStateProfile,
};

const PASSKEY_SESSION_TTL_SECS: u64 = 300;
const PASSKEY_SESSION_CAPACITY: u64 = 10_000;
const PASSKEY_USER_HANDLE_NAMESPACE: uuid::Uuid =
    uuid::Uuid::from_u128(0x4f5b_14fc_3148_5d64_9e5a_4f6d_9af9_b0f2);
const PASSKEY_SESSION_REDIS_NAMESPACE: &str = "auth:passkey:session";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PasskeySession {
    AccountRegistration {
        username: String,
        email: Option<String>,
        credential_name: Option<String>,
        state: PasskeyRegistration,
    },
    Registration {
        user_id: UserId,
        credential_name: Option<String>,
        state: PasskeyRegistration,
    },
    Login {
        user_id: UserId,
        brute_force_key: String,
        state: PasskeyAuthentication,
    },
    DiscoverableLogin {
        state: DiscoverableAuthentication,
    },
    Verification {
        user_id: UserId,
        state: PasskeyAuthentication,
    },
}

#[async_trait::async_trait]
pub trait PasskeySessionStore: Send + Sync {
    async fn store(&self, session_id: &str, session: &PasskeySession, ttl: Duration) -> Result<()>;
    async fn consume(&self, session_id: &str) -> Result<Option<PasskeySession>>;
    fn supports_cross_node_single_use(&self) -> bool;
}

#[must_use]
pub fn local_passkey_session_store() -> Arc<dyn PasskeySessionStore> {
    Arc::new(InMemoryPasskeySessionStore::new())
}

#[must_use]
pub fn shared_passkey_session_store(
    runtime: Arc<dyn RedisConnectionRuntime>,
    key_prefix: impl Into<String>,
) -> Arc<dyn PasskeySessionStore> {
    Arc::new(RedisPasskeySessionStore::from_runtime(runtime, key_prefix))
}

pub fn passkey_session_store_from_shared_state_profile(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn PasskeySessionStore>> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            let runtime =
                profile.require_shared_runtime("single-use WebAuthn challenge storage")?;
            Ok(shared_passkey_session_store(
                runtime,
                profile.key_prefix().to_string(),
            ))
        }
        SharedStateMode::SharedBestEffort => Ok(shared_passkey_session_store(
            profile
                .shared_runtime()
                .expect("shared state profile guarantees runtime in best-effort mode"),
            profile.key_prefix().to_string(),
        )),
        SharedStateMode::LocalOnly => Ok(local_passkey_session_store()),
    }
}

#[derive(Clone)]
struct PasskeySessionEntry {
    session: PasskeySession,
    ttl: Duration,
}

struct PasskeySessionExpiry;

impl moka::Expiry<String, PasskeySessionEntry> for PasskeySessionExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &PasskeySessionEntry,
        _now: std::time::Instant,
    ) -> Option<Duration> {
        Some(value.ttl)
    }
}

pub struct InMemoryPasskeySessionStore {
    entries: moka::sync::Cache<String, PasskeySessionEntry>,
}

impl InMemoryPasskeySessionStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: moka::sync::Cache::builder()
                .max_capacity(PASSKEY_SESSION_CAPACITY)
                .expire_after(PasskeySessionExpiry)
                .build(),
        }
    }
}

impl Default for InMemoryPasskeySessionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl PasskeySessionStore for InMemoryPasskeySessionStore {
    async fn store(&self, session_id: &str, session: &PasskeySession, ttl: Duration) -> Result<()> {
        self.entries.insert(
            session_id.to_string(),
            PasskeySessionEntry {
                session: session.clone(),
                ttl,
            },
        );
        Ok(())
    }

    async fn consume(&self, session_id: &str) -> Result<Option<PasskeySession>> {
        if self.entries.get(session_id).is_none() {
            return Ok(None);
        }
        Ok(self.entries.remove(session_id).map(|entry| entry.session))
    }

    fn supports_cross_node_single_use(&self) -> bool {
        false
    }
}

pub struct RedisPasskeySessionStore {
    store: RedisJsonSessionStore,
}

impl RedisPasskeySessionStore {
    #[must_use]
    pub fn from_runtime(
        runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            store: RedisJsonSessionStore::new(runtime, key_prefix),
        }
    }
}

#[async_trait::async_trait]
impl PasskeySessionStore for RedisPasskeySessionStore {
    async fn store(&self, session_id: &str, session: &PasskeySession, ttl: Duration) -> Result<()> {
        self.store
            .store(
                PASSKEY_SESSION_REDIS_NAMESPACE,
                session_id,
                session,
                ttl,
                "Failed to serialize WebAuthn session",
                "store WebAuthn session in Redis",
            )
            .await
    }

    async fn consume(&self, session_id: &str) -> Result<Option<PasskeySession>> {
        self.store
            .consume(
                PASSKEY_SESSION_REDIS_NAMESPACE,
                session_id,
                "Failed to deserialize WebAuthn session",
                "consume WebAuthn session from Redis",
            )
            .await
    }

    fn supports_cross_node_single_use(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub struct StartPasskeyRegistration {
    pub session_id: String,
    pub options_json: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct StartPasskeyLogin {
    pub session_id: String,
    pub options_json: Vec<u8>,
}

#[derive(Clone)]
pub struct PasskeyService {
    webauthn: Arc<Webauthn>,
    repository: WebAuthnCredentialRepository,
    user_service: Arc<crate::service::UserService>,
    session_store: Arc<dyn PasskeySessionStore>,
}

impl PasskeyService {
    pub fn new(
        config: &WebAuthnConfig,
        repository: WebAuthnCredentialRepository,
        user_service: Arc<crate::service::UserService>,
        session_store: Arc<dyn PasskeySessionStore>,
    ) -> Result<Self> {
        let rp_origin = Url::parse(&config.rp_origin)
            .map_err(|error| Error::InvalidInput(format!("Invalid webauthn.rp_origin: {error}")))?;
        let mut builder = WebauthnBuilder::new(config.rp_id.trim(), &rp_origin)
            .map_err(|error| Error::InvalidInput(format!("Invalid WebAuthn config: {error}")))?
            .rp_name(config.rp_name.trim())
            .allow_subdomains(config.allow_subdomains)
            .allow_any_port(config.allow_any_port)
            .timeout(Duration::from_secs(config.timeout_seconds));

        for origin in &config.allowed_origins {
            let parsed = Url::parse(origin).map_err(|error| {
                Error::InvalidInput(format!("Invalid webauthn.allowed_origins entry: {error}"))
            })?;
            builder = builder.append_allowed_origin(&parsed);
        }

        let webauthn = builder
            .build()
            .map_err(|error| Error::InvalidInput(format!("Invalid WebAuthn config: {error}")))?;

        Ok(Self {
            webauthn: Arc::new(webauthn),
            repository,
            user_service,
            session_store,
        })
    }

    pub fn encode_credential_id(credential_id: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(credential_id)
    }

    pub fn decode_credential_id(value: &str) -> Result<Vec<u8>> {
        let value = value.trim();
        if value.is_empty() {
            return Err(Error::InvalidInput(
                "Invalid passkey credential_id".to_string(),
            ));
        }
        let credential_id = URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| Error::InvalidInput("Invalid passkey credential_id".to_string()))?;
        if credential_id.is_empty() {
            return Err(Error::InvalidInput(
                "Invalid passkey credential_id".to_string(),
            ));
        }
        Ok(credential_id)
    }

    fn user_uuid(user_id: UserId) -> uuid::Uuid {
        uuid::Uuid::new_v5(
            &PASSKEY_USER_HANDLE_NAMESPACE,
            &user_id.as_i64().to_be_bytes(),
        )
    }

    fn to_credential_ids(credentials: &[WebAuthnCredential]) -> Vec<CredentialID> {
        credentials
            .iter()
            .map(|credential| CredentialID::from(credential.credential_id.clone()))
            .collect()
    }

    fn passkeys(credentials: &[WebAuthnCredential]) -> Vec<Passkey> {
        credentials
            .iter()
            .map(|credential| credential.passkey.clone())
            .collect()
    }

    fn validate_sign_in_method_delete_policy(remaining_sign_in_method_count: usize) -> Result<()> {
        if remaining_sign_in_method_count == 0 {
            return Err(Error::InvalidInput(
                "Cannot delete the last sign-in method".to_string(),
            ));
        }
        Ok(())
    }

    pub async fn start_registration(
        &self,
        user: &User,
        credential_name: Option<String>,
    ) -> Result<StartPasskeyRegistration> {
        let existing = self.repository.list_by_user(&user.id).await?;
        let exclude_credentials = Self::to_credential_ids(&existing);
        let (challenge, state) = self
            .webauthn
            .start_passkey_registration(
                Self::user_uuid(user.id),
                &user.username,
                &user.username,
                Some(exclude_credentials),
            )
            .map_err(|error| {
                Error::InvalidInput(format!("Failed to start passkey registration: {error}"))
            })?;
        let session_id = synctv_common::snanoid!(48);
        self.session_store
            .store(
                &session_id,
                &PasskeySession::Registration {
                    user_id: user.id,
                    credential_name,
                    state,
                },
                Duration::from_secs(PASSKEY_SESSION_TTL_SECS),
            )
            .await?;

        Ok(StartPasskeyRegistration {
            session_id,
            options_json: serde_json::to_vec(&challenge)
                .internal_with_err("Failed to serialize passkey registration challenge")?,
        })
    }

    pub async fn start_account_registration(
        &self,
        username: String,
        email: Option<String>,
        credential_name: Option<String>,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&synctv_common::ExecutionControl>,
    ) -> Result<StartPasskeyRegistration> {
        let username = username.trim().to_string();
        let email = email
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        self.user_service
            .ensure_registration_review_supported(RegistrationMode::WebAuthn)?;

        self.user_service
            .validate_registration_identity_with_control(
                &username,
                email.as_deref(),
                client_ip,
                control,
            )
            .await?;

        let user_handle = uuid::Uuid::new_v4();
        let (challenge, state) = self
            .webauthn
            .start_passkey_registration(user_handle, &username, &username, Some(Vec::new()))
            .map_err(|error| {
                Error::InvalidInput(format!("Failed to start passkey registration: {error}"))
            })?;
        let session_id = synctv_common::snanoid!(48);
        self.session_store
            .store(
                &session_id,
                &PasskeySession::AccountRegistration {
                    username,
                    email,
                    credential_name,
                    state,
                },
                Duration::from_secs(PASSKEY_SESSION_TTL_SECS),
            )
            .await?;

        Ok(StartPasskeyRegistration {
            session_id,
            options_json: serde_json::to_vec(&challenge)
                .internal_with_err("Failed to serialize passkey registration challenge")?,
        })
    }

    pub async fn finish_registration(
        &self,
        session_id: &str,
        credential_json: &[u8],
        authenticated_user_id: &UserId,
    ) -> Result<WebAuthnCredential> {
        let Some(PasskeySession::Registration {
            user_id,
            credential_name,
            state,
        }) = self.session_store.consume(session_id).await?
        else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };
        if &user_id != authenticated_user_id {
            return Err(Error::Authorization(
                "Passkey registration session belongs to a different user".to_string(),
            ));
        }

        let credential: RegisterPublicKeyCredential = serde_json::from_slice(credential_json)
            .map_err(|_| Error::Authentication("Authentication failed".to_string()))?;
        let passkey = self
            .webauthn
            .finish_passkey_registration(&credential, &state)
            .map_err(|_| Error::Authentication("Authentication failed".to_string()))?;

        if self
            .repository
            .get_by_credential_id(passkey.cred_id().as_ref())
            .await?
            .is_some()
        {
            return Err(Error::AlreadyExists(
                "Passkey credential is already registered".to_string(),
            ));
        }

        self.repository
            .create(&user_id, &passkey, credential_name.as_deref())
            .await
    }

    pub async fn finish_account_registration(
        &self,
        session_id: &str,
        credential_json: &[u8],
        client_ip: Option<std::net::IpAddr>,
        control: Option<&synctv_common::ExecutionControl>,
    ) -> Result<(User, Option<String>, Option<String>)> {
        let Some(PasskeySession::AccountRegistration {
            username,
            email,
            credential_name,
            state,
        }) = self.session_store.consume(session_id).await?
        else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let registration_policy = self
            .user_service
            .ensure_registration_review_supported(RegistrationMode::WebAuthn)?;

        self.user_service
            .validate_registration_identity_with_control(
                &username,
                email.as_deref(),
                client_ip,
                control,
            )
            .await?;

        let credential: RegisterPublicKeyCredential = serde_json::from_slice(credential_json)
            .map_err(|_| Error::Authentication("Authentication failed".to_string()))?;
        let passkey = self
            .webauthn
            .finish_passkey_registration(&credential, &state)
            .map_err(|_| Error::Authentication("Authentication failed".to_string()))?;

        if self
            .repository
            .get_by_credential_id(passkey.cred_id().as_ref())
            .await?
            .is_some()
        {
            return Err(Error::AlreadyExists(
                "Passkey credential is already registered".to_string(),
            ));
        }

        if registration_policy.need_review {
            let pending_user = self
                .user_service
                .create_webauthn_registration_request(
                    &username,
                    email.as_deref(),
                    &passkey,
                    credential_name.as_deref(),
                )
                .await?;
            return Ok((pending_user, None, None));
        }

        let user = User::new(username.clone(), SignupMethod::WebAuthn);

        let mut tx: Transaction<'_, Postgres> = self.user_service.pool().begin().await?;
        let created_user = self
            .user_service
            .repository
            .create_with_executor(&user, &mut *tx)
            .await?;
        self.user_service
            .user_email_repository
            .create_for_user_with_executor(&created_user, email.as_deref(), &mut *tx)
            .await?;
        self.repository
            .create_with_executor(
                &created_user.id,
                &passkey,
                credential_name.as_deref(),
                &mut *tx,
            )
            .await?;
        tx.commit().await?;

        self.user_service
            .cache_username_best_effort(
                &created_user.id,
                &created_user.username,
                "passkey_register",
            )
            .await;
        self.user_service
            .notify_user_invalidation(&created_user.id)
            .await;

        let login = self
            .user_service
            .login_with_verified_external_credential_with_control(
                &created_user.id,
                &format!("passkey:{}", created_user.username),
                client_ip,
                control,
            )
            .await?;
        match login {
            crate::service::AuthenticatedLogin::Complete {
                user,
                email: _,
                access_token,
                refresh_token,
            } => Ok((user, Some(access_token), Some(refresh_token))),
            crate::service::AuthenticatedLogin::MfaRequired { .. } => Err(Error::Internal(
                "New passkey registrations must not require MFA during initial token issuance"
                    .to_string(),
            )),
        }
    }

    pub async fn start_login(
        &self,
        identifier: Option<&str>,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&synctv_common::ExecutionControl>,
    ) -> Result<StartPasskeyLogin> {
        if let Some(identifier) = identifier {
            return self
                .start_username_login(identifier, client_ip, control)
                .await;
        }

        self.user_service
            .check_passkey_discoverable_login_allowed_with_control(client_ip, control)
            .await?;

        let (challenge, state) = self
            .webauthn
            .start_discoverable_authentication()
            .map_err(|_| Error::Authentication("Authentication failed".to_string()))?;
        let session_id = synctv_common::snanoid!(48);
        self.session_store
            .store(
                &session_id,
                &PasskeySession::DiscoverableLogin { state },
                Duration::from_secs(PASSKEY_SESSION_TTL_SECS),
            )
            .await?;

        Ok(StartPasskeyLogin {
            session_id,
            options_json: serde_json::to_vec(&challenge)
                .internal_with_err("Failed to serialize passkey login challenge")?,
        })
    }

    async fn start_username_login(
        &self,
        identifier: &str,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&synctv_common::ExecutionControl>,
    ) -> Result<StartPasskeyLogin> {
        let brute_force_key =
            crate::service::UserService::normalize_external_login_identifier(identifier);
        let Some(user) = self
            .user_service
            .start_verified_external_login_with_control(identifier, client_ip, control)
            .await?
        else {
            self.user_service
                .record_external_login_failure_with_control(
                    &brute_force_key,
                    false,
                    client_ip,
                    control,
                )
                .await;
            return Err(Error::Authentication("Authentication failed".to_string()));
        };
        let credentials = self.repository.list_by_user(&user.id).await?;
        if credentials.is_empty() {
            self.user_service
                .record_external_login_failure_with_control(
                    &brute_force_key,
                    true,
                    client_ip,
                    control,
                )
                .await;
            return Err(Error::Authentication("Authentication failed".to_string()));
        }

        let (challenge, state) = self
            .webauthn
            .start_passkey_authentication(&Self::passkeys(&credentials))
            .map_err(|_| Error::Authentication("Authentication failed".to_string()))?;
        let session_id = synctv_common::snanoid!(48);
        self.session_store
            .store(
                &session_id,
                &PasskeySession::Login {
                    user_id: user.id,
                    brute_force_key,
                    state,
                },
                Duration::from_secs(PASSKEY_SESSION_TTL_SECS),
            )
            .await?;

        Ok(StartPasskeyLogin {
            session_id,
            options_json: serde_json::to_vec(&challenge)
                .internal_with_err("Failed to serialize passkey login challenge")?,
        })
    }

    pub async fn start_user_verification(&self, user_id: &UserId) -> Result<StartPasskeyLogin> {
        let credentials = self.repository.list_by_user(user_id).await?;
        if credentials.is_empty() {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }

        let (challenge, state) = self
            .webauthn
            .start_passkey_authentication(&Self::passkeys(&credentials))
            .map_err(|_| Error::Authentication("Authentication failed".to_string()))?;
        let session_id = synctv_common::snanoid!(48);
        self.session_store
            .store(
                &session_id,
                &PasskeySession::Verification {
                    user_id: *user_id,
                    state,
                },
                Duration::from_secs(PASSKEY_SESSION_TTL_SECS),
            )
            .await?;

        Ok(StartPasskeyLogin {
            session_id,
            options_json: serde_json::to_vec(&challenge)
                .internal_with_err("Failed to serialize passkey verification challenge")?,
        })
    }

    pub async fn finish_login(
        &self,
        session_id: &str,
        credential_json: &[u8],
        client_ip: Option<std::net::IpAddr>,
        control: Option<&synctv_common::ExecutionControl>,
    ) -> Result<crate::service::AuthenticatedLogin> {
        let Some(session) = self.session_store.consume(session_id).await? else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        match session {
            PasskeySession::Login {
                user_id,
                brute_force_key,
                state,
            } => {
                self.finish_username_login(
                    user_id,
                    brute_force_key,
                    state,
                    credential_json,
                    client_ip,
                    control,
                )
                .await
            }
            PasskeySession::DiscoverableLogin { state } => {
                self.finish_discoverable_login(state, credential_json, client_ip, control)
                    .await
            }
            _ => Err(Error::Authentication("Authentication failed".to_string())),
        }
    }

    async fn finish_username_login(
        &self,
        user_id: UserId,
        brute_force_key: String,
        state: PasskeyAuthentication,
        credential_json: &[u8],
        client_ip: Option<std::net::IpAddr>,
        control: Option<&synctv_common::ExecutionControl>,
    ) -> Result<crate::service::AuthenticatedLogin> {
        let Ok(credential) = serde_json::from_slice::<PublicKeyCredential>(credential_json) else {
            self.user_service
                .record_external_login_failure_with_control(
                    &brute_force_key,
                    true,
                    client_ip,
                    control,
                )
                .await;
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let Ok(auth_result) = self
            .webauthn
            .finish_passkey_authentication(&credential, &state)
        else {
            self.user_service
                .record_external_login_failure_with_control(
                    &brute_force_key,
                    true,
                    client_ip,
                    control,
                )
                .await;
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let mut stored = self
            .repository
            .get_by_credential_id(auth_result.cred_id().as_ref())
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;
        if stored.user_id != user_id {
            self.user_service
                .record_external_login_failure_with_control(
                    &brute_force_key,
                    true,
                    client_ip,
                    control,
                )
                .await;
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        let changed = stored
            .passkey
            .update_credential(&auth_result)
            .unwrap_or(false);
        if changed || i64::from(auth_result.counter()) != stored.sign_count {
            self.repository
                .update_after_authentication(
                    &stored.credential_id,
                    &stored.passkey,
                    i64::from(auth_result.counter()),
                )
                .await?;
        }

        self.user_service
            .login_with_verified_external_credential_with_control(
                &user_id,
                &brute_force_key,
                client_ip,
                control,
            )
            .await
    }

    async fn finish_discoverable_login(
        &self,
        state: DiscoverableAuthentication,
        credential_json: &[u8],
        client_ip: Option<std::net::IpAddr>,
        control: Option<&synctv_common::ExecutionControl>,
    ) -> Result<crate::service::AuthenticatedLogin> {
        let Ok(credential) = serde_json::from_slice::<PublicKeyCredential>(credential_json) else {
            self.record_discoverable_login_failure(client_ip, control)
                .await;
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let Ok((_user_handle, credential_id)) = self
            .webauthn
            .identify_discoverable_authentication(&credential)
        else {
            self.record_discoverable_login_failure(client_ip, control)
                .await;
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let Some(mut stored) = self.repository.get_by_credential_id(credential_id).await? else {
            self.record_discoverable_login_failure(client_ip, control)
                .await;
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let discoverable_key = DiscoverableKey::from(stored.passkey.clone());
        let Ok(auth_result) = self.webauthn.finish_discoverable_authentication(
            &credential,
            state,
            &[discoverable_key],
        ) else {
            self.record_discoverable_login_failure(client_ip, control)
                .await;
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let changed = stored
            .passkey
            .update_credential(&auth_result)
            .unwrap_or(false);
        if changed || i64::from(auth_result.counter()) != stored.sign_count {
            self.repository
                .update_after_authentication(
                    &stored.credential_id,
                    &stored.passkey,
                    i64::from(auth_result.counter()),
                )
                .await?;
        }

        let brute_force_key = format!(
            "passkey:{}",
            Self::encode_credential_id(&stored.credential_id)
        );
        self.user_service
            .login_with_verified_external_credential_with_control(
                &stored.user_id,
                &brute_force_key,
                client_ip,
                control,
            )
            .await
    }

    async fn record_discoverable_login_failure(
        &self,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&synctv_common::ExecutionControl>,
    ) {
        if let Err(error) = self
            .user_service
            .record_passkey_discoverable_login_failure_with_control(client_ip, control)
            .await
        {
            tracing::warn!(error = %error, "Failed to record passkey login failure for brute-force tracking");
        }
    }

    pub async fn finish_user_verification(
        &self,
        session_id: &str,
        credential_json: &[u8],
        authenticated_user_id: &UserId,
    ) -> Result<()> {
        let Some(PasskeySession::Verification { user_id, state }) =
            self.session_store.consume(session_id).await?
        else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };
        if &user_id != authenticated_user_id {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }

        let credential: PublicKeyCredential = serde_json::from_slice(credential_json)
            .map_err(|_| Error::Authentication("Authentication failed".to_string()))?;
        let auth_result = self
            .webauthn
            .finish_passkey_authentication(&credential, &state)
            .map_err(|_| Error::Authentication("Authentication failed".to_string()))?;

        let mut stored = self
            .repository
            .get_by_credential_id(auth_result.cred_id().as_ref())
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;
        if stored.user_id != user_id {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }

        let changed = stored
            .passkey
            .update_credential(&auth_result)
            .unwrap_or(false);
        if changed || i64::from(auth_result.counter()) != stored.sign_count {
            self.repository
                .update_after_authentication(
                    &stored.credential_id,
                    &stored.passkey,
                    i64::from(auth_result.counter()),
                )
                .await?;
        }

        Ok(())
    }

    pub async fn list_credentials(&self, user_id: &UserId) -> Result<Vec<WebAuthnCredential>> {
        self.repository.list_by_user(user_id).await
    }

    pub async fn delete_credential(&self, user_id: &UserId, credential_id: &[u8]) -> Result<bool> {
        let mut tx: Transaction<'_, Postgres> = self.user_service.pool().begin().await?;
        self.user_service
            .repository
            .get_by_id_for_update_with_executor(user_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound("User not found".to_string()))?;
        let exists = self
            .repository
            .exists_for_user_with_executor(user_id, credential_id, &mut *tx)
            .await?;
        if !exists {
            tx.commit().await?;
            return Ok(false);
        }

        let remaining_factors = self
            .user_service
            .user_preferences_repository
            .auth_factors_with_excluded_passkey(user_id, Some(credential_id), &mut *tx)
            .await?;
        let active_oauth2_identity_count = self
            .user_service
            .active_oauth2_identity_count_with_executor(user_id, &mut *tx)
            .await?;
        let remaining_sign_in_method_count = crate::service::UserService::sign_in_method_count(
            &remaining_factors,
            active_oauth2_identity_count,
        );
        Self::validate_sign_in_method_delete_policy(remaining_sign_in_method_count)?;
        if self
            .user_service
            .user_preferences_repository
            .two_factor_enabled_with_executor(user_id, &mut *tx)
            .await?
            && !remaining_factors.supports_two_factor()
        {
            return Err(Error::InvalidInput(
                    "Cannot delete this passkey while two-factor authentication is enabled because the remaining verification methods are insufficient".to_string(),
                ));
        }

        let deleted = self
            .repository
            .delete_for_user_with_executor(user_id, credential_id, &mut *tx)
            .await?;
        tx.commit().await?;
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_registration_session() -> PasskeySession {
        let origin = Url::parse("https://app.example.com").expect("valid origin");
        let webauthn = WebauthnBuilder::new("app.example.com", &origin)
            .expect("valid WebAuthn builder")
            .build()
            .expect("valid WebAuthn config");
        let (_challenge, state) = webauthn
            .start_passkey_registration(uuid::Uuid::new_v4(), "alice_123", "alice_123", None)
            .expect("registration should start");

        PasskeySession::Registration {
            user_id: UserId::expect_positive(123_i64),
            credential_name: Some("Laptop".to_string()),
            state,
        }
    }

    #[tokio::test]
    async fn local_passkey_session_store_consumes_sessions_once() {
        let store = local_passkey_session_store();
        let session = sample_registration_session();

        store
            .store("session-1", &session, Duration::from_mins(1))
            .await
            .expect("store session");

        assert!(
            store
                .consume("session-1")
                .await
                .expect("consume session")
                .is_some(),
            "first consume should return the stored session"
        );
        assert!(
            store
                .consume("session-1")
                .await
                .expect("consume session again")
                .is_none(),
            "second consume must not replay a WebAuthn challenge"
        );
    }

    #[tokio::test]
    async fn local_passkey_session_store_expires_sessions() {
        let store = local_passkey_session_store();
        let session = sample_registration_session();

        store
            .store("session-ttl", &session, Duration::from_millis(10))
            .await
            .expect("store session");
        tokio::time::sleep(Duration::from_millis(30)).await;

        assert!(
            store
                .consume("session-ttl")
                .await
                .expect("consume expired session")
                .is_none(),
            "expired WebAuthn challenge state must not be accepted"
        );
    }

    #[test]
    fn passkey_credential_id_roundtrips_url_safe_base64() {
        let credential_id = b"credential-id-with-binary-\0-\xff";
        let encoded = PasskeyService::encode_credential_id(credential_id);

        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));
        assert_eq!(
            PasskeyService::decode_credential_id(&encoded).expect("decode credential id"),
            credential_id
        );
    }

    #[test]
    fn passkey_credential_id_decode_rejects_empty_or_invalid_values() {
        assert!(PasskeyService::decode_credential_id("").is_err());
        assert!(PasskeyService::decode_credential_id("   ").is_err());
        assert!(PasskeyService::decode_credential_id("not valid!!").is_err());
    }

    #[test]
    fn passkey_user_uuid_is_stable_standard_and_not_raw_database_id_bytes() {
        let user_id = UserId::expect_positive(42_i64);
        let user_uuid = PasskeyService::user_uuid(user_id);

        assert_eq!(user_uuid, PasskeyService::user_uuid(user_id));
        assert_ne!(
            user_uuid,
            PasskeyService::user_uuid(UserId::expect_positive(43_i64))
        );
        assert_eq!(user_uuid.get_version_num(), 5);
        assert_ne!(&user_uuid.as_bytes()[8..16], &42_i64.to_be_bytes());
    }

    #[test]
    fn deleting_passkey_requires_a_remaining_sign_in_method() {
        assert!(matches!(
            PasskeyService::validate_sign_in_method_delete_policy(0),
            Err(Error::InvalidInput(_))
        ));
        assert!(PasskeyService::validate_sign_in_method_delete_policy(1).is_ok());
        assert!(PasskeyService::validate_sign_in_method_delete_policy(2).is_ok());
    }
}
