use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

use synctv_core::bootstrap::RedisHandles;
use synctv_core::Config;
use synctv_cluster::sync::{ConnectionManager, ClusterManager};
use synctv_cluster::discovery::{NodeRegistry, HealthMonitor, LoadBalancer, LoadBalancingStrategy};
#[cfg(feature = "k8s")]
use synctv_cluster::discovery::K8sDnsDiscovery;

/// Initialize the shared cluster components: `NodeRegistry`, heartbeat loop,
/// `HealthMonitor`, and `LoadBalancer`.
///
/// This is the common code shared between the "`k8s_dns`" and "redis" discovery
/// branches. Both modes use a Redis-backed `NodeRegistry` for health tracking.
pub async fn init_cluster_components(
    redis_handles: &RedisHandles,
    cm: &Arc<ClusterManager>,
    config: &Config,
    connection_manager: &ConnectionManager,
) -> (Option<Arc<NodeRegistry>>, Option<Arc<HealthMonitor>>, Option<Arc<LoadBalancer>>) {
    let node_id = cm.node_id().to_string();
    let heartbeat_timeout_secs: i64 = 30;

    let registry = match NodeRegistry::new(redis_handles.client.clone(), node_id.clone(), heartbeat_timeout_secs, &config.redis.key_prefix) {
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

/// Initialize cluster discovery infrastructure (`NodeRegistry` + `HealthMonitor` + `LoadBalancer`).
///
/// Supports two discovery modes:
///   "redis"   - Redis-based node registry (default)
///   "`k8s_dns`" - Kubernetes headless service DNS discovery
///
/// Returns (`NodeRegistry`, `HealthMonitor`, `LoadBalancer`, optional DNS refresh handle).
pub async fn init_cluster_discovery(
    config: &Config,
    redis_handles: &RedisHandles,
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

                    let (nr, hm, lb) = init_cluster_components(redis_handles, cm, config, connection_manager).await;

                    // Bridge: periodically merge DNS-discovered peers into the
                    // NodeRegistry so HealthMonitor/LoadBalancer see newly-scaled
                    // pods before they self-register via Redis heartbeat.
                    if let Some(ref registry) = nr {
                        let dns = k8s_discovery.clone();
                        let reg = registry.clone();
                        let bridge_cancel = cm.cancel_token();
                        tokio::spawn(async move {
                            let mut timer = tokio::time::interval(Duration::from_secs(15));
                            loop {
                                tokio::select! {
                                    () = bridge_cancel.cancelled() => {
                                        info!("K8s DNS -> NodeRegistry sync bridge shutting down");
                                        return;
                                    }
                                    _ = timer.tick() => {
                                        let dns_peers = dns.get_peers_as_node_info().await;
                                        if !dns_peers.is_empty() {
                                            reg.merge_dns_peers(dns_peers).await;
                                        }
                                    }
                                }
                            }
                        });
                        info!("K8s DNS -> NodeRegistry sync bridge started (15s interval)");
                    }

                    (nr, hm, lb, Some(dns_refresh_handle))
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

            let (nr, hm, lb) = init_cluster_components(redis_handles, cm, config, connection_manager).await;
            (nr, hm, lb, None)
        }
    }
}
