mod migrations;
mod rtmp_auth;
mod server;

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

use synctv_core::{
    logging,
    bootstrap::{load_config, init_database, init_services, bootstrap_root_user},
    provider::{AlistProvider, BilibiliProvider, EmbyProvider},
    Config,
};
use synctv_cluster::sync::{ConnectionManager, ConnectionLimits, ClusterManager, ClusterConfig};
use synctv_cluster::discovery::{NodeRegistry, HealthMonitor, LoadBalancer, LoadBalancingStrategy};
#[cfg(feature = "k8s")]
use synctv_cluster::discovery::K8sDnsDiscovery;
use synctv_cluster::leader::LeaderElector;
#[cfg(feature = "k8s")]
use synctv_cluster::leader::{K8sLeaderElector, K8sLeaderElectorConfig};

use server::{SyncTvServer, Services};

/// Adapter that implements `PlaybackBroadcaster` by delegating to `ClusterManager`.
struct ClusterPlaybackBroadcaster {
    cluster_manager: Arc<ClusterManager>,
}

impl synctv_core::service::PlaybackBroadcaster for ClusterPlaybackBroadcaster {
    fn broadcast_playback_state(&self, state: &synctv_core::models::RoomPlaybackState) {
        let event = synctv_cluster::sync::ClusterEvent::PlaybackStateChanged {
            event_id: nanoid::nanoid!(16),
            room_id: state.room_id.clone(),
            // For system-initiated broadcasts (auto-play, reset), use a sentinel user_id.
            // The consumer in messaging.rs only reads the state payload, not the user fields.
            user_id: synctv_core::models::UserId::from_string("system".to_string()),
            username: "system".to_string(),
            state: state.clone(),
            timestamp: chrono::Utc::now(),
        };
        let _ = self.cluster_manager.broadcast(event);
    }
}

/// Generate a unique node ID for this server instance.
/// Prefers the POD_NAME environment variable (set by Kubernetes downward API)
/// for predictable, consistent node IDs in K8s deployments.
/// Falls back to hostname + local IP + random suffix for non-K8s environments.
fn generate_node_id() -> String {
    // In Kubernetes, POD_NAME is injected via the downward API and provides
    // a stable, predictable identifier (e.g. "synctv-0", "synctv-abc123")
    if let Ok(pod_name) = std::env::var("POD_NAME") {
        if !pod_name.is_empty() {
            return pod_name;
        }
    }

    use std::net::UdpSocket;

    // Try to get hostname, fallback to "unknown"
    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());

    // Get local IP address if available
    let local_ip = UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| s.connect("8.8.8.8:80").map(|()| s))
        .and_then(|s| s.local_addr()).map_or_else(|_| "0.0.0.0".to_string(), |addr| addr.ip().to_string());

    // Add random suffix for uniqueness
    let suffix = nanoid::nanoid!(6);

    format!("{hostname}_{local_ip}-{suffix}")
}

/// Initialize the shared cluster components: NodeRegistry, heartbeat loop,
/// HealthMonitor, and LoadBalancer.
///
/// This is the common code shared between the "k8s_dns" and "redis" discovery
/// branches. Both modes use a Redis-backed NodeRegistry for health tracking.
async fn init_cluster_components(
    redis_url: &str,
    cm: &Arc<ClusterManager>,
    config: &Config,
    connection_manager: &ConnectionManager,
) -> (Option<Arc<NodeRegistry>>, Option<Arc<HealthMonitor>>, Option<Arc<LoadBalancer>>) {
    let node_id = cm.node_id().to_string();
    let heartbeat_timeout_secs: i64 = 30;

    let registry = match NodeRegistry::new(Some(redis_url.to_string()), node_id.clone(), heartbeat_timeout_secs, &config.redis.key_prefix) {
        Ok(r) => Arc::new(r),
        Err(e) => {
            warn!("Failed to create NodeRegistry: {}", e);
            return (None, None, None);
        }
    };

    let advertise_grpc = config.advertise_grpc_address();
    let advertise_http = config.advertise_http_address();

    if let Err(e) = registry.register(advertise_grpc.clone(), advertise_http.clone()).await {
        warn!("Failed to register node in Redis: {}", e);
        return (None, None, None);
    }

    info!(
        node_id = %node_id,
        advertise_grpc = %advertise_grpc,
        advertise_http = %advertise_http,
        "Node registered in cluster"
    );

    let conn_mgr_for_hb = connection_manager.clone();
    cm.start_heartbeat_loop(
        registry.clone(),
        advertise_grpc,
        advertise_http,
        Some(move || conn_mgr_for_hb.connection_count()),
    ).await;

    let health_monitor = Arc::new(HealthMonitor::new(registry.clone(), 15));
    match health_monitor.start().await {
        Ok(hm_handle) => {
            info!("Health monitor started");
            health_monitor.set_join_handle(hm_handle);
        }
        Err(e) => {
            warn!("Failed to start health monitor: {}", e);
        }
    }

    let lb = Arc::new(
        LoadBalancer::new(registry.clone(), LoadBalancingStrategy::LeastConnections)
            .with_health_monitor(health_monitor.clone())
    );
    info!("Load balancer initialized with LeastConnections strategy");

    (Some(registry), Some(health_monitor), Some(lb))
}

/// Initialize cluster discovery infrastructure (NodeRegistry + HealthMonitor + LoadBalancer).
///
/// Supports two discovery modes:
///   "redis"   - Redis-based node registry (default)
///   "k8s_dns" - Kubernetes headless service DNS discovery
///
/// Returns (NodeRegistry, HealthMonitor, LoadBalancer, optional DNS refresh handle).
async fn init_cluster_discovery(
    config: &Config,
    cm: &Arc<ClusterManager>,
    connection_manager: &ConnectionManager,
) -> (Option<Arc<NodeRegistry>>, Option<Arc<HealthMonitor>>, Option<Arc<LoadBalancer>>, Option<tokio::task::JoinHandle<()>>) {
    let discovery_mode = config.cluster.discovery_mode.as_str();

    match discovery_mode {
        #[cfg(feature = "k8s")]
        "k8s_dns" => {
            info!("Using K8s DNS discovery mode");
            match K8sDnsDiscovery::from_env(config.server.grpc_port, config.server.http_port) {
                Ok(k8s_discovery) => {
                    // Perform initial DNS resolution
                    if let Err(e) = k8s_discovery.refresh().await {
                        warn!("Initial K8s DNS resolution failed (will retry): {}", e);
                    }
                    let peers = k8s_discovery.get_peers().await;
                    info!(
                        dns_name = %k8s_discovery.dns_name(),
                        peer_count = peers.len(),
                        "K8s DNS discovery initialized"
                    );

                    // Start background refresh loop (re-resolve every 10 seconds)
                    let dns_refresh_handle = k8s_discovery.start_refresh_loop(10).await;

                    // Still use Redis-based NodeRegistry for health monitoring if Redis is available.
                    // DNS discovers peers; Redis registry tracks heartbeats and health.
                    if !config.redis.url.is_empty() {
                        let (nr, hm, lb) = init_cluster_components(&config.redis.url, cm, config, connection_manager).await;

                        // Bridge: periodically merge DNS-discovered peers into the
                        // NodeRegistry so HealthMonitor/LoadBalancer see newly-scaled
                        // pods before they self-register via Redis heartbeat.
                        if let Some(ref registry) = nr {
                            let dns = k8s_discovery.clone();
                            let reg = registry.clone();
                            tokio::spawn(async move {
                                let mut timer = tokio::time::interval(Duration::from_secs(15));
                                loop {
                                    timer.tick().await;
                                    let dns_peers = dns.get_peers_as_node_info().await;
                                    if !dns_peers.is_empty() {
                                        reg.merge_dns_peers(dns_peers).await;
                                    }
                                }
                            });
                            info!("K8s DNS -> NodeRegistry sync bridge started (15s interval)");
                        }

                        (nr, hm, lb, Some(dns_refresh_handle))
                    } else {
                        // K8s DNS mode without Redis: DNS resolution works, but there is no
                        // NodeRegistry, HealthMonitor, or LoadBalancer. Cluster coordination
                        // (pub/sub, heartbeats, load balancing) requires Redis even in k8s_dns mode.
                        warn!(
                            "K8s DNS discovery is active but Redis is not configured. \
                             DNS will resolve peers, but cluster health monitoring, load balancing, \
                             and pub/sub are DISABLED. To enable full cluster functionality, \
                             configure a Redis URL. See: https://docs.synctv.dev/deployment/k8s"
                        );
                        (None, None, None, Some(dns_refresh_handle))
                    }
                }
                Err(e) => {
                    error!("Failed to initialize K8s DNS discovery: {}", e);
                    error!("Ensure HEADLESS_SERVICE_NAME and POD_NAMESPACE env vars are set");
                    (None, None, None, None)
                }
            }
        }
        #[cfg(not(feature = "k8s"))]
        "k8s_dns" => {
            error!(
                "K8s DNS discovery mode requires the 'k8s' feature. \
                 Rebuild with: cargo build --features k8s"
            );
            (None, None, None, None)
        }
        _ => {
            // Default: Redis-based discovery
            if discovery_mode != "redis" {
                warn!(
                    discovery_mode = %discovery_mode,
                    "Unknown discovery mode, falling back to 'redis'"
                );
            }

            if !config.redis.url.is_empty() {
                let (nr, hm, lb) = init_cluster_components(&config.redis.url, cm, config, connection_manager).await;
                (nr, hm, lb, None)
            } else {
                info!("Redis not configured, cluster discovery disabled");
                (None, None, None, None)
            }
        }
    }
}

/// Initialize livestream components (RTMP server and live streaming infrastructure).
///
/// Returns the `LivestreamState` handle (for graceful shutdown) and the shared
/// `LiveStreamingInfrastructure` (passed to gRPC/HTTP servers).
async fn init_livestream(
    config: &Config,
    synctv_services: &synctv_core::bootstrap::services::Services,
    node_id: &str,
) -> Result<(Option<server::LivestreamState>, Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>)> {
    if config.redis.url.is_empty() {
        info!("Redis not configured, livestream features disabled");
        return Ok((None, None));
    }

    info!("Initializing livestream infrastructure...");

    let redis_client = match redis::Client::open(config.redis.url.clone()) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to create Redis client for livestream: {}", e);
            return Ok((None, None));
        }
    };

    let redis_conn = match redis_client.get_connection_manager().await {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to connect to Redis for livestream: {}", e);
            return Ok((None, None));
        }
    };

    // Publisher registry for Redis (used by PullStreamManager + PublisherManager)
    let publisher_registry = Arc::new(synctv_livestream::relay::StreamRegistry::new(redis_conn)) as Arc<dyn synctv_livestream::relay::StreamRegistryTrait>;

    // Shared tracker for user->stream mapping (kick-on-ban)
    let user_stream_tracker: synctv_livestream::api::UserStreamTracker =
        Arc::new(synctv_livestream::api::StreamTracker::new());

    // Stream lifecycle event channel (app-level logging)
    let (stream_lifecycle_tx, mut stream_lifecycle_rx) =
        tokio::sync::broadcast::channel::<rtmp_auth::StreamLifecycleEvent>(64);

    tokio::spawn(async move {
        while let Ok(event) = stream_lifecycle_rx.recv().await {
            match event {
                rtmp_auth::StreamLifecycleEvent::Started { room_id, media_id, user_id } => {
                    info!(
                        room_id = %room_id,
                        media_id = %media_id,
                        user_id = %user_id,
                        "Stream started"
                    );
                }
                rtmp_auth::StreamLifecycleEvent::Stopped { room_id, media_id, user_id } => {
                    info!(
                        room_id = %room_id,
                        media_id = %media_id,
                        user_id = %user_id,
                        "Stream stopped"
                    );
                }
            }
        }
    });

    // RTMP auth callback (needs synctv-core services)
    let rtmp_auth: Arc<dyn synctv_livestream::AuthCallback> =
        Arc::new(rtmp_auth::SyncTvRtmpAuth::new(
            synctv_services.room_service.clone(),
            synctv_services.user_service.clone(),
            synctv_services.publish_key_service.clone(),
            user_stream_tracker.clone(),
            publisher_registry.clone(),
            node_id.to_string(),
            Some(stream_lifecycle_tx),
        ));

    // One-shot facade: start all xiu components
    let rtmp_listen_addr = format!("{}:{}", config.server.host, config.livestream.rtmp_port);
    let handle = synctv_livestream::LivestreamServer::new(
        synctv_livestream::LivestreamConfig {
            rtmp_address: rtmp_listen_addr,
            gop_cache_size: config.livestream.gop_cache_size as usize,
            node_id: node_id.to_string(),
            cleanup_check_interval_seconds: config.livestream.cleanup_check_interval_seconds,
            stream_timeout_seconds: config.livestream.stream_timeout_seconds,
            cluster_secret: if config.server.cluster_secret.is_empty() {
                None
            } else {
                Some(config.server.cluster_secret.clone())
            },
            gop_cache_max_memory_mb: config.livestream.gop_cache_max_memory_mb,
        },
        publisher_registry,
        user_stream_tracker,
    )
    .with_auth(rtmp_auth)
    .start()
    .await
    .map_err(|e| anyhow::anyhow!("Failed to start livestream: {e}"))?;

    let live_infra = handle.infrastructure.clone();
    let state = Some(server::LivestreamState { handle });

    Ok((state, Some(live_infra)))
}

/// Initialize WebRTC components: SFU manager and STUN server.
async fn init_webrtc(
    config: &Config,
) -> (Option<Arc<synctv_sfu::SfuManager>>, Option<Arc<synctv_core::service::StunServer>>) {
    // STUN server (if enabled, powered by turn-rs)
    let stun_server = if config.webrtc.enable_builtin_stun {
        info!("Starting built-in STUN server (turn-rs)...");
        let bind_addr = format!("{}:{}", config.webrtc.stun_host, config.webrtc.stun_port);

        // Resolve external address with auto-detection fallback chain:
        // 1. Explicit config (stun_external_addr)
        // 2. advertise_host config / POD_IP
        // 3. Cloud metadata (AWS/GCP/Azure)
        let external_addr = if !config.webrtc.stun_external_addr.is_empty() {
            config.webrtc.stun_external_addr.clone()
        } else {
            let advertise = config.advertise_host();
            // Check if advertise_host resolved to something usable
            let candidate = format!("{advertise}:{}", config.webrtc.stun_port);
            if synctv_core::service::validate_external_addr(&candidate).is_ok() {
                candidate
            } else {
                // Try auto-detecting from cloud metadata
                info!("advertise_host '{}' is not a routable external IP, attempting cloud metadata detection...", advertise);
                match synctv_core::service::resolve_external_ip().await {
                    Some(ip) => format!("{ip}:{}", config.webrtc.stun_port),
                    None => {
                        warn!(
                            "Could not auto-detect external IP for STUN server. \
                             Falling back to '{}'. Set SYNCTV_WEBRTC_STUN_EXTERNAL_ADDR \
                             or STUN_EXTERNAL_IP to a public IP for NAT traversal to work.",
                            candidate
                        );
                        candidate
                    }
                }
            }
        };

        // Validate the final external address
        if let Err(e) = synctv_core::service::validate_external_addr(&external_addr) {
            warn!("{}", e);
            warn!("STUN server will start but NAT traversal may not work correctly");
        }

        let stun_config = synctv_core::service::StunServerConfig {
            bind_addr,
            external_addr,
        };
        match synctv_core::service::StunServer::start(stun_config).await {
            Ok(server) => {
                info!("Built-in STUN server started on {}", server.local_addr());
                Some(server)
            }
            Err(e) => {
                warn!("Failed to start STUN server: {}", e);
                warn!("WebRTC P2P connectivity may be limited without STUN");
                None
            }
        }
    } else {
        info!("Built-in STUN server disabled");
        None
    };

    // SFU manager (if needed for WebRTC mode)
    let sfu_manager = if config.webrtc.mode == synctv_core::config::WebRTCMode::SFU
        || config.webrtc.mode == synctv_core::config::WebRTCMode::Hybrid
    {
        info!("Initializing SFU manager for mode: {:?}", config.webrtc.mode);
        let sfu_config = synctv_sfu::SfuConfig::from_webrtc_fields(
            config.webrtc.sfu_threshold,
            config.webrtc.max_sfu_rooms,
            config.webrtc.max_peers_per_sfu_room,
            config.webrtc.enable_simulcast,
            config.webrtc.enable_bandwidth_estimation,
        );
        let manager = synctv_sfu::SfuManager::new(sfu_config);
        info!(
            "SFU manager initialized (threshold: {}, max_rooms: {}, max_peers_per_room: {})",
            config.webrtc.sfu_threshold,
            if config.webrtc.max_sfu_rooms == 0 { "unlimited".to_string() } else { config.webrtc.max_sfu_rooms.to_string() },
            config.webrtc.max_peers_per_sfu_room
        );
        Some(manager)
    } else {
        info!("SFU manager disabled (mode: {:?})", config.webrtc.mode);
        None
    };

    (sfu_manager, stun_server)
}

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Load configuration (load_config already calls validate())
    let config = load_config()?;

    // 1.5. Generate node_id once for the entire process
    let node_id = generate_node_id();

    // 2. Initialize logging
    // Hold the guard so buffered log entries are flushed on shutdown
    let _log_guard = logging::init_logging(&config.logging)?;
    info!("SyncTV server starting...");
    info!("gRPC address: {}", config.grpc_address());
    info!("HTTP address: {}", config.http_address());

    // 3. Initialize database
    let pool = init_database(&config).await?;

    // 4. Run migrations (with distributed lock if Redis is available)
    migrations::run_migrations(&pool, &config.redis.url).await?;

    // 4.3. Bootstrap root user (if enabled and no root user exists)
    info!("Checking root user bootstrap...");
    if let Err(e) = bootstrap_root_user(&pool, &config.bootstrap, config.server.development_mode).await {
        warn!("Failed to bootstrap root user: {}", e);
        warn!("You may need to manually create a root user");
        // Non-fatal: continue startup even if bootstrap fails
    }

    // 4.5. Initialize audit log partitions
    info!("Initializing audit log partitions...");
    synctv_core::service::ensure_audit_partitions_on_startup(&pool)
        .await
        .map_err(|e| {
            warn!("Failed to initialize audit partitions (non-fatal): {}", e);
            // Non-fatal: continue startup even if partition creation fails
            e
        })
        .ok();

    // Start automatic partition management (check every 24 hours)
    let partition_cancel = tokio_util::sync::CancellationToken::new();
    let audit_manager = synctv_core::service::AuditPartitionManager::new(pool.clone());
    let _audit_task = audit_manager.start_auto_management(24, partition_cancel.clone());
    info!("Audit log partition management started");

    // 4.7. Initialize CacheInvalidationService early (before init_services)
    // This is the single source of truth for cache invalidation across the entire
    // application. It uses the cluster node_id so invalidation messages are
    // correctly attributed to this replica.
    let redis_client_for_cache = if config.redis.url.is_empty() {
        None
    } else {
        redis::Client::open(config.redis.url.clone()).ok()
    };
    let key_builder = synctv_core::cache::KeyBuilder::from_config(&config);
    let cache_invalidation_service = Arc::new(
        synctv_core::cache::CacheInvalidationService::new(
            redis_client_for_cache,
            node_id.clone(),
            key_builder.cache_invalidation_stream(),
        ),
    );

    // 5. Initialize services (needed for settings_registry)
    let mut synctv_services = init_services(pool.clone(), &config, cache_invalidation_service.clone()).await?;

    // Start the cache invalidation Redis subscriber (exactly once, after all
    // components have registered their invalidation callbacks via init_services).
    if let Err(e) = cache_invalidation_service.start().await {
        warn!("Failed to start cache invalidation listener: {}", e);
    }

    // 4.6. Initialize chat message partitions with dynamic granularity
    info!("Initializing chat message partitions...");
    synctv_core::service::ensure_chat_partitions_on_startup(
        &pool,
        synctv_services.settings_registry.clone()
    )
        .await
        .map_err(|e| {
            warn!("Failed to initialize chat partitions (non-fatal): {}", e);
            e
        })
        .ok();

    // Start automatic chat partition management (time-based)
    // Check interval: 24 hours (only manages partitions, not per-room limits)
    let chat_partition_manager = synctv_core::service::ChatPartitionManager::new(
        pool.clone(),
        synctv_services.settings_registry.clone()
    );
    let _chat_partition_task = chat_partition_manager.start_auto_management(24, partition_cancel.clone());
    info!("Chat message partition management started (check interval: 24 hours)");

    // Start periodic data cleanup (soft-deleted records, expired tokens, old notifications)
    let cleanup_service = synctv_core::service::CleanupService::new(
        pool.clone(),
        synctv_core::service::CleanupConfig::default(),
    );
    let _cleanup_task = cleanup_service.start_periodic(24, partition_cancel.clone());
    info!("Periodic data cleanup started (interval: 24 hours)");

    // 5.5. Initialize leader election for singleton operations.
    // Only one replica at a time becomes the leader and runs singleton tasks
    // (database migrations are already gated by DistributedLock; leader election
    // provides a continuous leadership signal for periodic cleanup tasks).
    let leader_cancel = tokio_util::sync::CancellationToken::new();
    let leader_elector = {
        let leader_mode = config.cluster.leader_election_mode.as_str();
        match leader_mode {
            #[cfg(feature = "k8s")]
            "k8s_lease" => {
                info!("Using K8s Lease-based leader election");
                // Get pod identity and namespace from environment
                let pod_name = std::env::var("POD_NAME").unwrap_or_else(|_| node_id.clone());
                let namespace = std::env::var("POD_NAMESPACE").unwrap_or_else(|_| "default".to_string());

                match K8sLeaderElector::new(
                    pod_name.clone(),
                    namespace.clone(),
                    K8sLeaderElectorConfig::default(),
                ).await {
                    Ok(elector) => {
                        let _leader_handle = elector.start(leader_cancel.clone());
                        info!(
                            pod_name = %pod_name,
                            namespace = %namespace,
                            "K8s leader election started"
                        );
                        Some(synctv_cluster::leader::AnyLeaderElector::K8s(elector))
                    }
                    Err(e) => {
                        error!("Failed to initialize K8s leader election: {}", e);
                        error!("Ensure POD_NAME and POD_NAMESPACE env vars are set and RBAC is configured");
                        None
                    }
                }
            }
            #[cfg(not(feature = "k8s"))]
            "k8s_lease" => {
                error!(
                    "K8s Lease-based leader election requires the 'k8s' feature. \
                     Rebuild with: cargo build --features k8s"
                );
                None
            }
            _ => {
                // Default: Redis-based leader election
                if leader_mode != "redis" {
                    warn!(
                        leader_election_mode = %leader_mode,
                        "Unknown leader election mode, falling back to 'redis'"
                    );
                }

                if let Some(ref conn) = synctv_services.redis_conn {
                    let elector = LeaderElector::new(conn.clone(), node_id.clone(), &config.redis.key_prefix);
                    let _leader_handle = elector.start(leader_cancel.clone());
                    info!("Redis-based leader election started (node_id={})", node_id);
                    Some(synctv_cluster::leader::AnyLeaderElector::Redis(elector))
                } else {
                    // Without Redis, this is a single-node deployment; we are always the leader.
                    info!("Redis not configured, skipping leader election (single-node mode)");
                    None
                }
            }
        }
    };

    // 6. Initialize connection manager with configurable limits (needed early for heartbeat loop)
    let connection_limits = ConnectionLimits {
        max_per_user: config.connection_limits.max_per_user,
        max_per_room: config.connection_limits.max_per_room,
        max_total: config.connection_limits.max_total,
        idle_timeout: Duration::from_secs(config.connection_limits.idle_timeout_seconds),
        max_duration: Duration::from_secs(config.connection_limits.max_duration_seconds),
    };
    let connection_manager = ConnectionManager::new(connection_limits);
    info!(
        max_per_user = config.connection_limits.max_per_user,
        max_per_room = config.connection_limits.max_per_room,
        max_total = config.connection_limits.max_total,
        "Connection manager initialized with configurable limits"
    );

    // Initialize ClusterManager (unified cluster management)
    // Extract permission service for cross-replica cache invalidation
    let permission_service = Some(synctv_services.room_service.permission_service().clone());

    let cluster_manager = if config.redis.url.is_empty() {
        info!("Redis not configured, using single-node ClusterManager");
        // Create a single-node ClusterManager (no Redis)
        let cluster_config = ClusterConfig {
            redis_url: String::new(),
            node_id: node_id.clone(),
            dedup_window: std::time::Duration::from_secs(10),
            cleanup_interval: std::time::Duration::from_secs(30),
            critical_channel_capacity: config.cluster.critical_channel_capacity,
            publish_channel_capacity: config.cluster.publish_channel_capacity,
        };
        match ClusterManager::new(cluster_config, None, None).await {
            Ok(manager) => Some(Arc::new(manager)),
            Err(e) => {
                error!("Failed to create single-node ClusterManager: {}", e);
                None
            }
        }
    } else {
        let cluster_config = ClusterConfig {
            redis_url: config.redis.url.clone(),
            node_id: node_id.clone(),
            dedup_window: std::time::Duration::from_secs(10),
            cleanup_interval: std::time::Duration::from_secs(30),
            critical_channel_capacity: config.cluster.critical_channel_capacity,
            publish_channel_capacity: config.cluster.publish_channel_capacity,
        };
        match ClusterManager::new(
            cluster_config,
            permission_service,
            Some((*cache_invalidation_service).clone()),
        ).await {
            Ok(manager) => {
                info!("ClusterManager initialized with cross-replica cache invalidation");
                Some(Arc::new(manager))
            }
            Err(e) => {
                error!("Failed to create ClusterManager: {}", e);
                error!("Continuing in single-node mode");
                None
            }
        }
    };

    // Wire cluster broadcaster into PlaybackService for cross-replica playback sync.
    // Arc::get_mut requires exclusive ownership (refcount == 1). This must run
    // before any code clones synctv_services.room_service.
    if let Some(ref cm) = cluster_manager {
        let room_svc = Arc::get_mut(&mut synctv_services.room_service)
            .ok_or_else(|| anyhow::anyhow!("BUG: room_service Arc has multiple owners during initialization"))?;
        room_svc.set_playback_cluster_broadcaster(Arc::new(ClusterPlaybackBroadcaster {
            cluster_manager: cm.clone(),
        }));
        info!("PlaybackService wired with cluster broadcaster");
    }

    // 7.5. Initialize cluster discovery infrastructure
    let (node_registry, health_monitor, load_balancer, dns_refresh_handle) = if let Some(ref cm) = cluster_manager {
        init_cluster_discovery(&config, cm, &connection_manager).await
    } else {
        (None, None, None, None)
    };

    // Note: Redis Pub/Sub is now handled by ClusterManager
    // We get the publish_tx from cluster_manager for backward compatibility
    let redis_publish_tx = cluster_manager
        .as_ref()
        .and_then(|cm| cm.redis_publish_tx().cloned());

    // 9. Initialize livestream components (RTMP server and live streaming infrastructure)
    let (livestream_state, live_streaming_infrastructure) =
        init_livestream(&config, &synctv_services, &node_id).await?;

    // 9.5-9.7. Initialize WebRTC components (STUN server and SFU manager)
    let (sfu_manager, stun_server) = init_webrtc(&config).await;

    // 10. Create server with all services
    let provider_instance_manager = synctv_services.provider_instance_manager.clone();
    let alist_provider = Arc::new(AlistProvider::new(provider_instance_manager.clone()));
    let bilibili_provider = Arc::new(BilibiliProvider::new(provider_instance_manager.clone()));
    let emby_provider = Arc::new(EmbyProvider::new(provider_instance_manager.clone()));

    let services = Services {
        user_service: synctv_services.user_service.clone(),
        room_service: synctv_services.room_service.clone(),
        jwt_service: synctv_services.jwt_service.clone(),
        cluster_manager,
        redis_publish_tx,
        rate_limiter: synctv_services.rate_limiter.clone(),
        rate_limit_config: synctv_services.rate_limit_config.clone(),
        content_filter: synctv_services.content_filter.clone(),
        connection_manager,
        providers_manager: synctv_services.providers_manager.clone(),
        provider_instance_manager,
        provider_instance_repository: synctv_services.provider_instance_repo.clone(),
        user_provider_credential_repository: synctv_services.user_provider_credential_repo.clone(),
        alist_provider,
        bilibili_provider,
        emby_provider,
        oauth2_service: synctv_services.oauth2_service.clone(),
        settings_service: synctv_services.settings_service.clone(),
        settings_registry: synctv_services.settings_registry.clone(),
        email_service: synctv_services.email_service.clone(),
        email_token_service: synctv_services.email_token_service.clone(),
        publish_key_service: synctv_services.publish_key_service.clone(),
        notification_service: Some(synctv_services.notification_service.clone()),
        audit_service: synctv_services.audit_service.clone(),
        live_streaming_infrastructure,
        stun_server,
        sfu_manager,
        node_registry,
        health_monitor,
        load_balancer,
        token_blacklist: synctv_services.token_blacklist.clone(),
        redis_conn: synctv_services.redis_conn.clone(),
        settings_cancel: synctv_services.settings_cancel.clone(),
        partition_cancel,
        leader_elector,
        leader_cancel,
        dns_refresh_abort: dns_refresh_handle.map(|h| h.abort_handle()),
    };

    let server = SyncTvServer::new(config, services, livestream_state, pool);

    // 11. Start all servers
    server.start().await?;

    Ok(())
}
