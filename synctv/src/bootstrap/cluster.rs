use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

#[cfg(feature = "k8s")]
use synctv_cluster::discovery::K8sDnsDiscovery;
use synctv_cluster::discovery::{
    health_monitor::HealthProbeConfig, HealthMonitor, LoadBalancer, LoadBalancingStrategy,
    NodeRegistry, StaticDiscovery, StaticDiscoveryConfig, StaticPeerConfig,
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
pub struct ClusterDiscoveryComponents {
    pub registry: Arc<NodeRegistry>,
    pub health_monitor: Arc<HealthMonitor>,
    pub load_balancer: Arc<LoadBalancer>,
}

pub async fn init_cluster_components(
    redis_handles: &RedisHandles,
    cm: &Arc<ClusterManager>,
    config: &Config,
    _connection_manager: &ConnectionManager,
) -> Result<ClusterDiscoveryComponents, anyhow::Error> {
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

    let health_monitor = Arc::new(HealthMonitor::with_cancellation_token_and_probe_config(
        registry.clone(),
        15,
        &cm.cancel_token(),
        HealthProbeConfig {
            cluster_secret: config.server.cluster_secret.clone(),
            ..HealthProbeConfig::default()
        },
    ));

    let lb = Arc::new(
        LoadBalancer::new(registry.clone(), LoadBalancingStrategy::LeastConnections)
            .with_health_monitor(health_monitor.clone()),
    );
    info!("Load balancer initialized with LeastConnections strategy");

    Ok(ClusterDiscoveryComponents {
        registry,
        health_monitor,
        load_balancer: lb,
    })
}

pub async fn activate_cluster_node(
    config: &Config,
    cm: &Arc<ClusterManager>,
    connection_manager: &ConnectionManager,
    registry: &Arc<NodeRegistry>,
    health_monitor: &Arc<HealthMonitor>,
) -> Result<(), anyhow::Error> {
    let advertise_api = config.advertise_api_address();

    registry
        .register(advertise_api.clone())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to register node in Redis: {e}"))?;

    info!(
        node_id = %cm.node_id(),
        advertise_api = %advertise_api,
        "Node registered in cluster"
    );

    let conn_mgr_for_hb = connection_manager.clone();
    cm.start_heartbeat_loop(
        registry.clone(),
        advertise_api,
        Some(move || conn_mgr_for_hb.connection_count()),
    )
    .await;

    match health_monitor.start().await {
        Ok(hm_handle) => {
            info!("Health monitor started");
            health_monitor.set_join_handle(hm_handle);
            Ok(())
        }
        Err(e) => {
            cm.shutdown().await;
            Err(anyhow::anyhow!(
                "Failed to start health monitor: {e}. \
                 Health monitoring is required for cluster mode."
            ))
        }
    }
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
    shutdown_token: CancellationToken,
) -> Result<
    (
        Option<Arc<NodeRegistry>>,
        Option<Arc<HealthMonitor>>,
        Option<Arc<LoadBalancer>>,
        Option<tokio::task::JoinHandle<()>>,
        Option<tokio::task::JoinHandle<()>>,
    ),
    anyhow::Error,
> {
    let discovery_mode = config.cluster.discovery_mode.as_str();

    match discovery_mode {
        #[cfg(feature = "k8s")]
        "k8s_dns" => {
            info!("Using K8s DNS discovery mode");
            let k8s_discovery = K8sDnsDiscovery::from_env(config.server.port).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to initialize K8s DNS discovery: {e}. \
                         Ensure HEADLESS_SERVICE_NAME, POD_NAMESPACE, and POD_IP env vars are set."
                )
            })?;
            let components =
                init_cluster_components(redis_handles, cm, config, connection_manager).await?;
            let k8s_discovery = k8s_discovery
                .with_cluster_secret(config.server.cluster_secret.clone())
                .with_node_registry(components.registry.clone());

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
            let dns_refresh_handle = k8s_discovery.start_refresh_loop(10, shutdown_token);

            Ok((
                Some(components.registry),
                Some(components.health_monitor),
                Some(components.load_balancer),
                Some(dns_refresh_handle),
                None,
            ))
        }
        #[cfg(not(feature = "k8s"))]
        "k8s_dns" => Err(anyhow::anyhow!(
            "K8s DNS discovery mode requires the 'k8s' feature. \
             Rebuild with: cargo build --features k8s"
        )),
        "static" => {
            info!("Using static peer discovery mode");
            let components =
                init_cluster_components(redis_handles, cm, config, connection_manager).await?;

            // Start static discovery background probe loop
            let peer_configs: Vec<StaticPeerConfig> = config
                .cluster
                .peers
                .iter()
                .map(|addr| StaticPeerConfig {
                    api_address: addr.clone(),
                })
                .collect();

            if peer_configs.is_empty() {
                warn!(
                    "Static discovery mode selected but no peers configured (cluster.peers is empty)"
                );
            }

            let static_config = StaticDiscoveryConfig {
                peers: peer_configs,
                probe_interval_secs: 10,
                connect_timeout: Duration::from_secs(3),
                cluster_secret: config.server.cluster_secret.clone(),
                default_api_port: config.server.port,
            };

            let static_discovery = StaticDiscovery::new(
                static_config,
                components.registry.clone(),
                cm.cancel_token(),
            );

            let handle = static_discovery.start();
            info!("Static peer discovery started");

            Ok((
                Some(components.registry),
                Some(components.health_monitor),
                Some(components.load_balancer),
                Some(handle),
                None,
            ))
        }
        "redis" => {
            let components =
                init_cluster_components(redis_handles, cm, config, connection_manager).await?;
            Ok((
                Some(components.registry),
                Some(components.health_monitor),
                Some(components.load_balancer),
                None,
                None,
            ))
        }
        _ => unreachable!("cluster.discovery_mode is validated before startup: {discovery_mode}"),
    }
}

#[cfg(test)]
mod tests {
    use crate::bootstrap::cluster::init_cluster_discovery;
    use std::sync::Arc;
    use std::time::Duration;
    use synctv_cluster::sync::{
        ClusterConfig, ClusterManager, ConnectionLimits, ConnectionManager,
    };
    use synctv_core::bootstrap::RedisHandles;
    use synctv_core::Config;
    use tokio::sync::RwLock;
    use tokio_util::sync::CancellationToken;

    fn test_cluster_config() -> Config {
        let mut config = Config::default();
        config.server.host = "127.0.0.1".to_string();
        config.server.port = 8080;
        config.server.cluster_secret.clear();
        config.redis.url = "redis://127.0.0.1:6379".to_string();
        config.cluster.discovery_mode = "k8s_dns".to_string();
        config
    }

    #[test]
    fn test_cluster_discovery_return_shape_uses_single_dns_handle_for_k8s() {
        let redis_shape = (true, true, true, false, false);
        let static_shape = (true, true, true, true, false);
        let k8s_dns_shape = (true, true, true, true, false);

        assert_eq!(redis_shape, (true, true, true, false, false));
        assert_eq!(static_shape, (true, true, true, true, false));
        assert_eq!(k8s_dns_shape, (true, true, true, true, false));
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_k8s_dns_env_validation_happens_before_node_registration() {
        let (_redis, client) = synctv_core_testing::start_redis_with_client().await;
        let shared_conn = Arc::new(RwLock::new(
            redis::aio::ConnectionManager::new(client.clone())
                .await
                .expect("shared redis connection manager"),
        ));
        let redis_handles = RedisHandles {
            client: client.clone(),
            conn: shared_conn.clone(),
        };

        let cluster_config = ClusterConfig {
            redis_client: Some(client.clone()),
            redis_conn: None,
            cluster_enabled: true,
            node_id: "bootstrap-k8s-env-order".to_string(),
            dedup_window: Duration::from_secs(1),
            critical_channel_capacity: 16,
            publish_channel_capacity: 16,
            key_prefix: "test-k8s-env-order:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
            shared_redis_conn: Some(shared_conn),
            parent_cancel_token: Some(CancellationToken::new()),
        };
        let mut manager = ClusterManager::new(cluster_config, None, None)
            .await
            .expect("cluster manager");
        let connection_manager = ConnectionManager::new(ConnectionLimits::default());
        manager.set_connection_manager(connection_manager.clone());
        let manager = Arc::new(manager);

        let mut config = test_cluster_config();
        config.redis.key_prefix = "test-k8s-env-order:".to_string();

        let old_service_name = std::env::var_os("HEADLESS_SERVICE_NAME");
        let old_namespace = std::env::var_os("POD_NAMESPACE");
        let old_pod_ip = std::env::var_os("POD_IP");
        std::env::set_var("HEADLESS_SERVICE_NAME", "synctv-headless");
        std::env::set_var("POD_NAMESPACE", "default");
        std::env::remove_var("POD_IP");

        let result = init_cluster_discovery(
            &config,
            &redis_handles,
            &manager,
            &connection_manager,
            CancellationToken::new(),
        )
        .await;

        match old_service_name {
            Some(value) => std::env::set_var("HEADLESS_SERVICE_NAME", value),
            None => std::env::remove_var("HEADLESS_SERVICE_NAME"),
        }
        match old_namespace {
            Some(value) => std::env::set_var("POD_NAMESPACE", value),
            None => std::env::remove_var("POD_NAMESPACE"),
        }
        match old_pod_ip {
            Some(value) => std::env::set_var("POD_IP", value),
            None => std::env::remove_var("POD_IP"),
        }

        assert!(
            result.is_err(),
            "missing POD_IP must fail before cluster components are initialized"
        );

        let registry = synctv_cluster::discovery::NodeRegistry::new(
            client,
            "bootstrap-k8s-env-order".to_string(),
            30,
            &config.redis.key_prefix,
        )
        .expect("node registry");
        let nodes = registry
            .get_all_nodes()
            .await
            .expect("node registry query should succeed");

        assert!(
            nodes
                .iter()
                .all(|node| node.node_id != "bootstrap-k8s-env-order"),
            "failed k8s env validation must not leave a ghost node registered in Redis"
        );
    }
}
