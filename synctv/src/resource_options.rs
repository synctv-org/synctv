use std::collections::HashMap;

use crate::bootstrap::{
    CacheOptions, CoreServicesOptions, DatabaseInitOptions, DatabasePoolOptions,
    FileStorageBackendOptions, FileStorageDatabaseCompressionOption, FileStorageDatabaseOptions,
    FileStorageOptions, FileStorageS3Options, JwtOptions, MessagingRateLimitOptions,
    RedisConnectionOptions, RedisDeploymentMode, RootUserBootstrapOptions, SecurityOptions,
    SsrfOptions,
};
use synctv_api::{
    ApiRuntimeSettings, ApiServerSettings, ClusterRuntimeSettings, ConnectionLimitSettings,
    LivestreamRuntimeSettings, MetricsAuthMode, MetricsAuthSettings, MetricsKubernetesAuthSettings,
    MetricsRuntimeSettings, ProxySliceCacheRuntimeSettings, RateLimitScopeRule,
    RateLimitScopeStrategy, RedisRuntimeSettings, RequestRateLimitSettings, WebRtcRuntimeSettings,
};
#[cfg(feature = "k8s")]
use synctv_cluster::leader::K8sLeaderRuntimeOptions;
use synctv_cluster::leader::{LeaderElectionMode, LeaderRedisDeploymentMode, LeaderRuntimeOptions};
use synctv_core::clock::{
    ClockSyncOptions, ClockSyncProvider, ClockSyncSntpProviderOptions, TimeOptions,
};
use synctv_core::logging::{
    ComponentLoggingOptions, LogColor, LogOutput, LogRotation, LoggingOptions,
};
use synctv_core::service::{
    LocalProviderHttpOptions, MediaProvidersOptions, PasskeyServiceOptions,
};
use synctv_core::validation::PasswordComplexityOptions;
use synctv_livestream::{HlsS3Options, HlsStorageBackend};
use synctv_management::server::{ManagementRuntimeSettings, ManagementTransport};
use synctv_realtime::sync::ConnectionLimitsOptions;

use crate::app_config::{
    AppConfig, ClusterLeaderElectionMode, FileStorageBackendConfig, FileStorageDatabaseCompression,
    HlsStorageBackend as AppHlsStorageBackend, HlsStorageConfig,
    ManagementTransport as AppManagementTransport, MetricsAuthMode as AppMetricsAuthMode,
    RateLimitScopeStrategy as AppRateLimitScopeStrategy,
    RedisDeploymentMode as AppRedisDeploymentMode,
};

pub fn logging_options(config: &AppConfig) -> LoggingOptions {
    LoggingOptions {
        global: component_logging("global", &config.logging, Vec::new()),
        components: vec![
            component_logging(
                "server",
                &config.server.logging,
                vec![
                    "synctv::server".to_string(),
                    "synctv_api".to_string(),
                    "synctv_api_common".to_string(),
                    "synctv_api_grpc".to_string(),
                    "synctv_api_http".to_string(),
                ],
            ),
            component_logging(
                "health",
                &config.health.logging,
                vec!["synctv::health".to_string()],
            ),
            component_logging(
                "metrics",
                &config.metrics.logging,
                vec![
                    "synctv::metrics".to_string(),
                    "synctv_core::metrics".to_string(),
                ],
            ),
            component_logging(
                "cluster",
                &config.cluster.logging,
                vec![
                    "synctv::cluster".to_string(),
                    "synctv::bootstrap::cluster".to_string(),
                    "synctv_cluster".to_string(),
                ],
            ),
            component_logging(
                "management",
                &config.management.logging,
                vec!["synctv_management".to_string()],
            ),
            component_logging(
                "livestream",
                &config.livestream.logging,
                vec![
                    "synctv::bootstrap::livestream".to_string(),
                    "synctv::rtmp_auth".to_string(),
                    "synctv_livestream".to_string(),
                    "synctv_xiu".to_string(),
                ],
            ),
            component_logging(
                "webrtc",
                &config.webrtc.logging,
                vec![
                    "synctv::bootstrap::webrtc".to_string(),
                    "synctv_core::service::stun_server".to_string(),
                ],
            ),
        ],
    }
}

fn component_logging(
    name: &str,
    config: &crate::app_config::LoggingConfig,
    targets: Vec<String>,
) -> ComponentLoggingOptions {
    let output = match &config.output {
        crate::app_config::LogOutput::Named(crate::app_config::LogOutputName::Stdout) => {
            LogOutput::Stdout
        }
        crate::app_config::LogOutput::Named(crate::app_config::LogOutputName::Stderr) => {
            LogOutput::Stderr
        }
        crate::app_config::LogOutput::File(file) => LogOutput::File {
            path: file.path.clone().into(),
            rotation: match file.rotation.strategy.as_str() {
                "hourly" => LogRotation::Hourly,
                "never" => LogRotation::Never,
                _ => LogRotation::Daily,
            },
            max_files: file.rotation.max_files,
        },
    };
    ComponentLoggingOptions {
        name: name.to_string(),
        targets,
        level: config.level.clone(),
        format: config.format.clone(),
        output,
        color: match config.color {
            crate::app_config::LogColor::Auto => LogColor::Auto,
            crate::app_config::LogColor::Always => LogColor::Always,
            crate::app_config::LogColor::Never => LogColor::Never,
        },
    }
}

pub fn database_pool_options(config: &AppConfig) -> DatabasePoolOptions {
    DatabasePoolOptions {
        url: config.database.url.clone(),
        read_url: config.database.read_url.clone(),
        read_host: config.database.read_host.clone(),
        read_port: config.database.read_port,
        host: config.database.host.clone(),
        port: config.database.port,
        username: config.database.username.clone(),
        password: config.database.password.clone(),
        name: config.database.name.clone(),
        max_connections: config.database.max_connections,
        min_connections: config.database.min_connections,
        connect_timeout_seconds: config.database.connect_timeout_seconds,
        idle_timeout_seconds: config.database.idle_timeout_seconds,
        max_lifetime_seconds: config.database.max_lifetime_seconds,
    }
}

pub fn database_init_options(config: &AppConfig) -> DatabaseInitOptions {
    DatabaseInitOptions {
        database: database_pool_options(config),
        logging: logging_options(config),
    }
}

pub fn redis_deployment_mode(mode: &AppRedisDeploymentMode) -> RedisDeploymentMode {
    match mode {
        AppRedisDeploymentMode::Standalone => RedisDeploymentMode::Standalone,
        AppRedisDeploymentMode::Sentinel => RedisDeploymentMode::Sentinel,
    }
}

pub fn redis_connection_options(config: &AppConfig) -> RedisConnectionOptions {
    RedisConnectionOptions {
        url: config.redis.url.clone(),
        host: config.redis.host.clone(),
        port: config.redis.port,
        username: config.redis.username.clone(),
        password: config.redis.password.clone(),
        database: config.redis.database,
        connect_timeout_seconds: config.redis.connect_timeout_seconds,
        response_timeout_seconds: config.redis.response_timeout_seconds,
        pipeline_buffer_size: config.redis.pipeline_buffer_size,
        key_prefix: config.redis.key_prefix.clone(),
        deployment_mode: redis_deployment_mode(&config.redis.deployment_mode),
        sentinel_master_name: config.redis.sentinel_master_name.clone(),
        sentinel_addresses: config.redis.sentinel_addresses.clone(),
    }
}

pub fn time_options(config: &AppConfig) -> TimeOptions {
    TimeOptions {
        timezone: config.time.timezone.clone(),
        clock_sync: ClockSyncOptions {
            enabled: config.time.clock_sync.enabled,
            provider: match &config.time.clock_sync.provider {
                crate::app_config::ClockSyncProvider::Sntp(provider) => {
                    ClockSyncProvider::Sntp(ClockSyncSntpProviderOptions {
                        servers: provider.servers.clone(),
                        interval_seconds: provider.interval_seconds,
                        timeout_millis: provider.timeout_millis,
                    })
                }
            },
        },
    }
}

pub fn root_user_bootstrap_options(config: &AppConfig) -> RootUserBootstrapOptions {
    RootUserBootstrapOptions {
        create_root_user: config.bootstrap.create_root_user,
        root_username: config.bootstrap.root_username.clone(),
        root_password: config.bootstrap.root_password.clone(),
    }
}

fn ssrf_options(config: &AppConfig) -> SsrfOptions {
    SsrfOptions {
        enabled: config.security.ssrf.enabled,
        allow_private_network_targets: config.security.ssrf.allow_private_network_targets,
        allowed_hosts: config.security.ssrf.allowed_hosts.clone(),
        allowed_ip_ranges: config.security.ssrf.allowed_ip_ranges.clone(),
    }
}

fn security_options(config: &AppConfig) -> SecurityOptions {
    SecurityOptions {
        email_outbox_encryption_key: config.security.email_outbox_encryption_key.clone(),
        opaque_server_setup_secret: config.security.opaque_server_setup_secret.clone(),
        login_discovery_key: config.security.login_discovery_key.clone(),
        ssrf: ssrf_options(config),
    }
}

fn media_providers_options(config: &AppConfig) -> MediaProvidersOptions {
    let http = |value: &crate::app_config::LocalProviderHttpConfig| LocalProviderHttpOptions {
        request_timeout_seconds: value.request_timeout_seconds,
        connect_timeout_seconds: value.connect_timeout_seconds,
    };

    MediaProvidersOptions {
        alist: http(&config.media_providers.alist),
        bilibili: http(&config.media_providers.bilibili),
        emby: http(&config.media_providers.emby),
        cloudreve: http(&config.media_providers.cloudreve),
    }
}

fn cache_options(config: &AppConfig) -> CacheOptions {
    CacheOptions {
        l1_capacity: config.cache.l1_capacity,
        l1_ttl_seconds: config.cache.l1_ttl_seconds,
        l2_ttl_seconds: config.cache.l2_ttl_seconds,
        username_cache_capacity: config.cache.username_cache_capacity,
        username_cache_ttl_seconds: config.cache.username_cache_ttl_seconds,
    }
}

fn messaging_rate_limit_options(config: &AppConfig) -> MessagingRateLimitOptions {
    MessagingRateLimitOptions {
        chat_per_second: config.messaging_rate_limits.chat_per_second,
        window_seconds: config.messaging_rate_limits.window_seconds,
    }
}

fn jwt_options(config: &AppConfig) -> JwtOptions {
    JwtOptions {
        secret: config.jwt.secret.clone(),
        access_token_duration_hours: config.jwt.access_token_duration_hours,
        refresh_token_duration_days: config.jwt.refresh_token_duration_days,
        guest_token_duration_hours: config.jwt.guest_token_duration_hours,
        clock_skew_leeway_secs: config.jwt.clock_skew_leeway_secs,
    }
}

fn database_compression_option(
    compression: FileStorageDatabaseCompression,
) -> FileStorageDatabaseCompressionOption {
    match compression {
        FileStorageDatabaseCompression::None => FileStorageDatabaseCompressionOption::None,
        FileStorageDatabaseCompression::Lz4 => FileStorageDatabaseCompressionOption::Lz4,
        FileStorageDatabaseCompression::Zstd => FileStorageDatabaseCompressionOption::Zstd,
    }
}

fn file_storage_backend_options(backend: &FileStorageBackendConfig) -> FileStorageBackendOptions {
    match backend {
        FileStorageBackendConfig::Disabled => FileStorageBackendOptions::Disabled,
        FileStorageBackendConfig::Database(config) => {
            FileStorageBackendOptions::Database(FileStorageDatabaseOptions {
                compression: database_compression_option(config.compression),
                compression_min_size_bytes: config.compression_min_size_bytes,
                compression_min_savings_percent: config.compression_min_savings_percent,
            })
        }
        FileStorageBackendConfig::S3(config) => {
            FileStorageBackendOptions::S3(FileStorageS3Options {
                endpoint: config.endpoint.clone(),
                access_key_id: config.access_key_id.clone(),
                secret_access_key: config.secret_access_key.clone(),
                bucket: config.bucket.clone(),
                region: config.region.clone(),
                base_path: config.base_path.clone(),
                public_base_url: config.public_base_url.clone(),
                upload_expires_seconds: config.upload_expires_seconds,
            })
        }
    }
}

fn file_storage_options(config: &AppConfig) -> FileStorageOptions {
    let backends = config
        .file_storage
        .backends
        .iter()
        .map(|(name, backend)| (name.clone(), file_storage_backend_options(backend)))
        .collect::<HashMap<_, _>>();

    FileStorageOptions {
        upload_token_secret: config.file_storage.upload_token_secret.clone(),
        default_backend: config.file_storage.default_backend.clone(),
        chat_attachments_backend: config.file_storage.chat_attachments_backend.clone(),
        user_avatars_backend: config.file_storage.user_avatars_backend.clone(),
        media_covers_backend: config.file_storage.media_covers_backend.clone(),
        room_covers_backend: config.file_storage.room_covers_backend.clone(),
        playlist_covers_backend: config.file_storage.playlist_covers_backend.clone(),
        backends,
    }
}

fn password_complexity_options(config: &AppConfig) -> PasswordComplexityOptions {
    PasswordComplexityOptions {
        min_length: config.password_complexity.min_length,
        require_uppercase: config.password_complexity.require_uppercase,
        require_lowercase: config.password_complexity.require_lowercase,
        require_digit: config.password_complexity.require_digit,
        require_special: config.password_complexity.require_special,
        max_repeated_chars: config.password_complexity.max_repeated_chars,
        zxcvbn_enabled: config.password_complexity.zxcvbn_enabled,
        zxcvbn_min_score: config.password_complexity.zxcvbn_min_score,
    }
}

fn passkey_options(config: &AppConfig) -> PasskeyServiceOptions {
    PasskeyServiceOptions {
        enabled: config.webauthn.enabled,
        rp_id: config.webauthn.rp_id.clone(),
        rp_origin: config.webauthn.rp_origin.clone(),
        rp_name: config.webauthn.rp_name.clone(),
        allowed_origins: config.webauthn.allowed_origins.clone(),
        allow_subdomains: config.webauthn.allow_subdomains,
        allow_any_port: config.webauthn.allow_any_port,
        timeout_seconds: config.webauthn.timeout_seconds,
        enumeration_protection_secret: config.security.webauthn_enumeration_key.clone(),
    }
}

pub fn core_services_options(config: &AppConfig) -> CoreServicesOptions {
    CoreServicesOptions {
        security: security_options(config),
        media_providers: media_providers_options(config),
        cluster_runtime_enabled: config.cluster_runtime_enabled(),
        redis: redis_connection_options(config),
        cache: cache_options(config),
        messaging_rate_limits: messaging_rate_limit_options(config),
        transport_compression_enabled: config.server.grpc_compression_enabled,
        file_storage: file_storage_options(config),
        jwt: jwt_options(config),
        password_complexity: password_complexity_options(config),
        passkey: passkey_options(config),
    }
}

pub fn connection_limits_options(config: &AppConfig) -> ConnectionLimitsOptions {
    ConnectionLimitsOptions {
        max_per_user: config.connection_limits.max_per_user,
        max_per_room: config.connection_limits.max_per_room,
        max_total: config.connection_limits.max_total,
        idle_timeout_seconds: config.connection_limits.idle_timeout_seconds,
        max_duration_seconds: config.connection_limits.max_duration_seconds,
        ws_message_rate_limit_per_second: config.connection_limits.ws_message_rate_limit_per_second,
    }
}

pub fn leader_runtime_options(config: &AppConfig) -> anyhow::Result<LeaderRuntimeOptions> {
    let options = LeaderRuntimeOptions {
        cluster_enabled: config.cluster.enabled,
        leader_election_mode: match config.cluster.leader_election_mode {
            ClusterLeaderElectionMode::Redis => LeaderElectionMode::Redis,
            ClusterLeaderElectionMode::K8sLease => LeaderElectionMode::K8sLease,
        },
        redis_deployment_mode: match config.redis.deployment_mode {
            AppRedisDeploymentMode::Standalone => LeaderRedisDeploymentMode::Standalone,
            AppRedisDeploymentMode::Sentinel => LeaderRedisDeploymentMode::Sentinel,
        },
        #[cfg(feature = "k8s")]
        k8s_lease: k8s_leader_runtime_options(config)?,
    };

    Ok(options)
}

#[cfg(feature = "k8s")]
fn k8s_leader_runtime_options(
    config: &AppConfig,
) -> anyhow::Result<Option<K8sLeaderRuntimeOptions>> {
    if !matches!(
        config.cluster.leader_election_mode,
        ClusterLeaderElectionMode::K8sLease
    ) {
        return Ok(None);
    }

    Ok(Some(K8sLeaderRuntimeOptions {
        pod_name: required_env("POD_NAME", "cluster.leader_election_mode='k8s_lease'")?,
        namespace: required_env("POD_NAMESPACE", "cluster.leader_election_mode='k8s_lease'")?,
    }))
}

#[cfg(feature = "k8s")]
fn required_env(name: &str, owner: &str) -> anyhow::Result<String> {
    let value =
        std::env::var(name).map_err(|_| anyhow::anyhow!("{owner} requires {name} to be set"))?;
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(anyhow::anyhow!(
            "{owner} requires {name} to contain a value"
        ));
    }
    Ok(value)
}

pub fn management_runtime_settings(config: &AppConfig) -> ManagementRuntimeSettings {
    ManagementRuntimeSettings {
        transport: match config.management.transport {
            AppManagementTransport::Tcp => ManagementTransport::Tcp,
            AppManagementTransport::Unix => ManagementTransport::Unix,
        },
        port: config.management.port,
        unix_socket_path: config.management.unix_socket_path.clone(),
        auth_token: config.management.auth_token.clone(),
        enable_reflection: config.management.enable_reflection,
        grpc_max_message_size_bytes: config.server.grpc_max_message_size_bytes,
        grpc_compression_enabled: config.server.grpc_compression_enabled,
        trusted_proxies: config.server.trusted_proxies.clone(),
    }
}

fn rate_limit_scope_strategy(strategy: AppRateLimitScopeStrategy) -> RateLimitScopeStrategy {
    match strategy {
        AppRateLimitScopeStrategy::FixedWindow => RateLimitScopeStrategy::FixedWindow,
        AppRateLimitScopeStrategy::Disabled => RateLimitScopeStrategy::Disabled,
    }
}

fn request_rate_limit_settings(config: &AppConfig) -> RequestRateLimitSettings {
    RequestRateLimitSettings {
        auth_max_requests: config.request_rate_limits.auth_max_requests,
        auth_window_seconds: config.request_rate_limits.auth_window_seconds,
        write_max_requests: config.request_rate_limits.write_max_requests,
        write_window_seconds: config.request_rate_limits.write_window_seconds,
        read_max_requests: config.request_rate_limits.read_max_requests,
        read_window_seconds: config.request_rate_limits.read_window_seconds,
        media_max_requests: config.request_rate_limits.media_max_requests,
        media_window_seconds: config.request_rate_limits.media_window_seconds,
        admin_max_requests: config.request_rate_limits.admin_max_requests,
        admin_window_seconds: config.request_rate_limits.admin_window_seconds,
        streaming_max_requests: config.request_rate_limits.streaming_max_requests,
        streaming_window_seconds: config.request_rate_limits.streaming_window_seconds,
        websocket_max_requests: config.request_rate_limits.websocket_max_requests,
        websocket_window_seconds: config.request_rate_limits.websocket_window_seconds,
        scopes: config
            .request_rate_limits
            .scopes
            .iter()
            .map(|(name, rule)| {
                (
                    name.clone(),
                    RateLimitScopeRule {
                        max_requests: rule.max_requests,
                        window_seconds: rule.window_seconds,
                        strategy: rate_limit_scope_strategy(rule.strategy),
                    },
                )
            })
            .collect(),
    }
}

fn metrics_auth_mode(mode: AppMetricsAuthMode) -> MetricsAuthMode {
    match mode {
        AppMetricsAuthMode::BearerToken => MetricsAuthMode::BearerToken,
        AppMetricsAuthMode::Basic => MetricsAuthMode::Basic,
        AppMetricsAuthMode::Kubernetes => MetricsAuthMode::Kubernetes,
    }
}

fn metrics_runtime_settings(config: &AppConfig) -> MetricsRuntimeSettings {
    MetricsRuntimeSettings {
        enabled: config.metrics.enabled,
        auth: MetricsAuthSettings {
            mode: metrics_auth_mode(config.metrics.auth.mode),
            bearer_token: config.metrics.auth.bearer_token.clone(),
            basic_username: config.metrics.auth.basic_username.clone(),
            basic_password: config.metrics.auth.basic_password.clone(),
            kubernetes: MetricsKubernetesAuthSettings {
                audience: config.metrics.auth.kubernetes.audience.clone(),
                authentication_cache_ttl_seconds: config
                    .metrics
                    .auth
                    .kubernetes
                    .authentication_cache_ttl_seconds,
                authorization_cache_ttl_seconds: config
                    .metrics
                    .auth
                    .kubernetes
                    .authorization_cache_ttl_seconds,
            },
        },
    }
}

fn proxy_slice_cache_runtime_settings(config: &AppConfig) -> ProxySliceCacheRuntimeSettings {
    ProxySliceCacheRuntimeSettings {
        enabled: config.proxy_slice_cache.enabled,
        slice_size_bytes: config.proxy_slice_cache.slice_size_bytes,
        max_cache_size_bytes: config.proxy_slice_cache.max_cache_size_bytes,
        segment_ttl_seconds: config.proxy_slice_cache.segment_ttl_seconds,
        stale_max_age_seconds: config.proxy_slice_cache.stale_max_age_seconds,
        stale_while_revalidate: config.proxy_slice_cache.stale_while_revalidate,
        file_backend_enabled: config.proxy_slice_cache.file_backend_enabled,
        file_cache_dir: config.proxy_slice_cache.file_cache_dir.clone(),
        eviction_interval_seconds: config.proxy_slice_cache.eviction_interval_seconds,
        watermark_ratio: config.proxy_slice_cache.watermark_ratio,
    }
}

fn server_state_hls_storage_backend(
    backend: AppHlsStorageBackend,
) -> synctv_core::service::ServerStateHlsStorageBackend {
    match backend {
        AppHlsStorageBackend::Memory => synctv_core::service::ServerStateHlsStorageBackend::Memory,
        AppHlsStorageBackend::File => synctv_core::service::ServerStateHlsStorageBackend::File,
        AppHlsStorageBackend::SharedFile => {
            synctv_core::service::ServerStateHlsStorageBackend::SharedFile
        }
        AppHlsStorageBackend::S3 => synctv_core::service::ServerStateHlsStorageBackend::S3,
    }
}

fn server_state_runtime_params(
    config: &AppConfig,
) -> synctv_core::service::ServerStateRuntimeParams {
    synctv_core::service::ServerStateRuntimeParams {
        cluster_enabled: config.cluster_runtime_enabled(),
        advertise_api_address: config.advertise_api_address(),
        cluster: synctv_core::service::ServerStateClusterOptions {
            discovery_mode: config.cluster.discovery_mode.to_string(),
        },
        database: synctv_core::service::ServerStateDatabaseOptions {
            host: config.database.host.clone(),
            port: config.database.port,
            name: config.database.name.clone(),
            max_connections: config.database.max_connections,
            min_connections: config.database.min_connections,
            connect_timeout_seconds: config.database.connect_timeout_seconds,
            idle_timeout_seconds: config.database.idle_timeout_seconds,
            max_lifetime_seconds: config.database.max_lifetime_seconds,
            read_url: config.database.read_url.clone(),
            read_host: config.database.read_host.clone(),
            read_port: config.database.read_port,
        },
        redis: synctv_core::service::ServerStateRedisOptions {
            deployment_mode: redis_deployment_mode(&config.redis.deployment_mode),
            database: config.redis.database,
            key_prefix: config.redis.key_prefix.clone(),
            connect_timeout_seconds: config.redis.connect_timeout_seconds,
            response_timeout_seconds: config.redis.response_timeout_seconds,
            pipeline_buffer_size: config.redis.pipeline_buffer_size,
            sentinel_master_name: config.redis.sentinel_master_name.clone(),
            sentinel_addresses: config.redis.sentinel_addresses.clone(),
        },
        livestream: synctv_core::service::ServerStateLivestreamOptions {
            rtmp_port: config.livestream.rtmp_port,
            public_rtmp_host: config.livestream.public_rtmp_host.clone(),
            gop_cache_size: config.livestream.gop_cache_size,
            stream_timeout_seconds: config.livestream.stream_timeout_seconds,
            gop_cache_max_memory_mb: config.livestream.gop_cache_max_memory_mb,
            hls_storage: synctv_core::service::ServerStateHlsStorageOptions {
                backend: server_state_hls_storage_backend(config.livestream.hls_storage.backend()),
                path: config.livestream.hls_storage.path().to_string(),
                memory_max_mb: config.livestream.hls_storage.memory_max_mb(),
            },
        },
    }
}

pub fn api_runtime_settings(config: &AppConfig) -> ApiRuntimeSettings {
    ApiRuntimeSettings {
        server: ApiServerSettings {
            bind_address: config.api_address(),
            project_url: config.server.project_url.clone(),
            apple_app_ids: config.webauthn.apple_app_ids.clone(),
            android_apps: config
                .webauthn
                .android_apps
                .iter()
                .map(|app| synctv_api::AndroidAppAssociationSettings {
                    package_name: app.package_name.clone(),
                    sha256_cert_fingerprints: app.sha256_cert_fingerprints.clone(),
                })
                .collect(),
            trusted_proxies: config.server.trusted_proxies.clone(),
            cors_allowed_origins: config.server.cors_allowed_origins.clone(),
            grpc_max_message_size_bytes: config.server.grpc_max_message_size_bytes,
            grpc_compression_enabled: config.server.grpc_compression_enabled,
            enable_reflection: config.server.enable_reflection,
        },
        request_rate_limits: request_rate_limit_settings(config),
        metrics: metrics_runtime_settings(config),
        cluster_enabled: config.cluster_runtime_enabled(),
        cluster_secret_configured: !config.cluster.secret.is_empty(),
        livestream: LivestreamRuntimeSettings {
            rtmp_port: config.livestream.rtmp_port,
            public_rtmp_host: config.public_rtmp_host(),
            flv_max_connection_duration_seconds: config
                .livestream
                .flv_max_connection_duration_seconds,
            flv_write_timeout_seconds: config.livestream.flv_write_timeout_seconds,
        },
        webrtc: WebRtcRuntimeSettings {
            filter_private_ice_candidates: config.webrtc.filter_private_ice_candidates,
        },
        proxy_slice_cache: proxy_slice_cache_runtime_settings(config),
        cluster: ClusterRuntimeSettings {
            secret: config.cluster.secret.clone(),
        },
        redis: RedisRuntimeSettings {
            key_prefix: config.redis.key_prefix.clone(),
        },
        connection_limits: ConnectionLimitSettings {
            ws_message_rate_limit_per_second: config
                .connection_limits
                .ws_message_rate_limit_per_second,
        },
        server_state: server_state_runtime_params(config),
    }
}

pub fn hls_storage_backend(backend: AppHlsStorageBackend) -> HlsStorageBackend {
    match backend {
        AppHlsStorageBackend::Memory => HlsStorageBackend::Memory,
        AppHlsStorageBackend::File => HlsStorageBackend::File,
        AppHlsStorageBackend::SharedFile => HlsStorageBackend::SharedFile,
        AppHlsStorageBackend::S3 => HlsStorageBackend::S3,
    }
}

pub fn hls_s3_options(storage: &HlsStorageConfig) -> HlsS3Options {
    storage
        .s3()
        .map_or_else(HlsS3Options::default, |config| HlsS3Options {
            endpoint: config.endpoint.clone(),
            access_key_id: config.access_key_id.clone(),
            secret_access_key: config.secret_access_key.clone(),
            bucket: config.bucket.clone(),
            region: config.region.clone(),
            base_path: config.base_path.clone(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_logging_is_the_unscoped_output() {
        let options = logging_options(&AppConfig::default());

        assert_eq!(options.global.name, "global");
        assert!(options.global.targets.is_empty());
        assert!(options
            .components
            .iter()
            .all(|component| !component.targets.is_empty()));
    }

    #[test]
    fn cluster_logging_includes_application_bootstrap_target() {
        let options = logging_options(&AppConfig::default());
        let cluster = options
            .components
            .iter()
            .find(|component| component.name == "cluster")
            .expect("cluster logging component must exist");

        assert!(cluster
            .targets
            .iter()
            .any(|target| target == "synctv::bootstrap::cluster"));
    }

    #[test]
    fn independent_network_services_have_dedicated_logging_targets() {
        let options = logging_options(&AppConfig::default());
        let targets_for = |name: &str| {
            options
                .components
                .iter()
                .find(|component| component.name == name)
                .map(|component| component.targets.as_slice())
                .expect("logging component must exist")
        };

        assert!(targets_for("server").contains(&"synctv_api_http".to_string()));
        assert!(targets_for("metrics").contains(&"synctv_core::metrics".to_string()));
        assert!(targets_for("livestream").contains(&"synctv_livestream".to_string()));
        assert!(targets_for("livestream").contains(&"synctv_xiu".to_string()));
        assert!(targets_for("webrtc").contains(&"synctv_core::service::stun_server".to_string()));
    }
}
