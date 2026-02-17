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

/// Map a provider API string error to an appropriate gRPC status code.
///
/// Uses the keyword-based [`classify_error`](crate::impls::classify_error)
/// for legacy provider errors that return `String` instead of `ApiError`.
pub(crate) fn map_provider_error(err: String) -> tonic::Status {
    use crate::impls::{classify_error, ErrorKind};
    match classify_error(&err) {
        ErrorKind::NotFound => tonic::Status::not_found(err),
        ErrorKind::Unauthenticated => tonic::Status::unauthenticated(err),
        ErrorKind::PermissionDenied => tonic::Status::permission_denied(err),
        ErrorKind::AlreadyExists => tonic::Status::already_exists(err),
        ErrorKind::InvalidArgument => tonic::Status::invalid_argument(err),
        ErrorKind::Internal => {
            tracing::error!("Provider internal error: {err}");
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
    pub cluster_manager: Arc<ClusterManager>,
    pub redis_publish_tx: Option<tokio::sync::mpsc::Sender<PublishRequest>>,
    pub rate_limiter: RateLimiter,
    pub rate_limit_config: RateLimitConfig,
    pub content_filter: ContentFilter,
    pub connection_manager: ConnectionManager,
    pub providers_manager: Option<Arc<ProvidersManager>>,
    pub provider_instance_manager: Arc<RemoteProviderManager>,
    pub provider_instance_repository: Arc<synctv_core::repository::ProviderInstanceRepository>,
    pub user_provider_credential_repository: Arc<synctv_core::repository::UserProviderCredentialRepository>,
    pub settings_service: Arc<SettingsService>,
    pub settings_registry: Option<Arc<SettingsRegistry>>,
    pub email_service: Option<Arc<EmailService>>,
    pub email_token_service: Option<Arc<EmailTokenService>>,
    pub sfu_manager: Option<Arc<synctv_sfu::SfuManager>>,
    pub live_streaming_infrastructure: Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    pub publish_key_service: Option<Arc<synctv_core::service::PublishKeyService>>,
    pub notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    pub oauth2_service: Option<Arc<synctv_core::service::OAuth2Service>>,
    pub audit_service: Arc<synctv_core::service::AuditService>,
    pub node_registry: Option<Arc<synctv_cluster::discovery::NodeRegistry>>,
    pub token_blacklist_service: synctv_core::service::TokenBlacklistService,
    /// Shared Redis connection for playback caching
    pub redis_conn: Option<redis::aio::ConnectionManager>,
    pub shutdown_rx: Option<tokio::sync::watch::Receiver<bool>>,
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
        provider_instance_repository: _,
        user_provider_credential_repository,
        settings_service,
        settings_registry,
        email_service,
        email_token_service,
        sfu_manager,
        live_streaming_infrastructure,
        publish_key_service,
        notification_service,
        oauth2_service,
        audit_service,
        node_registry,
        token_blacklist_service,
        redis_conn,
        shutdown_rx,
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
    let cluster_node_id = cluster_manager.node_id().to_string();

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
        sfu_manager.clone(),
        publish_key_service,
        jwt_service.clone(),
        live_streaming_infrastructure.clone(),
        providers_manager_for_client.clone(),
        settings_registry.clone(),
    ).with_redis_publish_tx(redis_publish_tx.clone())
     .with_redis_conn(redis_conn.clone())
     .with_rate_limiter(rate_limiter.clone()));

    let rate_limiter_for_layer = rate_limiter.clone();
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
        token_blacklist_service: token_blacklist_service.clone(),
        settings_registry: settings_registry.clone(),
        providers_manager: providers_manager_for_client,
        config: Arc::new(config.clone()),
        sfu_manager: sfu_manager.clone(),
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

    // Create server builder with the security checking tower layer.
    // This layer extracts the raw JWT bearer token from the HTTP Authorization
    // header and performs four async security checks via the shared SecurityPipeline:
    // 1. JWT verification (validate signature, expiration, access token type)
    // 2. Token blacklist check (explicit logout/revocation via Redis)
    // 3. Password invalidation check (tokens issued before password change)
    // 4. User status check (banned/pending/deleted)
    // It runs before tonic routes and interceptors, so public endpoints (no Authorization header)
    // pass through without security checks.
    let security_pipeline = synctv_core::service::SecurityPipeline::new(
        Arc::new(token_blacklist_service.clone()),
        user_service.clone(),
    );
    let blacklist_layer = blacklist_layer::BlacklistCheckLayer::new(
        jwt_service,
        security_pipeline,
    );
    // Distributed rate limiting layer: uses Redis when available (shared across
    // replicas), falls back to in-memory governor when Redis is unavailable.
    // Determines tier per-request from the gRPC service path.
    let distributed_rate_limit_layer = rate_limit_layer::GrpcRateLimitLayer::new(
        rate_limiter_for_layer,
        60,  // 60 second window
        Arc::new(config.clone()),
    );
    let mut server_builder = Server::builder()
        .layer(blacklist_layer)
        .layer(distributed_rate_limit_layer);

    // Clone interceptors for different services
    let user_interceptor = auth_interceptor.clone();
    let admin_interceptor = auth_interceptor.clone();
    let room_interceptor1 = auth_interceptor.clone();
    let room_interceptor2 = auth_interceptor.clone();

    // Rate limiting is handled by the distributed_rate_limit_layer applied at the
    // server level (above). Per-service interceptors only handle auth concerns.

    // Build router - register all client services with auth interceptors
    let client_service_clone1 = client_service.clone();
    let client_service_clone2 = client_service.clone();
    let client_service_clone3 = client_service.clone();
    let client_service_clone4 = client_service.clone();
    let client_service_clone5 = client_service.clone();

    let mut router = server_builder
        // AuthService (public: register, login, refresh_token)
        .add_service(AuthServiceServer::new(client_service))
        // UserService - JWT authentication (inject UserContext)
        .add_service(UserServiceServer::with_interceptor(
            client_service_clone1,
            move |req| user_interceptor.inject_user(req),
        ))
        // RoomService - JWT + room_id (inject RoomContext)
        .add_service(RoomServiceServer::with_interceptor(
            client_service_clone2,
            move |req| room_interceptor1.inject_room(req),
        ))
        // MediaService - JWT + room_id (inject RoomContext)
        .add_service(MediaServiceServer::with_interceptor(
            client_service_clone3,
            move |req| room_interceptor2.inject_room(req),
        ))
        // PublicService (public room discovery)
        .add_service(PublicServiceServer::new(client_service_clone4))
        // EmailService (send codes, confirm with token)
        .add_service(EmailServiceServer::new(client_service_clone5))
        // AdminService - JWT authentication (inject UserContext)
        .add_service(AdminServiceServer::with_interceptor(
            admin_service,
            move |req| admin_interceptor.inject_user(req),
        ));

    // Register NotificationService if notification_service is configured
    if let Some(notif_svc) = notification_service {
        let notification_interceptor = auth_interceptor.clone();
        let notification_api = Arc::new(crate::impls::NotificationApiImpl::new(notif_svc));
        let notif_impl = NotificationServiceImpl::new(notification_api);
        router = router.add_service(NotificationServiceServer::with_interceptor(
            notif_impl,
            move |req| notification_interceptor.inject_user(req),
        ));
        tracing::info!("NotificationService gRPC registered");
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
        router = router.add_service(OAuth2ServiceServer::new(oauth2_impl));
        tracing::info!("OAuth2Service gRPC registered (public + authenticated split)");
    }

    // Register provider gRPC services
    if let Some(_providers_mgr) = providers_manager {
        tracing::info!("Registering provider gRPC services");

        // Create AppState for provider gRPC services
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
        let app_state = Arc::new(crate::http::AppState {
            config: Arc::new(config.clone()),
            user_service: user_service_for_provider.clone(),
            room_service: room_service_for_provider.clone(),
            provider_instance_manager: _providers_mgr.instance_manager().clone(),
            user_provider_credential_repository: user_provider_credential_repository.clone(),
            alist_provider: alist_provider.clone(),
            bilibili_provider: bilibili_provider.clone(),
            emby_provider: emby_provider.clone(),
            cluster_manager: None, // gRPC doesn't expose cluster_manager to HTTP
            connection_manager: Arc::new(connection_manager_for_provider.clone()),
            jwt_service: jwt_service_for_provider.clone(),
            redis_publish_tx: redis_publish_tx.clone(),
            oauth2_service: None,
            settings_service: Some(settings_service.clone()),
            settings_registry: None,
            email_service: None,
            publish_key_service: None,
            notification_service: None,
            live_streaming_infrastructure: None,
            rate_limiter: rate_limiter_for_provider,
            rate_limit_config: Arc::new(crate::http::middleware::RateLimitConfig::default()),
            jwt_validator: provider_jwt_validator,
            ws_ticket_service: None, // WebSocket ticket is HTTP-only
            client_api: Arc::new(crate::impls::ClientApiImpl::new(
                user_service_for_provider,
                room_service_for_provider,
                Arc::new(connection_manager_for_provider.clone()),
                Arc::new(config.clone()),
                sfu_manager,
                None, // No publish_key_service for provider gRPC
                jwt_service_for_provider.clone(),
                None, // No live_streaming_infrastructure for provider gRPC
                None, // No providers_manager for provider gRPC
                None, // No settings_registry for provider gRPC
            ).with_redis_publish_tx(redis_publish_tx.clone())
             .with_redis_conn(redis_conn.clone())),
            admin_api: None,
            notification_api: None,
            oauth2_api: None, // OAuth2 not used in provider gRPC
            bilibili_api: Arc::new(crate::impls::BilibiliApiImpl::new(bilibili_provider.clone())),
            alist_api: Arc::new(crate::impls::AlistApiImpl::new(alist_provider.clone())),
            emby_api: Arc::new(crate::impls::EmbyApiImpl::new(emby_provider.clone())),
            redis_conn: redis_conn.clone(),
            security_pipeline: Arc::new(synctv_core::service::SecurityPipeline::new(
                Arc::new(token_blacklist_service.clone()),
                user_service.clone(),
            )),
            sfu_session_manager: None,
            token_blacklist_service: token_blacklist_service.clone(),
        });

        // Register provider gRPC services with auth interceptor
        use synctv_proto::providers::alist::alist_provider_service_server::AlistProviderServiceServer;
        use synctv_proto::providers::bilibili::bilibili_provider_service_server::BilibiliProviderServiceServer;
        use synctv_proto::providers::emby::emby_provider_service_server::EmbyProviderServiceServer;

        let provider_interceptor1 = auth_interceptor.clone();
        let provider_interceptor2 = auth_interceptor.clone();
        let provider_interceptor3 = auth_interceptor.clone();

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
    if !config.server.cluster_secret.is_empty() {
        if let Some(ref nr) = node_registry {
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
        } else {
            // Fallback: create a standalone NodeRegistry for cluster gRPC (single-node mode)
            let redis_url = if config.redis.url.is_empty() {
                None
            } else {
                Some(config.redis.url.clone())
            };
            match synctv_cluster::discovery::NodeRegistry::new(redis_url, cluster_node_id.clone(), 30, &config.redis.key_prefix) {
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
        }
    }

    // Register gRPC health check service (standard grpc.health.v1.Health)
    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    // Mark the overall server as serving
    health_reporter
        .set_serving::<AuthServiceServer<ClientServiceImpl>>()
        .await;
    router = router.add_service(health_service);
    tracing::info!("gRPC health check service registered");

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

