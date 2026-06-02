use futures::future::BoxFuture;
use futures::FutureExt;
use std::future::Future;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use synctv_core::provider::ExecutionControl;
use synctv_core::service::auth::{
    AuthErrorCategory, AuthenticatedToken, JwtValidator, SecurityPipeline,
};
use synctv_core::service::{RateLimitError, RequestRateLimiterService};
use synctv_core::{Config, RateLimitScopeStrategy};

use super::{ApiError, ErrorKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportProtocol {
    Http,
    Grpc,
}

impl TransportProtocol {
    #[must_use]
    pub const fn key_segment(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Grpc => "grpc",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointRateLimitCategory {
    Auth,
    Email,
    Write,
    Read,
    Media,
    Admin,
    Streaming,
    WebSocket,
}

impl EndpointRateLimitCategory {
    #[must_use]
    pub const fn key_suffix(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Email => "email",
            Self::Write => "write",
            Self::Read => "read",
            Self::Media => "media",
            Self::Admin => "admin",
            Self::Streaming => "streaming",
            Self::WebSocket => "websocket",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointRateLimitScope {
    AuthOpaqueRegistrationStart,
    AuthOpaqueRegistrationFinish,
    AuthOpaqueLoginStart,
    AuthOpaqueLoginFinish,
    AuthPasskeyRegistrationStart,
    AuthPasskeyRegistrationFinish,
    AuthPasskeyLoginStart,
    AuthPasskeyLoginFinish,
    AuthEmailLoginRequest,
    AuthEmailLoginConfirm,
    AuthMfaEmailRequest,
    AuthMfaEmailVerify,
    AuthMfaPasskeyStart,
    AuthMfaPasskeyFinish,
    AuthRefreshToken,
    AuthLogout,
    RoomCreate,
    RoomGet,
    RoomList,
    RoomJoin,
    RoomSettings,
    RoomPassword,
    RoomMembers,
    RoomPlaylist,
    RoomPlayback,
    RoomChat,
    RoomMedia,
    RoomCover,
    PlaylistCover,
    MediaCover,
    UserProfile,
    UserPreferences,
    UserAvatar,
    Notifications,
    ProviderAccount,
    ProviderBind,
    AdminSettings,
    AdminUsers,
    AdminRooms,
    AdminProviders,
    EmailDelivery,
    WebRtc,
    Ticket,
    Realtime,
}

impl EndpointRateLimitScope {
    #[must_use]
    pub const fn key_suffix(self) -> &'static str {
        match self {
            Self::AuthOpaqueRegistrationStart => "auth_opaque_registration_start",
            Self::AuthOpaqueRegistrationFinish => "auth_opaque_registration_finish",
            Self::AuthOpaqueLoginStart => "auth_opaque_login_start",
            Self::AuthOpaqueLoginFinish => "auth_opaque_login_finish",
            Self::AuthPasskeyRegistrationStart => "auth_passkey_registration_start",
            Self::AuthPasskeyRegistrationFinish => "auth_passkey_registration_finish",
            Self::AuthPasskeyLoginStart => "auth_passkey_login_start",
            Self::AuthPasskeyLoginFinish => "auth_passkey_login_finish",
            Self::AuthEmailLoginRequest => "auth_email_login_request",
            Self::AuthEmailLoginConfirm => "auth_email_login_confirm",
            Self::AuthMfaEmailRequest => "auth_mfa_email_request",
            Self::AuthMfaEmailVerify => "auth_mfa_email_verify",
            Self::AuthMfaPasskeyStart => "auth_mfa_passkey_start",
            Self::AuthMfaPasskeyFinish => "auth_mfa_passkey_finish",
            Self::AuthRefreshToken => "auth_refresh_token",
            Self::AuthLogout => "auth_logout",
            Self::RoomCreate => "room_create",
            Self::RoomGet => "room_get",
            Self::RoomList => "room_list",
            Self::RoomJoin => "room_join",
            Self::RoomSettings => "room_settings",
            Self::RoomPassword => "room_password",
            Self::RoomMembers => "room_members",
            Self::RoomPlaylist => "room_playlist",
            Self::RoomPlayback => "room_playback",
            Self::RoomChat => "room_chat",
            Self::RoomMedia => "room_media",
            Self::RoomCover => "room_cover",
            Self::PlaylistCover => "playlist_cover",
            Self::MediaCover => "media_cover",
            Self::UserProfile => "user_profile",
            Self::UserPreferences => "user_preferences",
            Self::UserAvatar => "user_avatar",
            Self::Notifications => "notifications",
            Self::ProviderAccount => "provider_account",
            Self::ProviderBind => "provider_bind",
            Self::AdminSettings => "admin_settings",
            Self::AdminUsers => "admin_users",
            Self::AdminRooms => "admin_rooms",
            Self::AdminProviders => "admin_providers",
            Self::EmailDelivery => "email_delivery",
            Self::WebRtc => "webrtc",
            Self::Ticket => "ticket",
            Self::Realtime => "realtime",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RequestMetadata {
    pub transport: TransportProtocol,
    pub authorization: Option<String>,
    pub client_ip: Option<IpAddr>,
    pub user_agent: Option<String>,
    pub endpoint_scope: Option<EndpointRateLimitScope>,
    /// Optional request budget extracted from the transport layer.
    ///
    /// This is intentionally metadata only. The impl layer may translate it
    /// into a cooperative deadline, but it is no longer enforced by wrapping
    /// the whole business future in a hard outer timeout.
    pub timeout: Option<Duration>,
}

impl RequestMetadata {
    #[must_use]
    pub const fn new(transport: TransportProtocol) -> Self {
        Self {
            transport,
            authorization: None,
            client_ip: None,
            user_agent: None,
            endpoint_scope: None,
            timeout: None,
        }
    }

    #[must_use]
    pub fn with_authorization(mut self, authorization: Option<String>) -> Self {
        self.authorization = authorization;
        self
    }

    #[must_use]
    pub const fn with_client_ip(mut self, client_ip: Option<IpAddr>) -> Self {
        self.client_ip = client_ip;
        self
    }

    #[must_use]
    pub fn with_user_agent(mut self, user_agent: Option<String>) -> Self {
        self.user_agent = user_agent;
        self
    }

    #[must_use]
    pub const fn with_endpoint_scope(
        mut self,
        endpoint_scope: Option<EndpointRateLimitScope>,
    ) -> Self {
        self.endpoint_scope = endpoint_scope;
        self
    }

    #[must_use]
    pub const fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }
}

/// Cooperative request execution context derived from transport metadata.
///
/// This carries the original metadata plus a minimal execution control handle.
/// Unlike the previous executor-wide timeout wrapper, this context does not
/// forcibly abort the business future from the outside.
#[derive(Debug, Clone)]
pub struct RequestContext {
    metadata: RequestMetadata,
    control: ExecutionControl,
}

impl RequestContext {
    #[must_use]
    pub fn from_metadata(metadata: RequestMetadata) -> Self {
        Self {
            control: ExecutionControl::from_timeout(metadata.timeout),
            metadata,
        }
    }

    #[must_use]
    pub fn from_metadata_ref(metadata: &RequestMetadata) -> Self {
        Self::from_metadata(metadata.clone())
    }

    #[must_use]
    pub const fn metadata(&self) -> &RequestMetadata {
        &self.metadata
    }

    #[must_use]
    pub const fn control(&self) -> &ExecutionControl {
        &self.control
    }

    #[must_use]
    pub fn timeout_budget(&self) -> Option<Duration> {
        self.metadata.timeout
    }

    #[must_use]
    pub const fn deadline(&self) -> Option<std::time::Instant> {
        self.control.deadline()
    }

    #[must_use]
    pub fn remaining_timeout(&self) -> Option<Duration> {
        self.control.remaining_timeout()
    }

    #[must_use]
    pub fn cancellation_token(&self) -> tokio_util::sync::CancellationToken {
        self.control.cancellation_token()
    }

    #[must_use]
    pub fn child_token(&self) -> tokio_util::sync::CancellationToken {
        self.control().child().cancellation_token()
    }

    pub fn cancel(&self) {
        self.control.cancel();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.control.is_cancelled()
    }

    pub fn check_deadline(&self) -> Result<(), ApiError> {
        self.control
            .check_deadline()
            .map_err(|err| ApiError::Timeout(err.to_string()))
    }

    pub fn check_active(&self) -> Result<(), ApiError> {
        self.control
            .check_active()
            .map_err(|err| ApiError::Timeout(err.to_string()))
    }

    #[must_use]
    pub fn child_execution_control(&self) -> ExecutionControl {
        self.control().child()
    }

    #[must_use]
    pub fn child_cancellation_control(&self) -> ExecutionControl {
        self.control().child().without_deadline()
    }
}

#[derive(Clone)]
pub struct RequestExecutor {
    config: Arc<Config>,
    jwt_validator: Arc<JwtValidator>,
    security_pipeline: Arc<SecurityPipeline>,
    rate_limiter: Arc<dyn RequestRateLimiterService>,
}

impl RequestExecutor {
    #[must_use]
    pub fn new(
        config: Arc<Config>,
        jwt_validator: Arc<JwtValidator>,
        security_pipeline: Arc<SecurityPipeline>,
        rate_limiter: Arc<dyn RequestRateLimiterService>,
    ) -> Self {
        Self {
            config,
            jwt_validator,
            security_pipeline,
            rate_limiter,
        }
    }

    pub async fn authenticate_required(
        &self,
        metadata: &RequestMetadata,
    ) -> Result<AuthenticatedToken, ApiError> {
        let authorization = metadata.authorization.as_deref().ok_or_else(|| {
            ApiError::Authentication(synctv_common::messages::AUTHENTICATION_REQUIRED.to_string())
        })?;
        self.authenticate_authorization(authorization).await
    }

    pub async fn authenticate_optional(
        &self,
        metadata: &RequestMetadata,
    ) -> Result<Option<AuthenticatedToken>, ApiError> {
        let Some(authorization) = metadata.authorization.as_deref() else {
            return Ok(None);
        };
        self.authenticate_authorization(authorization)
            .await
            .map(Some)
    }

    pub async fn authenticate_optional_if_valid(
        &self,
        metadata: &RequestMetadata,
    ) -> Result<Option<AuthenticatedToken>, ApiError> {
        let Some(authorization) = metadata.authorization.as_deref() else {
            return Ok(None);
        };

        match self.authenticate_authorization(authorization).await {
            Ok(authenticated) => Ok(Some(authenticated)),
            Err(err)
                if matches!(
                    err.classify(),
                    ErrorKind::Unauthenticated | ErrorKind::PermissionDenied
                ) =>
            {
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }

    async fn authenticate_authorization(
        &self,
        authorization: &str,
    ) -> Result<AuthenticatedToken, ApiError> {
        let claims = self
            .jwt_validator
            .validate_http(authorization)
            .map_err(|_| {
                ApiError::Authentication(
                    synctv_common::messages::INVALID_OR_EXPIRED_TOKEN.to_string(),
                )
            })?;

        self.security_pipeline
            .check(&claims)
            .await
            .map_err(map_security_pipeline_error)
    }

    pub async fn security_check_claims(
        &self,
        claims: &synctv_core::service::Claims,
    ) -> Result<AuthenticatedToken, ApiError> {
        self.security_pipeline
            .check(claims)
            .await
            .map_err(map_security_pipeline_error)
    }

    #[must_use]
    pub fn prepare_context(&self, metadata: &RequestMetadata) -> RequestContext {
        RequestContext::from_metadata_ref(metadata)
    }

    pub fn execute_public<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        self.execute_public_with_context(metadata, category, move |_| operation())
    }

    pub fn execute_public_with_context<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(RequestContext) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        let request_context = self.prepare_context(metadata);
        let request_control = request_context.child_execution_control();
        async move {
            request_context.check_active()?;
            request_control
                .run(async move {
                    self.enforce_rate_limit(
                        metadata,
                        category,
                        None,
                        Some(request_context.control()),
                    )
                    .await?;
                    request_context.check_active()?;
                    operation(request_context).await
                })
                .await
                .map_err(|err| ApiError::Timeout(err.to_string()))?
        }
        .boxed()
    }

    pub fn execute_public_with_control<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ExecutionControl) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        self.execute_public_with_context(metadata, category, move |request_context| {
            operation(request_context.child_execution_control())
        })
    }

    pub fn execute_optional_user<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(Option<AuthenticatedToken>) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        self.execute_optional_user_with_context(metadata, category, move |_, authenticated| {
            operation(authenticated)
        })
    }

    pub fn execute_optional_user_with_context<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(RequestContext, Option<AuthenticatedToken>) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        let request_context = self.prepare_context(metadata);
        let request_control = request_context.child_execution_control();
        async move {
            request_context.check_active()?;
            request_control
                .run(async move {
                    let rate_limit_identity = self.authenticate_optional_if_valid(metadata).await?;
                    self.enforce_rate_limit(
                        metadata,
                        category,
                        rate_limit_identity.as_ref(),
                        Some(request_context.control()),
                    )
                    .await?;
                    let authenticated = match rate_limit_identity {
                        Some(authenticated) => Some(authenticated),
                        None => self.authenticate_optional(metadata).await?,
                    };
                    request_context.check_active()?;
                    operation(request_context, authenticated).await
                })
                .await
                .map_err(|err| ApiError::Timeout(err.to_string()))?
        }
        .boxed()
    }

    pub fn execute_optional_user_with_control<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ExecutionControl, Option<AuthenticatedToken>) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        self.execute_optional_user_with_context(
            metadata,
            category,
            move |request_context, authenticated| {
                operation(request_context.child_execution_control(), authenticated)
            },
        )
    }

    pub fn execute_optional_user_if_valid<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(Option<AuthenticatedToken>) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        self.execute_optional_user_if_valid_with_context(
            metadata,
            category,
            move |_, authenticated| operation(authenticated),
        )
    }

    pub fn execute_optional_user_if_valid_with_context<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(RequestContext, Option<AuthenticatedToken>) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        let request_context = self.prepare_context(metadata);
        let request_control = request_context.child_execution_control();
        async move {
            request_context.check_active()?;
            request_control
                .run(async move {
                    let authenticated = self.authenticate_optional_if_valid(metadata).await?;
                    self.enforce_rate_limit(
                        metadata,
                        category,
                        authenticated.as_ref(),
                        Some(request_context.control()),
                    )
                    .await?;
                    request_context.check_active()?;
                    operation(request_context, authenticated).await
                })
                .await
                .map_err(|err| ApiError::Timeout(err.to_string()))?
        }
        .boxed()
    }

    pub fn execute_optional_user_if_valid_with_control<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ExecutionControl, Option<AuthenticatedToken>) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        self.execute_optional_user_if_valid_with_context(
            metadata,
            category,
            move |request_context, authenticated| {
                operation(request_context.child_execution_control(), authenticated)
            },
        )
    }

    pub fn execute_user<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(AuthenticatedToken) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        self.execute_user_with_context(metadata, category, move |_, authenticated| {
            operation(authenticated)
        })
    }

    pub fn execute_user_with_context<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(RequestContext, AuthenticatedToken) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        let request_context = self.prepare_context(metadata);
        let request_control = request_context.child_execution_control();
        async move {
            request_context.check_active()?;
            request_control
                .run(async move {
                    let rate_limit_identity = self.authenticate_optional_if_valid(metadata).await?;
                    self.enforce_rate_limit(
                        metadata,
                        category,
                        rate_limit_identity.as_ref(),
                        Some(request_context.control()),
                    )
                    .await?;
                    let authenticated = match rate_limit_identity {
                        Some(authenticated) => authenticated,
                        None => self.authenticate_required(metadata).await?,
                    };
                    request_context.check_active()?;
                    operation(request_context, authenticated).await
                })
                .await
                .map_err(|err| ApiError::Timeout(err.to_string()))?
        }
        .boxed()
    }

    pub fn execute_user_with_control<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ExecutionControl, AuthenticatedToken) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        self.execute_user_with_context(metadata, category, move |request_context, authenticated| {
            operation(request_context.child_execution_control(), authenticated)
        })
    }

    pub fn execute_authenticated_token_with_control<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        token: &'a str,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ExecutionControl, AuthenticatedToken) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        let request_context = self.prepare_context(metadata);
        let request_control = request_context.child_execution_control();
        async move {
            request_context.check_active()?;
            request_control
                .run(async move {
                    let authenticated = match self.jwt_validator.validate_token(token) {
                        Ok(claims) => self.security_check_claims(&claims).await.ok(),
                        Err(_) => None,
                    };
                    self.enforce_rate_limit(
                        metadata,
                        category,
                        authenticated.as_ref(),
                        Some(request_context.control()),
                    )
                    .await?;
                    let authenticated = if let Some(authenticated) = authenticated {
                        authenticated
                    } else {
                        let claims = self.jwt_validator.validate_token(token).map_err(|_| {
                            ApiError::Authentication(
                                synctv_common::messages::INVALID_OR_EXPIRED_TOKEN.to_string(),
                            )
                        })?;
                        self.security_check_claims(&claims).await?
                    };
                    request_context.check_active()?;
                    operation(request_context.child_execution_control(), authenticated).await
                })
                .await
                .map_err(|err| ApiError::Timeout(err.to_string()))?
        }
        .boxed()
    }

    async fn enforce_rate_limit(
        &self,
        metadata: &RequestMetadata,
        category: EndpointRateLimitCategory,
        authenticated: Option<&AuthenticatedToken>,
        control: Option<&ExecutionControl>,
    ) -> Result<(), ApiError> {
        let rate_limit =
            rate_limit_policy_for_config(&self.config, category, metadata.endpoint_scope);
        let subject_key = authenticated.map_or_else(
            || {
                metadata
                    .client_ip
                    .map_or_else(|| "anon:unknown".to_string(), |ip| format!("anon:{ip}"))
            },
            |authenticated| format!("user:{}", authenticated.user_id),
        );

        if rate_limit.strategy.enabled() {
            let key = rate_limit_key(category, metadata.endpoint_scope, &subject_key);
            self.rate_limiter
                .check_rate_limit_with_control(
                    &key,
                    rate_limit.budget.max_requests,
                    rate_limit.budget.window_seconds,
                    control,
                )
                .await
                .map_err(map_rate_limit_error)?;
        }

        Ok(())
    }
}

fn rate_limit_key(
    category: EndpointRateLimitCategory,
    endpoint_scope: Option<EndpointRateLimitScope>,
    subject_key: &str,
) -> String {
    let scope =
        endpoint_scope.map_or_else(|| category.key_suffix(), EndpointRateLimitScope::key_suffix);
    format!("ratelimit:{scope}:{subject_key}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RateLimitBudget {
    max_requests: u32,
    window_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RateLimitPolicy {
    budget: RateLimitBudget,
    strategy: RateLimitScopeStrategy,
}

fn rate_limit_budget_for_config(
    config: &Config,
    category: EndpointRateLimitCategory,
) -> RateLimitBudget {
    let config = &config.request_rate_limits;
    let (max_requests, window_seconds) = match category {
        EndpointRateLimitCategory::Auth | EndpointRateLimitCategory::Email => {
            (config.auth_max_requests, config.auth_window_seconds)
        }
        EndpointRateLimitCategory::Write => {
            (config.write_max_requests, config.write_window_seconds)
        }
        EndpointRateLimitCategory::Read => (config.read_max_requests, config.read_window_seconds),
        EndpointRateLimitCategory::Media => {
            (config.media_max_requests, config.media_window_seconds)
        }
        EndpointRateLimitCategory::Admin => {
            (config.admin_max_requests, config.admin_window_seconds)
        }
        EndpointRateLimitCategory::Streaming => (
            config.streaming_max_requests,
            config.streaming_window_seconds,
        ),
        EndpointRateLimitCategory::WebSocket => (
            config.websocket_max_requests,
            config.websocket_window_seconds,
        ),
    };
    RateLimitBudget {
        max_requests,
        window_seconds,
    }
}

fn rate_limit_policy_for_config(
    config: &Config,
    category: EndpointRateLimitCategory,
    scope: Option<EndpointRateLimitScope>,
) -> RateLimitPolicy {
    let category_budget = rate_limit_budget_for_config(config, category);
    let Some(scope) = scope else {
        return RateLimitPolicy {
            budget: category_budget,
            strategy: RateLimitScopeStrategy::FixedWindow,
        };
    };
    let scope_rule = config.request_rate_limits.scopes.get(scope.key_suffix());
    let Some(scope_rule) = scope_rule else {
        return RateLimitPolicy {
            budget: category_budget,
            strategy: RateLimitScopeStrategy::FixedWindow,
        };
    };
    RateLimitPolicy {
        budget: RateLimitBudget {
            max_requests: scope_rule
                .max_requests
                .unwrap_or(category_budget.max_requests),
            window_seconds: scope_rule
                .window_seconds
                .unwrap_or(category_budget.window_seconds),
        },
        strategy: scope_rule.strategy,
    }
}

#[cfg(test)]
fn rate_limit_budget_tuple_for_config(
    config: &Config,
    category: EndpointRateLimitCategory,
) -> (u32, u64) {
    let budget = rate_limit_budget_for_config(config, category);
    (budget.max_requests, budget.window_seconds)
}

fn map_security_pipeline_error(err: synctv_core::Error) -> ApiError {
    match SecurityPipeline::classify_auth_error(&err) {
        AuthErrorCategory::Authentication => {
            ApiError::Authentication("Authentication failed".to_string())
        }
        AuthErrorCategory::Authorization => ApiError::from(err),
        AuthErrorCategory::Unavailable => ApiError::ServiceUnavailable(
            "Authentication service temporarily unavailable".to_string(),
        ),
        AuthErrorCategory::Internal => ApiError::Internal(err.to_string()),
    }
}

fn map_rate_limit_error(err: RateLimitError) -> ApiError {
    match err {
        RateLimitError::RateLimitExceeded {
            retry_after_seconds,
        } => ApiError::RateLimitedWithRetry {
            message: format!("Rate limit exceeded. Try again in {retry_after_seconds}s"),
            retry_after_seconds,
        },
        RateLimitError::BackendUnavailable(message) => ApiError::ServiceUnavailable(message),
        RateLimitError::Control(error) => ApiError::Timeout(error.to_string()),
        RateLimitError::RedisError(error) => {
            ApiError::ServiceUnavailable(format!("Rate limit service unavailable: {error}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_core::RateLimitScopeRule;

    #[test]
    fn request_context_preserves_budget_metadata() {
        let metadata = RequestMetadata::new(TransportProtocol::Http)
            .with_client_ip(Some("127.0.0.1".parse().expect("ip")))
            .with_timeout(Some(Duration::from_secs(5)));

        let context = RequestContext::from_metadata(metadata.clone());

        assert_eq!(context.metadata().transport, TransportProtocol::Http);
        assert_eq!(context.metadata().client_ip, metadata.client_ip);
        assert_eq!(context.metadata().endpoint_scope, metadata.endpoint_scope);
        assert_eq!(context.timeout_budget(), Some(Duration::from_secs(5)));
        assert!(context.deadline().is_some());
        assert!(context.remaining_timeout().is_some());
    }

    #[test]
    fn request_context_reports_expired_deadline_at_checkpoints() {
        let metadata =
            RequestMetadata::new(TransportProtocol::Grpc).with_timeout(Some(Duration::ZERO));
        let context = RequestContext::from_metadata(metadata);

        let err = context
            .check_active()
            .expect_err("zero budget should be expired immediately");
        assert!(matches!(err, ApiError::Timeout(message) if message == "Request timed out"));
    }

    #[test]
    fn request_context_reports_cancellation_at_checkpoints() {
        let context = RequestContext::from_metadata(RequestMetadata::new(TransportProtocol::Http));
        context.cancel();

        let err = context
            .check_active()
            .expect_err("cancelled context must fail at the next checkpoint");
        assert!(matches!(err, ApiError::Timeout(message) if message == "Request cancelled"));
    }

    #[test]
    fn request_websocket_rate_limit_uses_shared_request_budget() {
        let mut config = Config::default();
        config.request_rate_limits.write_max_requests = 30;
        config.request_rate_limits.write_window_seconds = 31;
        config.request_rate_limits.read_max_requests = 100;
        config.request_rate_limits.read_window_seconds = 101;
        config.request_rate_limits.streaming_max_requests = 200;
        config.request_rate_limits.streaming_window_seconds = 201;
        config.request_rate_limits.websocket_max_requests = 7;
        config.request_rate_limits.websocket_window_seconds = 8;

        assert_eq!(
            rate_limit_budget_tuple_for_config(&config, EndpointRateLimitCategory::WebSocket),
            (7, 8)
        );
        assert_eq!(
            rate_limit_budget_tuple_for_config(&config, EndpointRateLimitCategory::Streaming),
            (200, 201)
        );
        assert_eq!(
            rate_limit_budget_tuple_for_config(&config, EndpointRateLimitCategory::Write),
            (30, 31)
        );
    }

    #[test]
    fn rate_limit_keys_use_business_scope_only() {
        let fallback_key = rate_limit_key(EndpointRateLimitCategory::Read, None, "user:42");
        let scoped_key = rate_limit_key(
            EndpointRateLimitCategory::Read,
            Some(EndpointRateLimitScope::RoomMembers),
            "user:42",
        );

        assert_eq!(fallback_key, "ratelimit:read:user:42");
        assert_eq!(scoped_key, "ratelimit:room_members:user:42");
    }

    #[test]
    fn scope_rate_limit_policy_uses_fixed_window_by_default() {
        let mut config = Config::default();
        config.request_rate_limits.read_max_requests = 600;
        config.request_rate_limits.read_window_seconds = 60;

        let policy = rate_limit_policy_for_config(
            &config,
            EndpointRateLimitCategory::Read,
            Some(EndpointRateLimitScope::RoomMembers),
        );

        assert_eq!(policy.strategy, RateLimitScopeStrategy::FixedWindow);
        assert_eq!(
            policy.budget,
            RateLimitBudget {
                max_requests: 600,
                window_seconds: 60,
            }
        );
    }

    #[test]
    fn scope_rate_limit_policy_uses_scope_override() {
        let mut config = Config::default();
        config.request_rate_limits.scopes.insert(
            EndpointRateLimitScope::RoomMembers.key_suffix().to_string(),
            RateLimitScopeRule {
                max_requests: Some(90),
                window_seconds: Some(15),
                strategy: RateLimitScopeStrategy::FixedWindow,
            },
        );

        let policy = rate_limit_policy_for_config(
            &config,
            EndpointRateLimitCategory::Read,
            Some(EndpointRateLimitScope::RoomMembers),
        );

        assert_eq!(policy.strategy, RateLimitScopeStrategy::FixedWindow);
        assert_eq!(
            policy.budget,
            RateLimitBudget {
                max_requests: 90,
                window_seconds: 15,
            }
        );
    }

    #[test]
    fn scope_rate_limit_policy_falls_back_missing_rule_fields() {
        let mut config = Config::default();
        config.request_rate_limits.read_max_requests = 500;
        config.request_rate_limits.read_window_seconds = 45;
        config.request_rate_limits.scopes.insert(
            EndpointRateLimitScope::RoomMembers.key_suffix().to_string(),
            RateLimitScopeRule {
                max_requests: Some(80),
                window_seconds: None,
                strategy: RateLimitScopeStrategy::FixedWindow,
            },
        );

        let policy = rate_limit_policy_for_config(
            &config,
            EndpointRateLimitCategory::Read,
            Some(EndpointRateLimitScope::RoomMembers),
        );

        assert_eq!(
            policy.budget,
            RateLimitBudget {
                max_requests: 80,
                window_seconds: 45,
            }
        );
    }
}
