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

    // Create transport-level rate limit interceptor with tiered limits per service.
    // Each service gets its own tier matching HTTP rate limits to prevent attackers
    // from bypassing HTTP rate limits via the gRPC API.
    let grpc_rate_limiter = interceptors::GrpcRateLimitInterceptor::new(
        rate_limiter.clone(),
        interceptors::GrpcRateLimitTier::Read, // default tier, overridden per service below
        60,  // 60 second window
    );

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

    // Create server builder with blacklist checking tower layer.
    // This layer extracts the raw JWT bearer token from the HTTP Authorization
    // header and performs an async Redis blacklist lookup. It runs before tonic
    // routes and interceptors, so public endpoints (no Authorization header)
    // pass through without a blacklist check.
    let blacklist_layer = blacklist_layer::BlacklistCheckLayer::new(token_blacklist_service.clone());
    let mut server_builder = Server::builder()
        .layer(blacklist_layer);

    // Note: gRPC reflection is disabled - proto definitions are in synctv-proto crate
    // To enable reflection in the future, we would need to re-export descriptor from synctv-proto

    // Create auth interceptor for authenticated services
    let auth_interceptor = AuthInterceptor::new(jwt_service);

    // Clone interceptors for different services
    let user_interceptor = auth_interceptor.clone();
    let admin_interceptor = auth_interceptor.clone();
    let room_interceptor1 = auth_interceptor.clone();
    let room_interceptor2 = auth_interceptor.clone();

    // Assign appropriate rate limit tiers per service (aligned with HTTP middleware)
    let rl_auth = grpc_rate_limiter.with_tier(interceptors::GrpcRateLimitTier::Auth);
    let rl_user = grpc_rate_limiter.with_tier(interceptors::GrpcRateLimitTier::Write);
    let rl_room = grpc_rate_limiter.with_tier(interceptors::GrpcRateLimitTier::Write);
    let rl_media = grpc_rate_limiter.with_tier(interceptors::GrpcRateLimitTier::Media);
    let rl_public = grpc_rate_limiter.with_tier(interceptors::GrpcRateLimitTier::Read);
    let rl_email = grpc_rate_limiter.with_tier(interceptors::GrpcRateLimitTier::Email);
    let rl_admin = grpc_rate_limiter.with_tier(interceptors::GrpcRateLimitTier::Admin);

    // Build router - register all client services with rate limiting + auth interceptors
    let client_service_clone1 = client_service.clone();
    let client_service_clone2 = client_service.clone();
    let client_service_clone3 = client_service.clone();
    let client_service_clone4 = client_service.clone();
    let client_service_clone5 = client_service.clone();

    let mut router = server_builder
        // AuthService - rate limited (public: register, login, refresh_token)
        .add_service(AuthServiceServer::with_interceptor(
            client_service,
            move |req| rl_auth.check(req),
        ))
        // UserService - rate limited + JWT authentication (inject UserContext)
        .add_service(UserServiceServer::with_interceptor(
            client_service_clone1,
            move |req| {
                let req = rl_user.check(req)?;
                user_interceptor.inject_user(req)
            },
        ))
        // RoomService - rate limited + JWT + room_id (inject RoomContext)
        .add_service(RoomServiceServer::with_interceptor(
            client_service_clone2,
            move |req| {
                let req = rl_room.check(req)?;
                room_interceptor1.inject_room(req)
            },
        ))
        // MediaService - rate limited + JWT + room_id (inject RoomContext)
        .add_service(MediaServiceServer::with_interceptor(
            client_service_clone3,
            move |req| {
                let req = rl_media.check(req)?;
                room_interceptor2.inject_room(req)
            },
        ))
        // PublicService - rate limited (public room discovery)
        .add_service(PublicServiceServer::with_interceptor(
            client_service_clone4,
            move |req| rl_public.check(req),
        ))
        // EmailService - rate limited (send codes, confirm with token)
        .add_service(EmailServiceServer::with_interceptor(
            client_service_clone5,
            move |req| rl_email.check(req),
        ))
        // AdminService - rate limited + JWT authentication (inject UserContext)
        .add_service(AdminServiceServer::with_interceptor(
            admin_service,
            move |req| {
                let req = rl_admin.check(req)?;
                admin_interceptor.inject_user(req)
            },
        ));

    // Register NotificationService if notification_service is configured
    if let Some(notif_svc) = notification_service {
        let notification_interceptor = auth_interceptor.clone();
        let rl_notif = grpc_rate_limiter.with_tier(interceptors::GrpcRateLimitTier::Read);
        let notification_api = Arc::new(crate::impls::NotificationApiImpl::new(notif_svc));
        let notif_impl = NotificationServiceImpl::new(notification_api);
        router = router.add_service(NotificationServiceServer::with_interceptor(
            notif_impl,
            move |req| {
                let req = rl_notif.check(req)?;
                notification_interceptor.inject_user(req)
            },
        ));
        tracing::info!("NotificationService gRPC registered");
    }

    // Register OAuth2Service if oauth2_service is configured
    if let Some(oauth2_svc) = oauth2_service {
        use synctv_proto::client::o_auth2_service_server::OAuth2ServiceServer;
        let oauth2_interceptor = auth_interceptor.clone();
        let rl_oauth2 = grpc_rate_limiter.with_tier(interceptors::GrpcRateLimitTier::Auth);
        let oauth2_api = Arc::new(crate::impls::OAuth2ApiImpl::new(
            oauth2_svc,
            user_service.clone(),
        ));
        let oauth2_impl = oauth2_service::OAuth2GrpcService::new(oauth2_api);
        // OAuth2Service: All gRPC endpoints require authentication for simplicity
        // For public OAuth2 flows (GetAuthorizationUrl, ExchangeAuthorizationCode, ListAvailableProviders),
        // clients should use the HTTP API instead, which supports unauthenticated access
        router = router.add_service(OAuth2ServiceServer::with_interceptor(
            oauth2_impl,
            move |req| {
                let req = rl_oauth2.check(req)?;
                oauth2_interceptor.inject_user(req)
            },
        ));
        tracing::info!("OAuth2Service gRPC registered");
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
            token_blacklist_service: token_blacklist_service.clone(),
        });

        // Register provider gRPC services with auth interceptor
        use synctv_proto::providers::alist::alist_provider_service_server::AlistProviderServiceServer;
        use synctv_proto::providers::bilibili::bilibili_provider_service_server::BilibiliProviderServiceServer;
        use synctv_proto::providers::emby::emby_provider_service_server::EmbyProviderServiceServer;

        let provider_interceptor1 = auth_interceptor.clone();
        let provider_interceptor2 = auth_interceptor.clone();
        let provider_interceptor3 = auth_interceptor.clone();
        let rl_provider1 = grpc_rate_limiter.with_tier(interceptors::GrpcRateLimitTier::Read);
        let rl_provider2 = grpc_rate_limiter.with_tier(interceptors::GrpcRateLimitTier::Read);
        let rl_provider3 = grpc_rate_limiter.with_tier(interceptors::GrpcRateLimitTier::Read);

        router = router.add_service(AlistProviderServiceServer::with_interceptor(
            providers::alist::AlistProviderGrpcService::new(app_state.clone()),
            move |req| {
                let req = rl_provider1.check(req)?;
                provider_interceptor1.inject_user(req)
            },
        ));
        router = router.add_service(BilibiliProviderServiceServer::with_interceptor(
            providers::bilibili::BilibiliProviderGrpcService::new(app_state.clone()),
            move |req| {
                let req = rl_provider2.check(req)?;
                provider_interceptor2.inject_user(req)
            },
        ));
        router = router.add_service(EmbyProviderServiceServer::with_interceptor(
            providers::emby::EmbyProviderGrpcService::new(app_state),
            move |req| {
                let req = rl_provider3.check(req)?;
                provider_interceptor3.inject_user(req)
            },
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
            match synctv_cluster::discovery::NodeRegistry::new(redis_url, cluster_node_id.clone(), 30) {
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

