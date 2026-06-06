use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

#[cfg(feature = "k8s")]
use synctv_cluster::discovery::K8sDnsDiscovery;
use synctv_cluster::discovery::{
    health_monitor::HealthProbeConfig, ClusterHealthRuntime, ClusterNodeDirectory,
    ClusterNodeDirectoryFactory, HealthMonitor, StaticDiscovery, StaticDiscoveryConfig,
    StaticPeerConfig,
};
use synctv_core::{config::ClusterDiscoveryMode, Config};
use synctv_realtime::sync::{ConnectionRuntime, RealtimeManager, RealtimeMessageTransportFactory};

#[async_trait]
pub trait ClusterNodeActivator: Send + Sync {
    async fn activate(&self) -> Result<(), anyhow::Error>;
}

#[derive(Clone)]
pub struct DefaultClusterNodeActivator {
    config: Config,
    realtime_manager: Arc<RealtimeManager>,
    connection_manager: Arc<dyn ConnectionRuntime>,
    registry: Arc<dyn ClusterNodeDirectory>,
    health_monitor: Arc<dyn ClusterHealthRuntime>,
}

impl DefaultClusterNodeActivator {
    #[must_use]
    pub const fn new(
        config: Config,
        realtime_manager: Arc<RealtimeManager>,
        connection_manager: Arc<dyn ConnectionRuntime>,
        registry: Arc<dyn ClusterNodeDirectory>,
        health_monitor: Arc<dyn ClusterHealthRuntime>,
    ) -> Self {
        Self {
            config,
            realtime_manager,
            connection_manager,
            registry,
            health_monitor,
        }
    }
}

#[async_trait]
impl ClusterNodeActivator for DefaultClusterNodeActivator {
    async fn activate(&self) -> Result<(), anyhow::Error> {
        activate_cluster_node(
            &self.config,
            &self.realtime_manager,
            self.connection_manager.clone(),
            &self.registry,
            &self.health_monitor,
        )
        .await
    }
}

/// Initialize the shared cluster components: `NodeRegistry`, heartbeat loop,
/// and `HealthMonitor`.
///
/// This is the common code shared between the "`k8s_dns`" and "redis" discovery
/// branches. Both modes use a Redis-backed `NodeRegistry` for health tracking.
///
/// When cluster is explicitly enabled (`cluster.enabled = true`), failures are
/// treated as fatal and returned as `Err` so a node cannot run in a ghost state
/// with no registry or heartbeat.
pub struct ClusterDiscoveryComponents {
    pub registry: Arc<dyn ClusterNodeDirectory>,
    pub health_monitor: Arc<dyn ClusterHealthRuntime>,
}

pub struct ClusterDiscoveryTask {
    pub name: &'static str,
    pub handle: JoinHandle<()>,
}

impl ClusterDiscoveryTask {
    #[must_use]
    pub const fn new(name: &'static str, handle: JoinHandle<()>) -> Self {
        Self { name, handle }
    }
}

pub struct ClusterDiscoveryBundle {
    pub registry: Arc<dyn ClusterNodeDirectory>,
    pub health_monitor: Arc<dyn ClusterHealthRuntime>,
    pub background_tasks: Vec<ClusterDiscoveryTask>,
}

pub trait ClusterCoordinationProvider: Send + Sync {
    fn distributed_transport_factory(&self) -> Arc<dyn RealtimeMessageTransportFactory>;

    fn node_directory_factory(&self) -> Arc<dyn ClusterNodeDirectoryFactory>;
}

#[derive(Clone)]
struct RedisClusterCoordinationProvider {
    distributed_transport_factory: Arc<dyn RealtimeMessageTransportFactory>,
    node_directory_factory: Arc<dyn ClusterNodeDirectoryFactory>,
}

impl ClusterCoordinationProvider for RedisClusterCoordinationProvider {
    fn distributed_transport_factory(&self) -> Arc<dyn RealtimeMessageTransportFactory> {
        self.distributed_transport_factory.clone()
    }

    fn node_directory_factory(&self) -> Arc<dyn ClusterNodeDirectoryFactory> {
        self.node_directory_factory.clone()
    }
}

#[must_use]
pub fn build_cluster_coordination_provider(
    runtime: Arc<dyn synctv_core::RedisCoordinationRuntime>,
) -> Arc<dyn ClusterCoordinationProvider> {
    Arc::new(RedisClusterCoordinationProvider {
        distributed_transport_factory: synctv_realtime::build_realtime_message_transport_factory(
            runtime.clone(),
        ),
        node_directory_factory: synctv_cluster::build_cluster_node_directory_factory(runtime),
    })
}

impl ClusterDiscoveryBundle {
    #[must_use]
    pub fn new(
        components: ClusterDiscoveryComponents,
        background_tasks: Vec<ClusterDiscoveryTask>,
    ) -> Self {
        Self {
            registry: components.registry,
            health_monitor: components.health_monitor,
            background_tasks,
        }
    }
}

#[async_trait]
trait ClusterPeerDiscoveryDriver: Send + Sync {
    async fn start(
        self: Box<Self>,
        registry: Arc<dyn ClusterNodeDirectory>,
    ) -> Result<Vec<ClusterDiscoveryTask>, anyhow::Error>;
}

struct RedisClusterPeerDiscoveryDriver;

#[async_trait]
impl ClusterPeerDiscoveryDriver for RedisClusterPeerDiscoveryDriver {
    async fn start(
        self: Box<Self>,
        _registry: Arc<dyn ClusterNodeDirectory>,
    ) -> Result<Vec<ClusterDiscoveryTask>, anyhow::Error> {
        Ok(Vec::new())
    }
}

struct StaticClusterPeerDiscoveryDriver {
    config: StaticDiscoveryConfig,
    cancel_token: CancellationToken,
}

#[async_trait]
impl ClusterPeerDiscoveryDriver for StaticClusterPeerDiscoveryDriver {
    async fn start(
        self: Box<Self>,
        registry: Arc<dyn ClusterNodeDirectory>,
    ) -> Result<Vec<ClusterDiscoveryTask>, anyhow::Error> {
        let discovery = StaticDiscovery::from_runtime(self.config, registry, self.cancel_token);
        let handle = discovery.start();
        info!("Static peer discovery started");
        Ok(vec![ClusterDiscoveryTask::new("peer_discovery", handle)])
    }
}

#[cfg(feature = "k8s")]
struct K8sClusterPeerDiscoveryDriver {
    api_port: u16,
    cluster_secret: String,
    shutdown_token: CancellationToken,
}

#[cfg(feature = "k8s")]
#[async_trait]
impl ClusterPeerDiscoveryDriver for K8sClusterPeerDiscoveryDriver {
    async fn start(
        self: Box<Self>,
        registry: Arc<dyn ClusterNodeDirectory>,
    ) -> Result<Vec<ClusterDiscoveryTask>, anyhow::Error> {
        info!("Using K8s DNS discovery mode");
        let discovery = K8sDnsDiscovery::from_env(self.api_port).map_err(|e| {
            anyhow::anyhow!(
                "Failed to initialize K8s DNS discovery: {e}. \
                 Ensure HEADLESS_SERVICE_NAME, POD_NAMESPACE, and POD_IP env vars are set."
            )
        })?;
        let discovery = discovery
            .with_cluster_secret(self.cluster_secret)
            .with_node_directory(registry);

        if let Err(error) = discovery.refresh().await {
            warn!("Initial K8s DNS resolution failed (will retry): {}", error);
        }
        let peers = discovery.get_peers().await;
        info!(
            dns_name = %discovery.dns_name(),
            peer_count = peers.len(),
            "K8s DNS discovery initialized"
        );

        Ok(vec![ClusterDiscoveryTask::new(
            "peer_discovery",
            discovery.start_refresh_loop(10, self.shutdown_token),
        )])
    }
}

#[cfg(feature = "k8s")]
fn build_cluster_peer_discovery_driver(
    config: &Config,
    shutdown_token: CancellationToken,
) -> Box<dyn ClusterPeerDiscoveryDriver> {
    match config.cluster.discovery_mode {
        ClusterDiscoveryMode::K8sDns => Box::new(K8sClusterPeerDiscoveryDriver {
            api_port: config.server.port,
            cluster_secret: config.cluster.secret.clone(),
            shutdown_token,
        }),
        ClusterDiscoveryMode::Static => {
            build_static_cluster_peer_discovery_driver(config, shutdown_token)
        }
        ClusterDiscoveryMode::Redis => Box::new(RedisClusterPeerDiscoveryDriver),
    }
}

#[cfg(not(feature = "k8s"))]
fn build_cluster_peer_discovery_driver(
    config: &Config,
    shutdown_token: CancellationToken,
) -> Result<Box<dyn ClusterPeerDiscoveryDriver>, anyhow::Error> {
    match config.cluster.discovery_mode {
        ClusterDiscoveryMode::K8sDns => Err(anyhow::anyhow!(
            "K8s DNS discovery mode requires the 'k8s' feature. \
             Rebuild with: cargo build --features k8s"
        )),
        ClusterDiscoveryMode::Static => Ok(build_static_cluster_peer_discovery_driver(
            config,
            shutdown_token,
        )),
        ClusterDiscoveryMode::Redis => Ok(Box::new(RedisClusterPeerDiscoveryDriver)),
    }
}

fn build_static_cluster_peer_discovery_driver(
    config: &Config,
    shutdown_token: CancellationToken,
) -> Box<dyn ClusterPeerDiscoveryDriver> {
    info!("Using static peer discovery mode");
    let peers: Vec<StaticPeerConfig> = config
        .cluster
        .peers
        .iter()
        .map(|addr| StaticPeerConfig {
            api_address: addr.clone(),
        })
        .collect();

    if peers.is_empty() {
        warn!("Static discovery mode selected but no peers configured (cluster.peers is empty)");
    }

    Box::new(StaticClusterPeerDiscoveryDriver {
        config: StaticDiscoveryConfig {
            peers,
            probe_interval_secs: 10,
            connect_timeout: Duration::from_secs(3),
            cluster_secret: config.cluster.secret.clone(),
            default_api_port: config.server.port,
        },
        cancel_token: shutdown_token,
    })
}

pub fn init_cluster_components(
    node_directory_factory: &Arc<dyn ClusterNodeDirectoryFactory>,
    cm: &Arc<RealtimeManager>,
    config: &Config,
    _connection_manager: Arc<dyn ConnectionRuntime>,
) -> Result<ClusterDiscoveryComponents, anyhow::Error> {
    let node_id = cm.node_id().to_string();
    let heartbeat_timeout_secs: i64 = 30;

    let registry = node_directory_factory
        .build(node_id, heartbeat_timeout_secs, &config.redis.key_prefix)
        .map_err(|e| anyhow::anyhow!("Failed to create cluster node directory: {e}"))?;

    let health_monitor: Arc<dyn ClusterHealthRuntime> = Arc::new(
        HealthMonitor::with_runtime_cancellation_token_and_probe_config(
            registry.clone(),
            15,
            &cm.cancel_token(),
            HealthProbeConfig {
                cluster_secret: config.cluster.secret.clone(),
                ..HealthProbeConfig::default()
            },
        ),
    );

    Ok(ClusterDiscoveryComponents {
        registry,
        health_monitor,
    })
}

pub async fn activate_cluster_node(
    config: &Config,
    cm: &Arc<RealtimeManager>,
    connection_manager: Arc<dyn ConnectionRuntime>,
    registry: &Arc<dyn ClusterNodeDirectory>,
    health_monitor: &Arc<dyn ClusterHealthRuntime>,
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
    cm.start_heartbeat_loop_with_directory(
        registry.clone(),
        advertise_api,
        Some(move || conn_mgr_for_hb.connection_count()),
    )
    .await;

    match health_monitor.start() {
        Ok(()) => {
            info!("Health monitor started");
            Ok(())
        }
        Err(e) => {
            cm.shutdown().await;
            Err(anyhow::anyhow!(
                "Failed to start health monitor: {e}. \
                 Health monitoring is required for distributed mode."
            ))
        }
    }
}

/// Initialize cluster discovery infrastructure (`NodeRegistry` + `HealthMonitor`).
///
/// Supports two discovery modes:
///   "redis"   - Redis-based node registry (default)
///   "`k8s_dns`" - Kubernetes headless service DNS discovery
///
/// When distributed mode is explicitly enabled, any failure is propagated to the caller
/// as a fatal error, preventing the node from running in a ghost state.
pub async fn init_cluster_discovery(
    config: &Config,
    node_directory_factory: &Arc<dyn ClusterNodeDirectoryFactory>,
    cm: &Arc<RealtimeManager>,
    connection_manager: Arc<dyn ConnectionRuntime>,
    shutdown_token: CancellationToken,
) -> Result<ClusterDiscoveryBundle, anyhow::Error> {
    let components =
        init_cluster_components(node_directory_factory, cm, config, connection_manager)?;
    #[cfg(feature = "k8s")]
    let discovery_driver = build_cluster_peer_discovery_driver(config, shutdown_token);
    #[cfg(not(feature = "k8s"))]
    let discovery_driver = build_cluster_peer_discovery_driver(config, shutdown_token)?;
    let background_tasks = discovery_driver.start(components.registry.clone()).await?;

    Ok(ClusterDiscoveryBundle::new(components, background_tasks))
}

#[cfg(test)]
mod tests {
    use crate::bootstrap::cluster::{
        build_cluster_coordination_provider, init_cluster_components, init_cluster_discovery,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use synctv_cluster::discovery::{ClusterNodeDirectory, ClusterNodeDirectoryFactory};
    use synctv_core::{config::ClusterDiscoveryMode, Config};
    use synctv_realtime::sync::{
        build_room_message_runtime, ConnectionLimits, ConnectionManager, RealtimeConfig,
        RealtimeManager, RealtimeManagerRuntime,
    };
    use tokio::sync::RwLock;
    use tokio_util::sync::CancellationToken;

    fn test_realtime_config() -> Config {
        let mut config = Config::default();
        config.server.host = "127.0.0.1".to_string();
        config.server.port = 8080;
        config.cluster.secret.clear();
        config.redis.url = "redis://127.0.0.1:6379".to_string();
        config.cluster.discovery_mode = ClusterDiscoveryMode::K8sDns;
        config
    }

    #[test]
    fn test_cluster_discovery_task_uses_backend_agnostic_label() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");

        runtime.block_on(async {
            let task = super::ClusterDiscoveryTask::new("peer_discovery", tokio::spawn(async {}));

            assert_eq!(task.name, "peer_discovery");
            task.handle
                .await
                .expect("synthetic cluster discovery task should complete cleanly");
        });
    }

    #[derive(Clone, Default)]
    struct CountingDirectoryFactory {
        build_count: Arc<AtomicUsize>,
    }

    impl ClusterNodeDirectoryFactory for CountingDirectoryFactory {
        fn build(
            &self,
            node_id: String,
            heartbeat_timeout_secs: i64,
            key_prefix: &str,
        ) -> synctv_cluster::Result<Arc<dyn ClusterNodeDirectory>> {
            self.build_count.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(
                synctv_cluster::discovery::NodeRegistry::new_local_only(
                    node_id,
                    heartbeat_timeout_secs,
                    key_prefix,
                )?,
            ))
        }
    }

    #[tokio::test]
    async fn test_init_cluster_components_uses_injected_directory_factory() {
        let factory = CountingDirectoryFactory::default();
        let config = Config::default();
        let realtime_manager = Arc::new(
            RealtimeManager::new(RealtimeConfig {
                distributed_transport_factory: None,
                message_runtime: Arc::new(synctv_realtime::RoomMessageHub::new()),
                distributed_enabled: false,
                node_id: "bootstrap-factory-test".to_string(),
                dedup_window: Duration::from_secs(1),
                critical_channel_capacity: 8,
                publish_channel_capacity: 8,
                key_prefix: "bootstrap-factory-test:".to_string(),
                catchup_window_secs: 60,
                stream_max_length: 100,
                event_handler: None,
                parent_cancel_token: None,
            })
            .await
            .expect("local realtime manager should initialize"),
        );
        let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));

        let directory_factory: Arc<dyn ClusterNodeDirectoryFactory> = Arc::new(factory.clone());
        let components = init_cluster_components(
            &directory_factory,
            &realtime_manager,
            &config,
            connection_manager,
        )
        .expect("cluster components should use injected directory factory");

        assert_eq!(
            factory.build_count.load(Ordering::Relaxed),
            1,
            "init_cluster_components must build the node directory through the injected factory"
        );
        assert_eq!(
            components.registry.cluster_mode(),
            synctv_cluster::ClusterMode::Standalone,
            "test factory returns a local-only directory to prove the injected factory was used"
        );
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
        let coordination_provider = build_cluster_coordination_provider(
            synctv_core::coordination_runtime_from_client(client.clone()),
        );
        let realtime_config = RealtimeConfig {
            distributed_transport_factory: Some(
                coordination_provider.distributed_transport_factory(),
            ),
            message_runtime: build_room_message_runtime(
                &synctv_core::SharedStateProfile::from_runtime(
                    Some(synctv_core::shared_runtime(shared_conn.clone())),
                    "test-k8s-env-order:",
                    true,
                ),
            )
            .expect("shared message runtime should initialize"),
            distributed_enabled: true,
            node_id: "bootstrap-k8s-env-order".to_string(),
            dedup_window: Duration::from_secs(1),
            critical_channel_capacity: 16,
            publish_channel_capacity: 16,
            key_prefix: "test-k8s-env-order:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
            event_handler: None,
            parent_cancel_token: Some(CancellationToken::new()),
        };
        let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
        let manager = RealtimeManager::new_with_runtime(
            realtime_config,
            RealtimeManagerRuntime {
                connection_runtime: Some(connection_manager.clone()),
                leader_runtime: None,
            },
        )
        .await
        .expect("realtime manager");
        let manager = Arc::new(manager);

        let mut config = test_realtime_config();
        config.redis.key_prefix = "test-k8s-env-order:".to_string();

        let old_service_name = std::env::var_os("HEADLESS_SERVICE_NAME");
        let old_namespace = std::env::var_os("POD_NAMESPACE");
        let old_pod_ip = std::env::var_os("POD_IP");
        std::env::set_var("HEADLESS_SERVICE_NAME", "synctv-headless");
        std::env::set_var("POD_NAMESPACE", "default");
        std::env::remove_var("POD_IP");

        let result = init_cluster_discovery(
            &config,
            &coordination_provider.node_directory_factory(),
            &manager,
            connection_manager,
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
            synctv_core::coordination_runtime_from_client(client),
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
