//! Client API Implementation
//!
//! Unified implementation for all client API operations.
//! Used by both HTTP and gRPC handlers.
//!
//! Split into sub-modules by domain:
//! - `auth`: register, login, `refresh_token`
//! - `user`: `get_profile`, `set_username`, `set_password`
//! - `room`: create/get/join/leave/delete room, settings, chat, hot rooms
//! - `member`: `get_members`, kick, ban, unban, `set_permissions`
//! - `media`: add/remove/edit/swap media, batch operations, playlist items
//! - `playlist`: create/update/delete/list playlists
//! - `playback`: play, pause, seek, speed, `set_current_media`, `get_playback_state`
//! - `webrtc`: ICE servers, network quality

mod auth;
pub use auth::LogoutOutcome;
pub(crate) mod media;
mod member;
mod playback;
mod playback_lifecycle;
pub(crate) mod playlist;
mod room;
pub(crate) mod stream;
mod user;
mod webrtc;
pub(crate) use playback::{
    build_start_playback_request, build_update_playback, PlaybackUpdateCommand,
};
pub(crate) use room::build_create_websocket_ticket_request;

// Proto conversion helpers used across impls modules within this crate.
pub(crate) mod convert;

#[cfg(test)]
mod tests;

use futures::future::BoxFuture;
use std::sync::Arc;
use synctv_core::models::RoomId;
use synctv_core::service::{RoomService, UserService};
use synctv_core::RedisConnectionRuntime;

// Re-export public items from convert module
pub(crate) use convert::user_to_proto;
pub use convert::{
    media_to_proto, proto_role_to_room_role, proto_role_to_user_role, room_role_to_proto,
};

// Room password limits imported from the single source of truth in synctv-core
use synctv_core::validation::{ROOM_PASSWORD_MAX, ROOM_PASSWORD_MIN};

use crate::cluster_fanout::{default_cluster_fanout_service, ClusterFanoutService};
use crate::fanout::{default_room_settings_fanout_service, RoomSettingsFanoutService};
use crate::impls::{
    ApiError, EndpointRateLimitCategory, RequestContext, RequestExecutor, RequestMetadata,
};
use crate::media_fanout::{default_media_fanout_service, MediaFanoutService};
use crate::member_fanout::{default_member_fanout_service, MemberFanoutService};
use crate::membership_event_fanout::{
    default_membership_event_fanout_service, MembershipEventFanoutService,
};
use crate::playlist_fanout::{default_playlist_fanout_service, PlaylistFanoutService};
use crate::realtime_lifecycle::{default_realtime_lifecycle_service, RealtimeLifecycleService};
use crate::room_cache_fanout::{default_room_cache_fanout_service, RoomCacheFanoutService};
use crate::room_lifecycle_fanout::{
    default_room_lifecycle_fanout_service, RoomLifecycleFanoutService,
};
use crate::runtime::{RealtimeConnectionService, RealtimeEventService};

/// Validate a password that is being **set** (create room, set password, update settings).
pub(crate) fn validate_password_for_set(password: &str) -> Result<(), ApiError> {
    // Reject passwords that are purely whitespace. A password of e.g. " "
    // looks non-empty to a length check but provides no protection and confuses users.
    let trimmed = password.trim();
    if trimmed.is_empty() {
        return Err(ApiError::InvalidInput(
            "Room password cannot be empty or whitespace only".to_string(),
        ));
    }
    if trimmed.chars().count() < ROOM_PASSWORD_MIN {
        return Err(ApiError::InvalidInput(format!(
            "Password too short (minimum {ROOM_PASSWORD_MIN} characters)"
        )));
    }
    if password.chars().count() > ROOM_PASSWORD_MAX {
        return Err(ApiError::InvalidInput(format!(
            "Password too long (maximum {ROOM_PASSWORD_MAX} characters)"
        )));
    }
    Ok(())
}

/// Validate a password that is being **verified** during room join.
fn validate_password_for_verify(password: &str) -> Result<(), ApiError> {
    if password.chars().count() > ROOM_PASSWORD_MAX {
        return Err(ApiError::InvalidInput(format!(
            "Password too long (maximum {ROOM_PASSWORD_MAX} characters)"
        )));
    }
    Ok(())
}

/// Configuration for constructing a [`ClientApiImpl`].
///
/// Groups all dependencies into a single struct to avoid `too_many_arguments`.
pub struct ClientApiConfig {
    pub user_service: Arc<UserService>,
    pub room_service: Arc<RoomService>,
    pub connection_service: Arc<dyn RealtimeConnectionService>,
    pub config: Arc<synctv_core::Config>,
    pub publish_key_service: Option<Arc<dyn synctv_core::service::StreamingPublishKeyService>>,
    pub jwt_service: synctv_core::service::JwtService,
    pub live_streaming_infrastructure:
        Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    pub providers_manager: Option<Arc<synctv_core::service::ProvidersManager>>,
    pub settings_registry: Option<Arc<synctv_core::service::SettingsRegistry>>,
    pub credential_encryption: Option<synctv_core::service::CredentialEncryption>,
    pub provider_stores: Option<Arc<dyn synctv_core::provider::store::ProviderStoreResolver>>,
    pub public_id_codec: Arc<crate::PublicIdCodec>,
}

/// Client API implementation
#[derive(Clone)]
pub struct ClientApiImpl {
    pub user_service: Arc<UserService>,
    pub room_service: Arc<RoomService>,
    pub connection_service: Arc<dyn RealtimeConnectionService>,
    pub config: Arc<synctv_core::Config>,
    pub publish_key_service: Option<Arc<dyn synctv_core::service::StreamingPublishKeyService>>,
    pub jwt_service: synctv_core::service::JwtService,
    pub live_streaming_infrastructure:
        Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    pub providers_manager: Option<Arc<synctv_core::service::ProvidersManager>>,
    pub settings_registry: Option<Arc<synctv_core::service::SettingsRegistry>>,
    pub cluster_fanout: Arc<dyn ClusterFanoutService>,
    pub room_settings_fanout: Arc<dyn RoomSettingsFanoutService>,
    pub member_fanout: Arc<dyn MemberFanoutService>,
    pub membership_event_fanout: Arc<dyn MembershipEventFanoutService>,
    pub media_fanout: Arc<dyn MediaFanoutService>,
    pub playlist_fanout: Arc<dyn PlaylistFanoutService>,
    pub room_cache_fanout: Arc<dyn RoomCacheFanoutService>,
    pub realtime_lifecycle: Arc<dyn RealtimeLifecycleService>,
    pub room_lifecycle_fanout: Arc<dyn RoomLifecycleFanoutService>,
    pub realtime_event_service: Option<Arc<dyn RealtimeEventService>>,
    /// Redis runtime abstraction derived from the shared connection when available.
    pub redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
    /// Rate limiter for per-endpoint rate limiting (password checks, etc.)
    pub rate_limiter: Option<Arc<dyn synctv_core::service::RequestRateLimiterService>>,
    /// Resolved built-in STUN URL (e.g. "stun:203.0.113.1:3478"), set only when the
    /// built-in STUN server started successfully with a valid external address.
    /// When `None`, the built-in STUN entry is omitted from ICE server lists.
    pub builtin_stun_url: Option<String>,
    /// Credential encryption for protecting sensitive data in `source_config`
    pub credential_encryption: Option<synctv_core::service::CredentialEncryption>,
    /// Credential repository for resolving stored provider credentials
    pub credential_repo: Option<Arc<synctv_core::repository::UserProviderCredentialRepository>>,
    /// Proxy signing key for generating HMAC-signed proxy URLs
    pub signing_key: Option<Arc<synctv_core::service::ProxySigningKey>>,
    /// Per-provider stores for signed playback version mappings
    pub provider_stores: Option<Arc<dyn synctv_core::provider::store::ProviderStoreResolver>>,
    /// JWT validator for token validation (e.g. live streaming tokens)
    pub jwt_validator: Arc<synctv_core::service::auth::JwtValidator>,
    /// Shared sqids codec for API-facing resource identifiers.
    pub public_id_codec: Arc<crate::PublicIdCodec>,
    pub request_executor: Option<Arc<RequestExecutor>>,
}

impl ClientApiImpl {
    fn parse_room_id(&self, room_id: &str) -> Result<RoomId, ApiError> {
        self.public_id_codec
            .decode_room_id(room_id)
            .map_err(|err| ApiError::InvalidInput(format!("Invalid room_id: {err}")))
    }

    pub(crate) fn map_room_access_error(err: synctv_core::Error) -> ApiError {
        match err {
            synctv_core::Error::Authorization(msg) => {
                ApiError::Authorization(format!("Forbidden: {msg}"))
            }
            other => ApiError::from(other),
        }
    }

    pub(super) fn map_media_lookup_error(
        err: synctv_core::Error,
        not_found_message: &'static str,
    ) -> ApiError {
        match err {
            synctv_core::Error::NotFound(_) => ApiError::NotFound(not_found_message.to_string()),
            other => ApiError::from(other),
        }
    }

    pub(super) fn map_membership_probe_error(err: synctv_core::Error) -> ApiError {
        ApiError::from(err)
    }

    pub(super) fn map_livestream_backend_error(
        error: &(dyn std::error::Error + 'static),
    ) -> ApiError {
        crate::impls::map_livestream_backend_error(error)
    }

    /// Create a new `ClientApiImpl` from individual parameters.
    ///
    /// Prefer [`ClientApiImpl::from_config`] for new code.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        user_service: Arc<UserService>,
        room_service: Arc<RoomService>,
        connection_service: Arc<dyn RealtimeConnectionService>,
        config: Arc<synctv_core::Config>,
        publish_key_service: Option<Arc<dyn synctv_core::service::StreamingPublishKeyService>>,
        jwt_service: synctv_core::service::JwtService,
        live_streaming_infrastructure: Option<
            Arc<synctv_livestream::api::LiveStreamingInfrastructure>,
        >,
        providers_manager: Option<Arc<synctv_core::service::ProvidersManager>>,
        settings_registry: Option<Arc<synctv_core::service::SettingsRegistry>>,
        public_id_codec: Arc<crate::PublicIdCodec>,
    ) -> Self {
        let jwt_validator = Arc::new(synctv_core::service::auth::JwtValidator::new(Arc::new(
            jwt_service.clone(),
        )));
        let cluster_fanout = default_cluster_fanout_service(None, config.cluster_runtime_enabled());
        let room_settings_fanout =
            default_room_settings_fanout_service(cluster_fanout.clone(), None);
        let member_fanout = default_member_fanout_service(cluster_fanout.clone());
        let membership_event_fanout = default_membership_event_fanout_service(
            cluster_fanout.clone(),
            room_service.clone(),
            user_service.clone(),
            None,
        );
        let media_fanout = default_media_fanout_service(cluster_fanout.clone(), None);
        let playlist_fanout = default_playlist_fanout_service(cluster_fanout.clone());
        let room_cache_fanout = default_room_cache_fanout_service(cluster_fanout.clone());
        let realtime_lifecycle = default_realtime_lifecycle_service(
            connection_service.clone(),
            live_streaming_infrastructure.clone(),
            cluster_fanout.clone(),
        );
        let room_lifecycle_fanout = default_room_lifecycle_fanout_service(cluster_fanout.clone());
        Self {
            user_service,
            room_service,
            connection_service,
            config,
            publish_key_service,
            jwt_service,
            live_streaming_infrastructure,
            providers_manager,
            settings_registry,
            cluster_fanout,
            room_settings_fanout,
            member_fanout,
            membership_event_fanout,
            media_fanout,
            playlist_fanout,
            room_cache_fanout,
            realtime_lifecycle,
            room_lifecycle_fanout,
            realtime_event_service: None,
            redis_runtime: None,
            rate_limiter: None,
            builtin_stun_url: None,
            credential_encryption: None,
            credential_repo: None,
            signing_key: None,
            provider_stores: None,
            jwt_validator,
            public_id_codec,
            request_executor: None,
        }
    }

    /// Create a new `ClientApiImpl` from a config struct.
    #[must_use]
    pub fn from_config(config: ClientApiConfig) -> Self {
        let jwt_validator = Arc::new(synctv_core::service::auth::JwtValidator::new(Arc::new(
            config.jwt_service.clone(),
        )));
        let cluster_fanout =
            default_cluster_fanout_service(None, config.config.cluster_runtime_enabled());
        let room_settings_fanout =
            default_room_settings_fanout_service(cluster_fanout.clone(), None);
        let member_fanout = default_member_fanout_service(cluster_fanout.clone());
        let membership_event_fanout = default_membership_event_fanout_service(
            cluster_fanout.clone(),
            config.room_service.clone(),
            config.user_service.clone(),
            None,
        );
        let media_fanout = default_media_fanout_service(cluster_fanout.clone(), None);
        let playlist_fanout = default_playlist_fanout_service(cluster_fanout.clone());
        let room_cache_fanout = default_room_cache_fanout_service(cluster_fanout.clone());
        let realtime_lifecycle = default_realtime_lifecycle_service(
            config.connection_service.clone(),
            config.live_streaming_infrastructure.clone(),
            cluster_fanout.clone(),
        );
        let room_lifecycle_fanout = default_room_lifecycle_fanout_service(cluster_fanout.clone());
        Self {
            user_service: config.user_service,
            room_service: config.room_service,
            connection_service: config.connection_service,
            config: config.config,
            publish_key_service: config.publish_key_service,
            jwt_service: config.jwt_service,
            live_streaming_infrastructure: config.live_streaming_infrastructure,
            providers_manager: config.providers_manager,
            settings_registry: config.settings_registry,
            cluster_fanout,
            room_settings_fanout,
            member_fanout,
            membership_event_fanout,
            media_fanout,
            playlist_fanout,
            room_cache_fanout,
            realtime_lifecycle,
            room_lifecycle_fanout,
            realtime_event_service: None,
            redis_runtime: None,
            rate_limiter: None,
            builtin_stun_url: None,
            credential_encryption: config.credential_encryption,
            credential_repo: None,
            signing_key: None,
            provider_stores: config.provider_stores,
            jwt_validator,
            public_id_codec: config.public_id_codec,
            request_executor: None,
        }
    }

    /// Set the cluster fanout service for cross-replica invalidation and events.
    #[must_use]
    pub fn with_cluster_fanout_service(
        mut self,
        cluster_fanout: Arc<dyn ClusterFanoutService>,
    ) -> Self {
        self.room_settings_fanout = default_room_settings_fanout_service(
            cluster_fanout.clone(),
            self.realtime_event_service.clone(),
        );
        self.member_fanout = default_member_fanout_service(cluster_fanout.clone());
        self.membership_event_fanout = default_membership_event_fanout_service(
            cluster_fanout.clone(),
            self.room_service.clone(),
            self.user_service.clone(),
            self.realtime_event_service.clone(),
        );
        self.media_fanout = default_media_fanout_service(
            cluster_fanout.clone(),
            self.realtime_event_service.clone(),
        );
        self.playlist_fanout = default_playlist_fanout_service(cluster_fanout.clone());
        self.room_cache_fanout = default_room_cache_fanout_service(cluster_fanout.clone());
        self.realtime_lifecycle = default_realtime_lifecycle_service(
            self.connection_service.clone(),
            self.live_streaming_infrastructure.clone(),
            cluster_fanout.clone(),
        );
        self.room_lifecycle_fanout = default_room_lifecycle_fanout_service(cluster_fanout.clone());
        self.cluster_fanout = cluster_fanout;
        self
    }

    #[must_use]
    pub fn with_realtime_event_service(
        mut self,
        event_service: Arc<dyn RealtimeEventService>,
    ) -> Self {
        self.membership_event_fanout = default_membership_event_fanout_service(
            self.cluster_fanout.clone(),
            self.room_service.clone(),
            self.user_service.clone(),
            Some(event_service.clone()),
        );
        self.room_settings_fanout = default_room_settings_fanout_service(
            self.cluster_fanout.clone(),
            Some(event_service.clone()),
        );
        self.media_fanout =
            default_media_fanout_service(self.cluster_fanout.clone(), Some(event_service.clone()));
        self.realtime_event_service = Some(event_service);
        self
    }

    /// Set the shared runtime abstraction for playback caching.
    #[must_use]
    pub fn with_shared_runtime(mut self, runtime: Option<Arc<dyn RedisConnectionRuntime>>) -> Self {
        self.redis_runtime = runtime;
        self
    }

    /// Set credential encryption for protecting sensitive data in `source_config`
    #[must_use]
    pub fn with_credential_encryption(
        mut self,
        enc: Option<synctv_core::service::CredentialEncryption>,
    ) -> Self {
        self.credential_encryption = enc;
        self
    }

    /// Set the credential repository for resolving stored provider credentials
    #[must_use]
    pub fn with_credential_repo(
        mut self,
        repo: Arc<synctv_core::repository::UserProviderCredentialRepository>,
    ) -> Self {
        self.credential_repo = Some(repo);
        self
    }

    /// Set the proxy signing key for generating HMAC-signed proxy URLs
    #[must_use]
    pub fn with_signing_key(mut self, key: Arc<synctv_core::service::ProxySigningKey>) -> Self {
        self.signing_key = Some(key);
        self
    }

    /// Set the per-provider store registry used for signed playback mappings.
    #[must_use]
    pub fn with_provider_stores(
        mut self,
        stores: Arc<dyn synctv_core::provider::store::ProviderStoreResolver>,
    ) -> Self {
        self.provider_stores = Some(stores);
        self
    }

    /// Resolve a fresh Redis `ConnectionManager` clone from the shared `RwLock`.
    ///
    /// Returns `None` when Redis is not configured. The returned clone is cheap
    /// (internally Arc-backed) and always points to the current Redis master,
    /// even after a Sentinel failover.
    pub async fn resolve_redis_conn(&self) -> Option<redis::aio::ConnectionManager> {
        match &self.redis_runtime {
            Some(runtime) => Some(runtime.snapshot().await),
            None => None,
        }
    }

    /// Set the rate limiter for per-endpoint rate limiting (password checks, etc.)
    #[must_use]
    pub fn with_rate_limiter<T>(mut self, rate_limiter: T) -> Self
    where
        T: synctv_core::service::RequestRateLimiterService + 'static,
    {
        self.rate_limiter = Some(Arc::new(rate_limiter));
        self
    }

    /// Set the resolved built-in STUN URL for ICE server lists.
    /// Should be called with the external address from a successfully started `StunServer`.
    #[must_use]
    pub fn with_builtin_stun_url(mut self, url: String) -> Self {
        self.builtin_stun_url = Some(url);
        self
    }

    #[must_use]
    pub fn with_request_executor(mut self, request_executor: Arc<RequestExecutor>) -> Self {
        self.request_executor = Some(request_executor);
        self
    }

    fn request_executor(&self) -> Result<&Arc<RequestExecutor>, ApiError> {
        self.request_executor.as_ref().ok_or_else(|| {
            ApiError::ServiceUnavailable("Request executor is not configured".to_string())
        })
    }

    pub fn execute_public_endpoint<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        match self.request_executor() {
            Ok(executor) => executor.execute_public(metadata, category, move || async move {
                operation().await.map_err(Into::into)
            }),
            Err(err) => Box::pin(async move { Err(err) }),
        }
    }

    pub fn execute_public_endpoint_with_context<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(RequestContext) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        match self.request_executor() {
            Ok(executor) => executor.execute_public_with_context(
                metadata,
                category,
                move |request_context| async move {
                    operation(request_context).await.map_err(Into::into)
                },
            ),
            Err(err) => Box::pin(async move { Err(err) }),
        }
    }

    pub fn execute_public_endpoint_with_control<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(synctv_core::provider::ExecutionControl) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        match self.request_executor() {
            Ok(executor) => executor.execute_public_with_control(
                metadata,
                category,
                move |request_control| async move {
                    operation(request_control).await.map_err(Into::into)
                },
            ),
            Err(err) => Box::pin(async move { Err(err) }),
        }
    }

    pub fn execute_optional_user_endpoint<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(Option<synctv_core::service::AuthenticatedToken>) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        match self.request_executor() {
            Ok(executor) => {
                executor.execute_optional_user(
                    metadata,
                    category,
                    move |authenticated| async move {
                        operation(authenticated).await.map_err(Into::into)
                    },
                )
            }
            Err(err) => Box::pin(async move { Err(err) }),
        }
    }

    pub fn execute_optional_user_endpoint_with_context<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(RequestContext, Option<synctv_core::service::AuthenticatedToken>) -> Fut
            + Send
            + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        match self.request_executor() {
            Ok(executor) => executor.execute_optional_user_with_context(
                metadata,
                category,
                move |request_context, authenticated| async move {
                    operation(request_context, authenticated)
                        .await
                        .map_err(Into::into)
                },
            ),
            Err(err) => Box::pin(async move { Err(err) }),
        }
    }

    pub fn execute_optional_user_endpoint_with_control<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(
                synctv_core::provider::ExecutionControl,
                Option<synctv_core::service::AuthenticatedToken>,
            ) -> Fut
            + Send
            + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        match self.request_executor() {
            Ok(executor) => executor.execute_optional_user_with_control(
                metadata,
                category,
                move |request_control, authenticated| async move {
                    operation(request_control, authenticated)
                        .await
                        .map_err(Into::into)
                },
            ),
            Err(err) => Box::pin(async move { Err(err) }),
        }
    }

    pub fn execute_user_endpoint<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(synctv_core::service::AuthenticatedToken) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        match self.request_executor() {
            Ok(executor) => {
                executor.execute_user(metadata, category, move |authenticated| async move {
                    operation(authenticated).await.map_err(Into::into)
                })
            }
            Err(err) => Box::pin(async move { Err(err) }),
        }
    }

    pub fn execute_user_endpoint_with_context<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(RequestContext, synctv_core::service::AuthenticatedToken) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        match self.request_executor() {
            Ok(executor) => executor.execute_user_with_context(
                metadata,
                category,
                move |request_context, authenticated| async move {
                    operation(request_context, authenticated)
                        .await
                        .map_err(Into::into)
                },
            ),
            Err(err) => Box::pin(async move { Err(err) }),
        }
    }

    pub fn execute_user_endpoint_with_control<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(
                synctv_core::provider::ExecutionControl,
                synctv_core::service::AuthenticatedToken,
            ) -> Fut
            + Send
            + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        match self.request_executor() {
            Ok(executor) => executor.execute_user_with_control(
                metadata,
                category,
                move |request_control, authenticated| async move {
                    operation(request_control, authenticated)
                        .await
                        .map_err(Into::into)
                },
            ),
            Err(err) => Box::pin(async move { Err(err) }),
        }
    }
}
