use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

#[cfg(feature = "k8s")]
use synctv_cluster::discovery::K8sDnsDiscovery;
use synctv_cluster::discovery::{
    HealthMonitor, LoadBalancer, LoadBalancingStrategy, NodeRegistry, StaticDiscovery,
    StaticDiscoveryConfig, StaticPeerConfig,
};
use synctv_cluster::sync::{ClusterManager, ConnectionManager};
use synctv_core::bootstrap::RedisHandles;
use synctv_core::Config;

/// Initialize the shared cluster components: `NodeRegistry`, heartbeat loop,
/// `HealthMonitor`, and `LoadBalancer`.
///
/// This is the common code shared between the "`k8s_dns`" and "redis" discovery
/// branches. Both modes use a Redis-backed `NodeRegistry` for health tracking.
///
/// # D1 fix: When cluster is explicitly enabled (`cluster.enabled = true`),
/// failures are treated as fatal and returned as `Err`. Previously, failures
/// silently returned `(None, None, None)`, leaving the node in a ghost state
/// where it believes it's in a cluster but has no registry or heartbeat.
pub async fn init_cluster_components(
    redis_handles: &RedisHandles,
    cm: &Arc<ClusterManager>,
    config: &Config,
    connection_manager: &ConnectionManager,
) -> Result<(Arc<NodeRegistry>, Arc<HealthMonitor>, Arc<LoadBalancer>), anyhow::Error> {
    let node_id = cm.node_id().to_string();
    let heartbeat_timeout_secs: i64 = 30;

    let registry = NodeRegistry::new(
        redis_handles.client.clone(),
        node_id.clone(),
        heartbeat_timeout_secs,
        &config.redis.key_prefix,
    )
    .map(Arc::new)
    .map_err(|e| anyhow::anyhow!("Failed to create NodeRegistry: {e}"))?;

    let advertise_grpc = config.advertise_grpc_address();
    let advertise_http = config.advertise_http_address();

    registry
        .register(advertise_grpc.clone(), advertise_http.clone())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to register node in Redis: {e}"))?;

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
    )
    .await;

    let health_monitor = Arc::new(HealthMonitor::new(registry.clone(), 15));
    match health_monitor.start().await {
        Ok(hm_handle) => {
            info!("Health monitor started");
            health_monitor.set_join_handle(hm_handle);
        }
        Err(e) => {
            // P1 fix: Clean up resources on health monitor start failure
            // 1. Stop the heartbeat loop via cancellation token
            cm.cancel_token().cancel();
            // 2. Unregister the node from Redis
            if let Err(unreg_err) = registry.unregister().await {
                warn!(
                    error = %unreg_err,
                    "Failed to unregister node during health monitor startup failure cleanup"
                );
            }
            return Err(anyhow::anyhow!(
                "Failed to start health monitor: {e}. \
                 Health monitoring is required for cluster mode."
            ));
        }
    }

    let lb = Arc::new(
        LoadBalancer::new(registry.clone(), LoadBalancingStrategy::LeastConnections)
            .with_health_monitor(health_monitor.clone()),
    );
    info!("Load balancer initialized with LeastConnections strategy");

    Ok((registry, health_monitor, lb))
}

/// Initialize cluster discovery infrastructure (`NodeRegistry` + `HealthMonitor` + `LoadBalancer`).
///
/// Supports two discovery modes:
///   "redis"   - Redis-based node registry (default)
///   "`k8s_dns`" - Kubernetes headless service DNS discovery
///
/// Returns (`NodeRegistry`, `HealthMonitor`, `LoadBalancer`, optional DNS refresh handle).
///
/// # D1 fix: Returns `Result` instead of silently degrading to `(None, None, None, None)`.
/// When cluster mode is explicitly enabled, any failure is propagated to the caller
/// as a fatal error, preventing the node from running in a ghost state.
pub async fn init_cluster_discovery(
    config: &Config,
    redis_handles: &RedisHandles,
    cm: &Arc<ClusterManager>,
    connection_manager: &ConnectionManager,
) -> Result<
    (
        Option<Arc<NodeRegistry>>,
        Option<Arc<HealthMonitor>>,
        Option<Arc<LoadBalancer>>,
        Option<tokio::task::JoinHandle<()>>,
    ),
    anyhow::Error,
> {
    let discovery_mode = config.cluster.discovery_mode.as_str();

    match discovery_mode {
        #[cfg(feature = "k8s")]
        "k8s_dns" => {
            info!("Using K8s DNS discovery mode");
            let k8s_discovery =
                K8sDnsDiscovery::from_env(config.server.grpc_port, config.server.http_port)
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "Failed to initialize K8s DNS discovery: {e}. \
                             Ensure HEADLESS_SERVICE_NAME and POD_NAMESPACE env vars are set."
                        )
                    })?;

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

            let (registry, hm, lb) =
                init_cluster_components(redis_handles, cm, config, connection_manager).await?;

            // Bridge: periodically merge DNS-discovered peers into the
            // NodeRegistry so HealthMonitor/LoadBalancer see newly-scaled
            // pods before they self-register via Redis heartbeat.
            {
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

            Ok((Some(registry), Some(hm), Some(lb), Some(dns_refresh_handle)))
        }
        #[cfg(not(feature = "k8s"))]
        "k8s_dns" => Err(anyhow::anyhow!(
            "K8s DNS discovery mode requires the 'k8s' feature. \
             Rebuild with: cargo build --features k8s"
        )),
        "static" => {
            info!("Using static peer discovery mode");
            let (registry, hm, lb) =
                init_cluster_components(redis_handles, cm, config, connection_manager).await?;

            // Start static discovery background probe loop
            let peer_configs: Vec<StaticPeerConfig> = config
                .cluster
                .peers
                .iter()
                .map(|addr| StaticPeerConfig {
                    grpc_address: addr.clone(),
                    http_address: None,
                })
                .collect();

            if peer_configs.is_empty() {
                warn!("Static discovery mode selected but no peers configured (cluster.peers is empty)");
            }

            let static_config = StaticDiscoveryConfig {
                peers: peer_configs,
                probe_interval_secs: 10,
                connect_timeout: Duration::from_secs(3),
                cluster_secret: config.server.cluster_secret.clone(),
                default_http_port: config.server.http_port,
            };

            let static_discovery =
                StaticDiscovery::new(static_config, registry.clone(), cm.cancel_token());

            let handle = static_discovery.start();
            info!("Static peer discovery started");

            Ok((Some(registry), Some(hm), Some(lb), Some(handle)))
        }
        "redis" => {
            let (registry, hm, lb) =
                init_cluster_components(redis_handles, cm, config, connection_manager).await?;
            Ok((Some(registry), Some(hm), Some(lb), None))
        }
        _ => unreachable!("cluster.discovery_mode is validated before startup: {discovery_mode}"),
    }
}
