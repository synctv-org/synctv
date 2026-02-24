// Re-export proto types from synctv-proto
pub use synctv_proto::{client, admin};

// Re-export cluster proto from synctv-cluster (internal)
pub use synctv_cluster::grpc::synctv::cluster;

pub mod admin_service;
pub mod blacklist_layer;
pub mod client_service;
pub mod interceptors;
pub mod notification_service;
pub mod oauth2_service;
pub mod rate_limit_layer;

// Provider gRPC services (local implementations)
// Provider-specific gRPC services are registered from provider instances
pub mod providers;

pub use admin_service::AdminServiceImpl;
pub use client_service::{ClientServiceImpl, ClientServiceConfig};
pub use notification_service::NotificationServiceImpl;
pub use interceptors::{
    AuthInterceptor, ClusterAuthInterceptor, LoggingInterceptor,
};

/// Trait to apply gRPC message size limits to tonic service servers.
///
/// This trait provides a unified interface for setting max decoding/encoding
/// message sizes on tonic-generated service servers, protecting against OOM
/// attacks from oversized messages.
pub trait GrpcServiceExt: Sized {
    /// Apply message size limits (both decoding and encoding) to the service.
    /// Returns the service with limits configured.
    fn with_message_size_limit(self, max_size: usize) -> Self {
        self.with_decoding_limit(max_size)
            .with_encoding_limit(max_size)
    }

    /// Apply maximum decoding (incoming) message size limit.
    fn with_decoding_limit(self, limit: usize) -> Self;

    /// Apply maximum encoding (outgoing) message size limit.
    fn with_encoding_limit(self, limit: usize) -> Self;
}

// Implement GrpcServiceExt for all tonic-generated server types that support
// max_decoding_message_size and max_encoding_message_size methods.
// These implementations use the generated methods directly.

macro_rules! impl_grpc_service_ext {
    (<$T:ident> $server_type:ty) => {
        impl<$T> GrpcServiceExt for $server_type {
            fn with_decoding_limit(self, limit: usize) -> Self {
                self.max_decoding_message_size(limit)
            }
            fn with_encoding_limit(self, limit: usize) -> Self {
                self.max_encoding_message_size(limit)
            }
        }
    };
}

// Apply the macro to all gRPC service server types used in this crate
impl_grpc_service_ext!(<T> crate::proto::client::auth_service_server::AuthServiceServer<T>);
impl_grpc_service_ext!(<T> crate::proto::client::user_service_server::UserServiceServer<T>);
impl_grpc_service_ext!(<T> crate::proto::client::room_service_server::RoomServiceServer<T>);
impl_grpc_service_ext!(<T> crate::proto::client::media_service_server::MediaServiceServer<T>);
impl_grpc_service_ext!(<T> crate::proto::client::public_service_server::PublicServiceServer<T>);
impl_grpc_service_ext!(<T> crate::proto::client::email_service_server::EmailServiceServer<T>);
impl_grpc_service_ext!(<T> crate::proto::client::notification_service_server::NotificationServiceServer<T>);
impl_grpc_service_ext!(<T> crate::proto::client::o_auth2_service_server::OAuth2ServiceServer<T>);
impl_grpc_service_ext!(<T> crate::proto::admin_service_server::AdminServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::providers::alist::alist_provider_service_server::AlistProviderServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::providers::bilibili::bilibili_provider_service_server::BilibiliProviderServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::providers::emby::emby_provider_service_server::EmbyProviderServiceServer<T>);

/// Log an internal error and return a generic gRPC status to avoid leaking details.
///
/// Shared across all gRPC service implementations.
pub(crate) fn internal_err(context: &str, err: impl std::fmt::Display) -> tonic::Status {
    tracing::error!("{context}: {err}");
    tonic::Status::internal(context)
}

/// Map a typed [`ApiError`](crate::impls::ApiError) to a gRPC `Status`.
///
/// Shared across all gRPC service implementations to avoid duplicating the
/// identical match block in every service file.
///
/// For internal errors, the details are logged server-side and a generic
/// message is returned to the client to avoid leaking sensitive information.
pub(crate) fn map_api_error(err: crate::impls::ApiError) -> tonic::Status {
    use crate::impls::ErrorKind;
    let msg = err.to_string();
    match err.classify() {
        ErrorKind::NotFound => tonic::Status::not_found(msg),
        ErrorKind::Unauthenticated => tonic::Status::unauthenticated(msg),
        ErrorKind::PermissionDenied => tonic::Status::permission_denied(msg),
        ErrorKind::AlreadyExists => tonic::Status::already_exists(msg),
        ErrorKind::InvalidArgument => tonic::Status::invalid_argument(msg),
        ErrorKind::Internal => {
            tracing::error!("API internal error: {msg}");
            tonic::Status::internal("Internal error")
        }
    }
}

/// Map a `ProviderError` to an appropriate gRPC status code.
///
/// Uses typed matching on the `ProviderError` enum instead of
/// keyword-based string heuristics.
pub(crate) fn map_provider_error(err: synctv_core::provider::ProviderError) -> tonic::Status {
    use synctv_core::provider::ProviderError;
    let msg = err.to_string();
    match err {
        ProviderError::NetworkError(_) | ProviderError::ApiError(_) => {
            tonic::Status::unavailable(msg)
        }
        ProviderError::UpstreamHttp { status, .. } => {
            if (400..500).contains(&status) {
                tonic::Status::failed_precondition(msg)
            } else {
                tonic::Status::unavailable(msg)
            }
        }
        ProviderError::ParseError(_) | ProviderError::InvalidConfig(_)
        | ProviderError::InvalidUrl(_) | ProviderError::MissingField(_)
        | ProviderError::InvalidCredentialType | ProviderError::UnsupportedFormat(_) => {
            tonic::Status::invalid_argument(msg)
        }
        ProviderError::NotFound | ProviderError::InstanceNotFound(_) | ProviderError::MissingInstance => {
            tonic::Status::not_found(msg)
        }
        ProviderError::AuthRequired | ProviderError::CredentialRequired => {
            tonic::Status::unauthenticated(msg)
        }
        ProviderError::RouteRegistrationFailed(_) | ProviderError::IoError(_) | ProviderError::JsonError(_) => {
            tracing::error!("Provider internal error: {msg}");
            tonic::Status::internal("Internal error")
        }
    }
}

// Use synctv_proto for all server traits and message types (single source of truth)
use crate::proto::admin_service_server::AdminServiceServer;
use crate::proto::client::{
    auth_service_server::AuthServiceServer, email_service_server::EmailServiceServer,
    media_service_server::MediaServiceServer,
    notification_service_server::NotificationServiceServer,
    public_service_server::PublicServiceServer, room_service_server::RoomServiceServer,
    user_service_server::UserServiceServer,
};
use tonic::transport::Server;

use std::sync::Arc;
use synctv_cluster::sync::{ClusterManager, ConnectionManager, PublishRequest};
use synctv_core::provider::{AlistProvider, BilibiliProvider, EmbyProvider};
use synctv_core::service::auth::JwtService;
use synctv_core::service::{
    ContentFilter, EmailService, EmailTokenService, RemoteProviderManager, ProvidersManager,
    RateLimitConfig, RateLimiter, RoomService as CoreRoomService, SettingsRegistry,
    SettingsService, UserService as CoreUserService,
};
use synctv_core::Config;

/// Configuration for the gRPC server
#[derive(Clone)]
pub struct GrpcServerConfig<'a> {
    pub config: &'a Config,
    pub jwt_service: JwtService,
    pub user_service: Arc<CoreUserService>,
    pub room_service: Arc<CoreRoomService>,
    pub cluster_manager: Option<Arc<ClusterManager>>,
    pub redis_publish_tx: Option<tokio::sync::mpsc::Sender<PublishRequest>>,
    pub rate_limiter: RateLimiter,
    pub rate_limit_config: RateLimitConfig,
    pub content_filter: ContentFilter,
    pub connection_manager: ConnectionManager,
    pub providers_manager: Option<Arc<ProvidersManager>>,
    pub provider_instance_manager: Arc<RemoteProviderManager>,
    pub user_provider_credential_repository: Arc<synctv_core::repository::UserProviderCredentialRepository>,
    pub settings_service: Arc<SettingsService>,
    pub settings_registry: Option<Arc<SettingsRegistry>>,
    pub email_service: Option<Arc<EmailService>>,
    pub email_token_service: Option<Arc<EmailTokenService>>,
    pub live_streaming_infrastructure: Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    pub publish_key_service: Option<Arc<synctv_core::service::PublishKeyService>>,
    pub notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    pub oauth2_service: Option<Arc<synctv_core::service::OAuth2Service>>,
    pub audit_service: Arc<synctv_core::service::AuditService>,
    pub node_registry: Option<Arc<synctv_cluster::discovery::NodeRegistry>>,
    /// Pre-built Redis client (from the single `init_redis()` call).
    /// Used by the fallback `NodeRegistry` creation to avoid duplicate `redis::Client::open()`.
    /// `None` in standalone mode without Redis.
    pub redis_client: Option<redis::Client>,
    /// Shared Redis connection for playback caching (Sentinel-failover safe)
    pub redis_conn: Option<crate::SharedRedisConn>,
    pub shutdown_rx: Option<tokio::sync::watch::Receiver<bool>>,
    /// Resolved built-in STUN URL (e.g. "stun:203.0.113.1:3478") from a successfully started
    /// STUN server. When `None`, the built-in STUN entry is omitted from ICE server lists.
    pub builtin_stun_url: Option<String>,
}

/// Build and start the gRPC server
pub async fn serve(grpc_config: GrpcServerConfig<'_>) -> anyhow::Result<()> {
    let GrpcServerConfig {
        config,
        jwt_service,
        user_service,
        room_service,
        cluster_manager,
        redis_publish_tx,
        rate_limiter,
        rate_limit_config,
        content_filter,
        connection_manager,
        providers_manager,
        provider_instance_manager,
        user_provider_credential_repository,
        settings_service,
        settings_registry,
        email_service,
        email_token_service,
        live_streaming_infrastructure,
        publish_key_service,
        notification_service,
        oauth2_service,
        audit_service,
        node_registry,
        redis_client,
        redis_conn,
        shutdown_rx,
        builtin_stun_url,
    } = grpc_config;
    let addr = config.grpc_address().parse()?;

    tracing::info!("Starting gRPC server on {}", addr);

    // Clone services for all uses before unwrapping
    let user_service_for_client = user_service.clone();
    let user_service_for_admin = user_service.clone();
    let user_service_for_provider = user_service.clone();

    let room_service_for_client = room_service.clone();
    let room_service_for_provider = room_service.clone();

    let jwt_service_for_provider = jwt_service.clone();

    // Create service instances
    let user_service_clone =
        Arc::try_unwrap(user_service_for_client).unwrap_or_else(|arc| (*arc).clone());
    let room_service_clone =
        Arc::try_unwrap(room_service_for_client).unwrap_or_else(|arc| (*arc).clone());

    // Extract node_id reference before moving cluster_manager
    let cluster_node_id = cluster_manager
        .as_ref().map_or_else(|| "single-node".to_string(), |cm| cm.node_id().to_string());

    // Clone connection_manager for later use
    let connection_manager_for_provider = connection_manager.clone();

    let email_service_for_admin = email_service.clone();
    let providers_manager_for_client = providers_manager.clone();
    let rate_limiter_for_provider = rate_limiter.clone();

    // Build the shared ClientApiImpl for gRPC handlers
    let client_api = Arc::new(crate::impls::ClientApiImpl::new(
        user_service.clone(),
        room_service.clone(),
        Arc::new(connection_manager.clone()),
        Arc::new(config.clone()),
        publish_key_service,
        jwt_service.clone(),
        live_streaming_infrastructure.clone(),
        providers_manager_for_client.clone(),
        settings_registry.clone(),
    ).with_redis_publish_tx(redis_publish_tx.clone())
     .with_redis_conn(redis_conn.clone())
     .with_rate_limiter(rate_limiter.clone()));

    // Wire in the resolved STUN URL if the built-in STUN server started successfully
    let client_api = if let Some(stun_url) = builtin_stun_url {
        let inner = Arc::try_unwrap(client_api).unwrap_or_else(|arc| (*arc).clone());
        Arc::new(inner.with_builtin_stun_url(stun_url))
    } else {
        client_api
    };

    let rate_limiter_for_layer = rate_limiter.clone();
    let cluster_manager_for_rt = cluster_manager.clone();
    let client_service = ClientServiceImpl::from_config(ClientServiceConfig {
        user_service: user_service_clone,
        room_service: room_service_clone,
        cluster_manager,
        rate_limiter,
        rate_limit_config,
        content_filter,
        connection_manager,
        email_service,
        email_token_service,
        settings_registry: settings_registry.clone(),
        providers_manager: providers_manager_for_client,
        config: Arc::new(config.clone()),
        client_api: client_api.clone(),
    });

    // Build the shared AdminApiImpl for gRPC handlers (same impls layer used by HTTP)
    // AdminApiImpl requires EmailService; if not configured, create with None config
    // so send_test_email fails gracefully.
    let email_svc_for_admin_api = email_service_for_admin
        .unwrap_or_else(|| Arc::new(EmailService::new(None).expect("EmailService::new(None) should not fail")));

    let admin_api = Arc::new(crate::impls::AdminApiImpl::new(
        room_service.clone(),
        user_service_for_admin.clone(),
        settings_service.clone(),
        settings_registry.clone(),
        email_svc_for_admin_api,
        Arc::new(connection_manager_for_provider.clone()),
        provider_instance_manager,
        live_streaming_infrastructure,
        redis_publish_tx.clone(),
        audit_service.clone(),
    ));

    let admin_service = AdminServiceImpl::new(
        user_service_for_admin,
        admin_api,
    );

    // Create auth interceptor for authenticated services (clone jwt_service for blacklist layer)
    let auth_interceptor = AuthInterceptor::new(jwt_service.clone());

    // Create JwtValidator for rate limiting layer (needs to verify JWT to extract user_id)
    let jwt_validator_for_rate_limit = synctv_core::service::auth::JwtValidator::new(
        std::sync::Arc::new(jwt_service.clone()),
    );

    // Create server builder with the security checking tower layer.
    // This layer extracts the raw JWT bearer token from the HTTP Authorization
    // header and performs async security checks via the shared SecurityPipeline:
    // 1. JWT verification (validate signature, expiration, access token type)
    // 2. Password invalidation check (tokens issued before password change)
    // 3. User status check (banned/pending/deleted)
    // It runs before tonic routes and interceptors, so public endpoints (no Authorization header)
    // pass through without security checks.
    let security_pipeline = synctv_core::service::SecurityPipeline::new(
        user_service.clone(),
    ).with_token_blacklist(
        user_service.token_blacklist_store(),
        user_service.key_builder().clone(),
    );
    let blacklist_layer = blacklist_layer::BlacklistCheckLayer::new(
        jwt_service,
        security_pipeline,
    );
    // Distributed rate limiting layer: uses Redis when available (shared across
    // replicas), falls back to in-memory governor when Redis is unavailable.
    // Determines tier per-request from the gRPC service path.
    // Uses verified user_id from JWT claims as rate limit key to ensure all tokens
    // belonging to the same user share a single quota.
    let distributed_rate_limit_layer = rate_limit_layer::GrpcRateLimitLayer::new(
        rate_limiter_for_layer,
        Arc::new(config.clone()),
        jwt_validator_for_rate_limit,
    );
    let mut server_builder = Server::builder()
        .layer(blacklist_layer)
        .layer(distributed_rate_limit_layer);

    // Get the configured max message size (prevents OOM from oversized messages)
    let max_message_size = config.server.grpc_max_message_size_bytes;
    tracing::info!(
        max_message_size_bytes = max_message_size,
        max_message_size_mb = max_message_size / (1024 * 1024),
        "gRPC message size limit configured"
    );

    // Clone interceptors for different services
    let user_interceptor = auth_interceptor.clone();
    let admin_interceptor = auth_interceptor.clone();
    let room_interceptor1 = auth_interceptor.clone();
    let room_interceptor2 = auth_interceptor.clone();

    // Rate limiting is handled by the distributed_rate_limit_layer applied at the
    // server level (above). Per-service interceptors only handle auth concerns.

    // Build router - register all client services with auth interceptors
    // All services have message size limits applied to prevent OOM attacks
    let client_service_clone1 = client_service.clone();
    let client_service_clone2 = client_service.clone();
    let client_service_clone3 = client_service.clone();
    let client_service_clone4 = client_service.clone();
    let client_service_clone5 = client_service.clone();

    let mut router = server_builder
        // AuthService (public: register, login, refresh_token)
        .add_service(AuthServiceServer::new(client_service).with_message_size_limit(max_message_size))
        // UserService - JWT authentication (inject UserContext)
        // Use tonic::codegen::InterceptedService::new to preserve message size limits set on the service
        .add_service(tonic::codegen::InterceptedService::new(
            UserServiceServer::new(client_service_clone1).with_message_size_limit(max_message_size),
            move |req| user_interceptor.inject_user(req),
        ))
        // RoomService - JWT + room_id (inject RoomContext)
        .add_service(tonic::codegen::InterceptedService::new(
            RoomServiceServer::new(client_service_clone2).with_message_size_limit(max_message_size),
            move |req| room_interceptor1.inject_room(req),
        ))
        // MediaService - JWT + room_id (inject RoomContext)
        .add_service(tonic::codegen::InterceptedService::new(
            MediaServiceServer::new(client_service_clone3).with_message_size_limit(max_message_size),
            move |req| room_interceptor2.inject_room(req),
        ))
        // PublicService (public room discovery)
        .add_service(PublicServiceServer::new(client_service_clone4).with_message_size_limit(max_message_size))
        // EmailService (send codes, confirm with token)
        .add_service(EmailServiceServer::new(client_service_clone5).with_message_size_limit(max_message_size))
        // AdminService - JWT authentication (inject UserContext)
        .add_service(tonic::codegen::InterceptedService::new(
            AdminServiceServer::new(admin_service).with_message_size_limit(max_message_size),
            move |req| admin_interceptor.inject_user(req),
        ));

    // Register NotificationService if notification_service is configured
    if let Some(notif_svc) = notification_service {
        let notification_interceptor = auth_interceptor.clone();
        let notification_api = Arc::new(crate::impls::NotificationApiImpl::new(notif_svc.clone()));
        let notif_impl = NotificationServiceImpl::new(notification_api);
        router = router.add_service(tonic::codegen::InterceptedService::new(
            NotificationServiceServer::new(notif_impl).with_message_size_limit(max_message_size),
            move |req| notification_interceptor.inject_user(req),
        ));
        tracing::info!("NotificationService gRPC registered");

        // RT-1: Spawn a background task that bridges notification creation events
        // to the cluster event system, enabling real-time WebSocket push for
        // persistent user notifications. Without this, clients must poll.
        // The task listens for the server shutdown signal so it does not leak
        // when the gRPC server stops.
        if let Some(ref cm) = cluster_manager_for_rt {
            let cm = Arc::clone(cm);
            let mut notification_rx = notif_svc.subscribe_events();
            // Clone the optional shutdown watch receiver so the bridge task
            // can stop cleanly when the server receives a shutdown signal.
            // When no shutdown receiver is configured (e.g., test environments),
            // the bridge runs until the notification channel closes.
            let mut bridge_shutdown_rx: Option<tokio::sync::watch::Receiver<bool>> =
                shutdown_rx.clone();
            tokio::spawn(async move {
                loop {
                    // Build a future that resolves when the shutdown signal fires.
                    // When no receiver is available, use a pending future so the
                    // select falls through to the notification arm.
                    let shutdown_future: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
                        match bridge_shutdown_rx.as_mut() {
                            Some(rx) => Box::pin(async move { let _ = rx.changed().await; }),
                            None => Box::pin(std::future::pending()),
                        };

                    tokio::select! {
                        // Honour the server-wide shutdown signal.
                        () = shutdown_future => {
                            tracing::info!("Notification-to-cluster bridge task stopping (shutdown signal)");
                            break;
                        }
                        result = notification_rx.recv() => {
                            match result {
                                Ok(event) => {
                                    let cluster_event = synctv_cluster::sync::ClusterEvent::UserNotification {
                                        event_id: nanoid::nanoid!(16),
                                        user_id: event.user_id,
                                        notification_id: event.notification.id.to_string(),
                                        title: event.notification.title,
                                        content: event.notification.content,
                                        notification_type: event.notification.notification_type.to_string(),
                                        timestamp: chrono::Utc::now(),
                                    };
                                    cm.broadcast(cluster_event);
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    tracing::warn!(
                                        lagged = n,
                                        "Notification-to-cluster bridge lagged, some notifications may not have been pushed in real time"
                                    );
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                    tracing::info!("Notification event channel closed, stopping bridge task");
                                    break;
                                }
                            }
                        }
                    }
                }
            });
            tracing::info!("Notification-to-cluster bridge task spawned for real-time WebSocket push");
        }
    }

    // Register OAuth2Service if oauth2_service is configured.
    // Uses a single service with NO global auth interceptor. Public endpoints
    // (GetAuthorizationUrl, ExchangeAuthorizationCode, ListAvailableProviders)
    // require no authentication. Private endpoints (GetAuthorizationUrlForBind,
    // UnlinkProvider, GetLinkedProviders) perform inline JWT validation using
    // the auth interceptor passed to the service constructor.
    if let Some(oauth2_svc) = oauth2_service {
        use synctv_proto::client::o_auth2_service_server::OAuth2ServiceServer;
        let oauth2_auth_interceptor = auth_interceptor.clone();
        let oauth2_api = Arc::new(crate::impls::OAuth2ApiImpl::new(
            oauth2_svc,
            user_service.clone(),
        ));
        let oauth2_impl = oauth2_service::OAuth2GrpcService::new(oauth2_api, oauth2_auth_interceptor);
        // No global interceptor: public endpoints are unauthenticated,
        // private endpoints call require_auth() inline.
        router = router.add_service(OAuth2ServiceServer::new(oauth2_impl).with_message_size_limit(max_message_size));
        tracing::info!("OAuth2Service gRPC registered (public + authenticated split)");
    }

    // Register provider gRPC services
    if let Some(_providers_mgr) = providers_manager {
        tracing::info!("Registering provider gRPC services");

        // Create provider instances for the gRPC services
        let provider_instance_manager_for_provider = _providers_mgr.instance_manager().clone();
        let alist_provider = Arc::new(AlistProvider::new(
            provider_instance_manager_for_provider.clone(),
        ));
        let bilibili_provider = Arc::new(BilibiliProvider::new(
            provider_instance_manager_for_provider.clone(),
        ));
        let emby_provider = Arc::new(EmbyProvider::new(
            provider_instance_manager_for_provider,
        ));

        let provider_jwt_validator = Arc::new(synctv_core::service::auth::JwtValidator::new(
            Arc::new(jwt_service_for_provider.clone()),
        ));

        // Build a RouterConfig for provider gRPC services, sharing common fields
        let provider_router_config = Arc::new(crate::http::RouterConfig {
            config: Arc::new(config.clone()),
            user_service: user_service_for_provider,
            room_service: room_service_for_provider,
            provider_instance_manager: _providers_mgr.instance_manager().clone(),
            user_provider_credential_repository: user_provider_credential_repository.clone(),
            alist_provider: alist_provider.clone(),
            bilibili_provider: bilibili_provider.clone(),
            emby_provider: emby_provider.clone(),
            cluster_manager: None,
            connection_manager: Arc::new(connection_manager_for_provider.clone()),
            jwt_service: jwt_service_for_provider.clone(),
            redis_publish_tx: redis_publish_tx.clone(),
            oauth2_service: None,
            settings_service: Some(settings_service.clone()),
            settings_registry: None,
            email_service: None,
            email_token_service: None,
            publish_key_service: None,
            notification_service: None,
            audit_service: audit_service.clone(),
            live_streaming_infrastructure: None,
            rate_limiter: rate_limiter_for_provider,
            ws_ticket_service: None,
            redis_conn: redis_conn.clone(),
            builtin_stun_url: None,
            credential_encryption: None,
        });

        // Reuse the already-constructed client_api and use actual rate limit config
        let app_state = Arc::new(crate::http::AppState {
            router_config: provider_router_config,
            rate_limit_config: Arc::new(config.http_rate_limits.clone()),
            jwt_validator: provider_jwt_validator,
            security_pipeline: Arc::new(synctv_core::service::SecurityPipeline::new(
                user_service.clone(),
            ).with_token_blacklist(
                user_service.token_blacklist_store(),
                user_service.key_builder().clone(),
            )),
            client_api: client_api.clone(),
            admin_api: None,
            notification_api: None,
            oauth2_api: None,
            bilibili_api: Arc::new(crate::impls::BilibiliApiImpl::new(bilibili_provider)),
            alist_api: Arc::new(crate::impls::AlistApiImpl::new(alist_provider)),
            emby_api: Arc::new(crate::impls::EmbyApiImpl::new(emby_provider)),
        });

        // Register provider gRPC services with auth interceptor
        use synctv_proto::providers::alist::alist_provider_service_server::AlistProviderServiceServer;
        use synctv_proto::providers::bilibili::bilibili_provider_service_server::BilibiliProviderServiceServer;
        use synctv_proto::providers::emby::emby_provider_service_server::EmbyProviderServiceServer;

        let provider_interceptor1 = auth_interceptor.clone();
        let provider_interceptor2 = auth_interceptor.clone();
        let provider_interceptor3 = auth_interceptor.clone();

        // Register provider services with interceptors
        // Note: Message size limits must be applied via server-wide config, not per-service here
        router = router.add_service(AlistProviderServiceServer::with_interceptor(
            providers::alist::AlistProviderGrpcService::new(app_state.clone()),
            move |req| provider_interceptor1.inject_user(req),
        ));
        router = router.add_service(BilibiliProviderServiceServer::with_interceptor(
            providers::bilibili::BilibiliProviderGrpcService::new(app_state.clone()),
            move |req| provider_interceptor2.inject_user(req),
        ));
        router = router.add_service(EmbyProviderServiceServer::with_interceptor(
            providers::emby::EmbyProviderGrpcService::new(app_state),
            move |req| provider_interceptor3.inject_user(req),
        ));
    }

    // Register cluster gRPC service (requires cluster_secret and NodeRegistry)
    if config.server.cluster_secret.is_empty() {
        tracing::error!(
            "cluster_secret is empty — cluster gRPC service will NOT be registered. \
             Cluster coordination will be disabled. Set cluster_secret in config to enable."
        );
    } else if let Some(ref nr) = node_registry {
        let cluster_server = synctv_cluster::grpc::ClusterServer::new(
            nr.clone(),
            cluster_node_id.clone(),
        ).with_connection_manager(
            std::sync::Arc::new(connection_manager_for_provider.clone()),
        );
        let cluster_interceptor = ClusterAuthInterceptor::new(config.server.cluster_secret.clone());
        router = router.add_service(
            synctv_cluster::grpc::ClusterServiceServer::with_interceptor(
                cluster_server,
                move |req| cluster_interceptor.validate(req),
            ),
        );
        tracing::info!("Cluster gRPC service registered with shared-secret auth (using shared NodeRegistry)");
    } else if let Some(redis_client) = redis_client {
        // Fallback: create a standalone NodeRegistry for cluster gRPC (single-node mode with Redis)
        let fallback_result = synctv_cluster::discovery::NodeRegistry::new(redis_client, cluster_node_id.clone(), 30, &config.redis.key_prefix)
            .map_err(|e| e.to_string());
        match fallback_result {
            Ok(fallback_registry) => {
                let cluster_server = synctv_cluster::grpc::ClusterServer::new(
                    std::sync::Arc::new(fallback_registry),
                    cluster_node_id.clone(),
                ).with_connection_manager(
                    std::sync::Arc::new(connection_manager_for_provider.clone()),
                );
                let cluster_interceptor = ClusterAuthInterceptor::new(config.server.cluster_secret.clone());
                router = router.add_service(
                    synctv_cluster::grpc::ClusterServiceServer::with_interceptor(
                        cluster_server,
                        move |req| cluster_interceptor.validate(req),
                    ),
                );
                tracing::info!("Cluster gRPC service registered with shared-secret auth (standalone NodeRegistry)");
            }
            Err(e) => {
                tracing::warn!("Failed to create NodeRegistry for cluster gRPC: {e}");
            }
        }
    } else {
        tracing::info!("No Redis configured — skipping cluster gRPC service registration");
    }

    // Register gRPC health check service (standard grpc.health.v1.Health).
    // All registered services are marked as SERVING so gRPC health probes
    // return the correct status rather than UNKNOWN.
    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<AuthServiceServer<ClientServiceImpl>>()
        .await;
    health_reporter
        .set_serving::<UserServiceServer<ClientServiceImpl>>()
        .await;
    health_reporter
        .set_serving::<RoomServiceServer<ClientServiceImpl>>()
        .await;
    health_reporter
        .set_serving::<MediaServiceServer<ClientServiceImpl>>()
        .await;
    health_reporter
        .set_serving::<PublicServiceServer<ClientServiceImpl>>()
        .await;
    health_reporter
        .set_serving::<EmailServiceServer<ClientServiceImpl>>()
        .await;
    health_reporter
        .set_serving::<AdminServiceServer<AdminServiceImpl>>()
        .await;
    router = router.add_service(health_service);
    tracing::info!("gRPC health check service registered (all services marked SERVING)");

    // Register gRPC reflection service if enabled in config
    if config.server.enable_reflection {
        let reflection_service = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(synctv_proto::FILE_DESCRIPTOR_SET)
            .register_encoded_file_descriptor_set(synctv_proto::PROVIDERS_FILE_DESCRIPTOR_SET)
            .build_v1()
            .map_err(|e| anyhow::anyhow!("Failed to build gRPC reflection service: {e}"))?;
        router = router.add_service(reflection_service);
        tracing::info!("gRPC reflection service registered");
    }

    // Start server with graceful shutdown support
    router
        .serve_with_shutdown(addr, async move {
            if let Some(mut rx) = shutdown_rx {
                // Use centralized shutdown signal from the server
                let _ = rx.changed().await;
            } else {
                // Fallback: listen for Ctrl+C
                tokio::signal::ctrl_c().await.ok();
            }
        })
        .await
        .map_err(|e| anyhow::anyhow!("gRPC server error: {e}"))?;

    Ok(())
}

