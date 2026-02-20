//! Application lifecycle management.
//!
//! `Application` encapsulates the entire SyncTV startup sequence as a series
//! of named phases, each producing a typed output. This replaces the
//! monolithic `main()` function with a readable, maintainable structure.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use sqlx::PgPool;
use tracing::{error, info, warn};

use synctv_core::{
    bootstrap::{
        database::init_database_with_cancel, has_any_users, init_redis, init_services,
        bootstrap_root_user, RedisHandles,
    },
    cache::{CacheInvalidationService, KeyBuilder},
    provider::{AlistProvider, BilibiliProvider, EmbyProvider},
    Config,
};
use synctv_cluster::leader::LeaderElector;
#[cfg(feature = "k8s")]
use synctv_cluster::leader::{K8sLeaderElector, K8sLeaderElectorConfig};
use synctv_cluster::sync::{ClusterConfig, ClusterManager, ConnectionLimits, ConnectionManager};

use crate::bootstrap::cluster::init_cluster_discovery;
use crate::bootstrap::livestream::init_livestream;
use crate::bootstrap::node_id::generate_node_id;
use crate::bootstrap::webrtc::init_webrtc;
use crate::cluster_bridge::ClusterPlaybackBroadcaster;
use crate::server::{LivestreamState, Services, SyncTvServer};
use crate::shutdown::{AuditFlushHook, SettingsListenHook, ShutdownCoordinator};

/// Infrastructure: Redis, Database, NodeID.
struct Infrastructure {
    config: Config,
    pool: PgPool,
    redis_handles: RedisHandles,
    node_id: String,
}

/// Core services from `synctv-core`.
struct CoreState {
    services: synctv_core::bootstrap::services::Services,
    cache_invalidation: Arc<CacheInvalidationService>,
}

/// Leader election and singleton background tasks.
struct LeaderState {
    leader_check: Arc<dyn synctv_core::service::LeaderCheck>,
}

/// Cluster infrastructure.
struct ClusterState {
    cluster_manager: Option<Arc<ClusterManager>>,
    connection_manager: ConnectionManager,
    redis_publish_tx: Option<tokio::sync::mpsc::Sender<synctv_cluster::sync::PublishRequest>>,
    node_registry: Option<Arc<synctv_cluster::discovery::NodeRegistry>>,
    health_monitor: Option<Arc<synctv_cluster::discovery::HealthMonitor>>,
    load_balancer: Option<Arc<synctv_cluster::discovery::LoadBalancer>>,
}

/// Server components (livestream, WebRTC, providers).
struct ServerComponents {
    livestream_state: Option<LivestreamState>,
    live_infra: Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    stun_server: Option<Arc<synctv_core::service::StunServer>>,
    alist_provider: Arc<AlistProvider>,
    bilibili_provider: Arc<BilibiliProvider>,
    emby_provider: Arc<EmbyProvider>,
}

/// The assembled application, ready to be started.
pub struct Application {
    config: Config,
    pool: PgPool,
    services: Services,
    livestream_state: Option<LivestreamState>,
    shutdown: ShutdownCoordinator,
}

impl Application {
    /// Build the application through phased initialization.
    pub async fn build(config: Config) -> Result<Self> {
        let mut shutdown = ShutdownCoordinator::new();

        // Phase 1: Infrastructure (Redis, Database, NodeID)
        let infra = Self::init_infrastructure(config, &mut shutdown).await?;

        // Phase 2: Schema (migrations, root user, partitions)
        Self::init_schema(&infra).await?;

        // Phase 3: Core services
        let core = Self::init_core_services(&infra, &mut shutdown).await?;

        // Phase 4: Leader election and singleton tasks
        let leader = Self::init_leader_election(&infra, &core, &mut shutdown).await?;

        // Phase 5: Singleton background tasks
        Self::start_singleton_tasks(&infra, &core, &leader, &mut shutdown);

        // Phase 6: Cluster infrastructure
        let cluster = Self::init_cluster(&infra, &core, &mut shutdown).await;

        // Phase 7: Server components (livestream, WebRTC, providers)
        let servers = Self::init_servers(&infra, &core, &mut shutdown).await?;

        // Assemble
        Ok(Self::assemble(infra, core, cluster, servers, shutdown))
    }

    /// Start all servers and wait for shutdown.
    pub async fn run(self) -> Result<()> {
        let server = SyncTvServer::new(
            self.config,
            self.services,
            self.livestream_state,
            self.pool,
        );
        server.start_with_coordinator(self.shutdown).await
    }

    // -- Phase 1: Infrastructure ------------------------------------------------

    async fn init_infrastructure(
        config: Config,
        shutdown: &mut ShutdownCoordinator,
    ) -> Result<Infrastructure> {
        // Generate node_id once for the entire process
        let node_id = generate_node_id();
        info!("Node ID: {node_id}");

        // Redis (mandatory dependency)
        let sentinel_cancel = shutdown.register_token("sentinel_health_check");
        let redis_handles = init_redis(&config, Some(sentinel_cancel)).await?;

        // Database (with cancellable pool metrics task)
        let db_metrics_cancel = shutdown.register_token("db_pool_metrics");
        let pool = init_database_with_cancel(&config, Some(db_metrics_cancel)).await?;

        Ok(Infrastructure {
            config,
            pool,
            redis_handles,
            node_id,
        })
    }

    // -- Phase 2: Schema --------------------------------------------------------

    async fn init_schema(infra: &Infrastructure) -> Result<()> {
        // Run migrations (with distributed lock via shared Redis connection)
        let migration_lock = synctv_core::service::DistributedLock::new(
            infra.redis_handles.conn_snapshot().await,
        );
        crate::migrations::run_migrations(
            &infra.pool,
            &migration_lock,
            &infra.config.redis.key_prefix,
        )
        .await?;

        // Bootstrap root user
        info!("Checking root user bootstrap...");
        if let Err(e) = bootstrap_root_user(&infra.pool, &infra.config.bootstrap).await {
            // On first deployment (no users exist), bootstrap failure is fatal
            // because there would be no way to administer the system.
            if has_any_users(&infra.pool).await {
                warn!("Failed to bootstrap root user: {}", e);
                warn!("Existing users found, continuing startup");
            } else {
                return Err(anyhow::anyhow!(
                    "Failed to bootstrap root user on first deployment (no users exist): {}. \
                     The system cannot operate without at least one user.",
                    e
                ));
            }
        }

        // Initialize audit log partitions (non-fatal)
        info!("Initializing audit log partitions...");
        if let Err(e) =
            synctv_core::service::ensure_audit_partitions_on_startup(&infra.pool).await
        {
            error!(
                "Failed to initialize audit partitions (non-fatal, continuing startup): {}",
                e
            );
        }

        Ok(())
    }

    // -- Phase 3: Core services -------------------------------------------------

    async fn init_core_services(
        infra: &Infrastructure,
        shutdown: &mut ShutdownCoordinator,
    ) -> Result<CoreState> {
        // Initialize CacheInvalidationService early (before init_services).
        // Uses the cluster node_id so invalidation messages are correctly attributed.
        let key_builder = KeyBuilder::from_config(&infra.config);
        let cache_invalidation = Arc::new(CacheInvalidationService::new(
            Some(infra.redis_handles.client.clone()),
            infra.node_id.clone(),
            key_builder.cache_invalidation_stream(),
        ));

        // Start the cache invalidation Redis subscriber BEFORE init_services.
        // Issue #44: subscriber must be running before any service publishes an
        // invalidation event to avoid dropped messages during initialization.
        if let Err(e) = cache_invalidation.start().await {
            warn!("Failed to start cache invalidation listener: {}", e);
        }

        // Initialize core services
        let synctv_services = init_services(
            infra.pool.clone(),
            &infra.config,
            infra.redis_handles.clone(),
            cache_invalidation.clone(),
        )
        .await?;

        // Track settings cancellation token and listen task in shutdown coordinator
        shutdown.track_token("settings", synctv_services.settings_cancel.clone());
        shutdown.register_hook(AuditFlushHook {
            handle: synctv_services.audit_flush_handle.clone(),
        });
        shutdown.register_hook(SettingsListenHook {
            task: synctv_services.settings_listen_task.clone(),
        });

        // Initialize chat message partitions (needs settings_registry from services)
        info!("Initializing chat message partitions...");
        if let Err(e) = synctv_core::service::ensure_chat_partitions_on_startup(
            &infra.pool,
            synctv_services.settings_registry.clone(),
        )
        .await
        {
            error!(
                "Failed to initialize chat partitions (non-fatal, continuing startup): {}",
                e
            );
        }

        Ok(CoreState {
            services: synctv_services,
            cache_invalidation,
        })
    }

    // -- Phase 4: Leader election -----------------------------------------------

    async fn init_leader_election(
        infra: &Infrastructure,
        core: &CoreState,
        shutdown: &mut ShutdownCoordinator,
    ) -> Result<LeaderState> {
        let leader_cancel = shutdown.register_token("leader_election");

        let (leader_elector, leader_election_handle) = {
            let leader_mode = infra.config.cluster.leader_election_mode.as_str();
            match leader_mode {
                #[cfg(feature = "k8s")]
                "k8s_lease" => {
                    info!("Using K8s Lease-based leader election");
                    let pod_name =
                        std::env::var("POD_NAME").unwrap_or_else(|_| infra.node_id.clone());
                    let namespace =
                        std::env::var("POD_NAMESPACE").unwrap_or_else(|_| "default".to_string());

                    match K8sLeaderElector::new(
                        pod_name.clone(),
                        namespace.clone(),
                        K8sLeaderElectorConfig::default(),
                    )
                    .await
                    {
                        Ok(elector) => {
                            let handle = elector.start(leader_cancel.clone());
                            info!(
                                pod_name = %pod_name,
                                namespace = %namespace,
                                "K8s leader election started"
                            );
                            (
                                Some(synctv_cluster::leader::AnyLeaderElector::K8s(elector)),
                                Some(handle),
                            )
                        }
                        Err(e) => {
                            error!("Failed to initialize K8s leader election: {}", e);
                            error!(
                                "Ensure POD_NAME and POD_NAMESPACE env vars are set and RBAC is configured"
                            );
                            (None, None)
                        }
                    }
                }
                #[cfg(not(feature = "k8s"))]
                "k8s_lease" => {
                    error!(
                        "K8s Lease-based leader election requires the 'k8s' feature. \
                         Rebuild with: cargo build --features k8s"
                    );
                    (None, None)
                }
                _ => {
                    if leader_mode != "redis" {
                        warn!(
                            leader_election_mode = %leader_mode,
                            "Unknown leader election mode, falling back to 'redis'"
                        );
                    }

                    let plain_conn = core.services.redis_conn.read().await.clone();
                    let elector = LeaderElector::new(
                        plain_conn,
                        infra.node_id.clone(),
                        &infra.config.redis.key_prefix,
                    );
                    let handle = elector.start(leader_cancel.clone());
                    info!(
                        "Redis-based leader election started (node_id={})",
                        infra.node_id
                    );
                    (
                        Some(synctv_cluster::leader::AnyLeaderElector::Redis(elector)),
                        Some(handle),
                    )
                }
            }
        };

        // Track leader election background task
        if let Some(handle) = leader_election_handle {
            shutdown.register_task("leader_election", handle);
        }

        let leader_check: Arc<dyn synctv_core::service::LeaderCheck> = match &leader_elector {
            Some(elector) => Arc::new(elector.clone()),
            None => Arc::new(synctv_core::service::AlwaysLeader),
        };

        Ok(LeaderState { leader_check })
    }

    // -- Phase 5: Singleton background tasks ------------------------------------

    fn start_singleton_tasks(
        infra: &Infrastructure,
        core: &CoreState,
        leader: &LeaderState,
        shutdown: &mut ShutdownCoordinator,
    ) {
        let singleton_cancel = shutdown.register_token("singleton_tasks");

        let audit_manager = synctv_core::service::AuditPartitionManager::new(
            infra.pool.clone(),
            leader.leader_check.clone(),
        );
        shutdown.register_task(
            "audit_partition",
            audit_manager.start_auto_management(24, singleton_cancel.clone()),
        );
        info!("Audit log partition management started (leader-gated with fencing)");

        let chat_partition_manager = synctv_core::service::ChatPartitionManager::new(
            infra.pool.clone(),
            core.services.settings_registry.clone(),
            leader.leader_check.clone(),
        );
        shutdown.register_task(
            "chat_partition",
            chat_partition_manager.start_auto_management(24, singleton_cancel.clone()),
        );
        info!("Chat message partition management started (leader-gated with fencing, check interval: 24 hours)");

        let cleanup_service = synctv_core::service::CleanupService::new(
            infra.pool.clone(),
            synctv_core::service::CleanupConfig::default(),
            leader.leader_check.clone(),
        );
        shutdown.register_task(
            "data_cleanup",
            cleanup_service.start_periodic(24, singleton_cancel.clone()),
        );
        info!("Periodic data cleanup started (leader-gated with fencing, interval: 24 hours)");

        let db_maintenance = synctv_core::service::DatabaseMaintenanceService::new(
            infra.pool.clone(),
            leader.leader_check.clone(),
        );
        shutdown.register_task(
            "db_maintenance",
            db_maintenance.spawn_maintenance_loop(singleton_cancel),
        );
        info!("Database maintenance service started (leader-gated: partitions every 12h, cleanups every 1h)");
    }

    // -- Phase 6: Cluster infrastructure ----------------------------------------

    async fn init_cluster(
        infra: &Infrastructure,
        core: &CoreState,
        shutdown: &mut ShutdownCoordinator,
    ) -> ClusterState {
        // Connection manager
        let connection_limits = ConnectionLimits {
            max_per_user: infra.config.connection_limits.max_per_user,
            max_per_room: infra.config.connection_limits.max_per_room,
            max_total: infra.config.connection_limits.max_total,
            idle_timeout: Duration::from_secs(infra.config.connection_limits.idle_timeout_seconds),
            max_duration: Duration::from_secs(
                infra.config.connection_limits.max_duration_seconds,
            ),
        };
        let connection_manager = ConnectionManager::new(connection_limits);
        info!(
            max_per_user = infra.config.connection_limits.max_per_user,
            max_per_room = infra.config.connection_limits.max_per_room,
            max_total = infra.config.connection_limits.max_total,
            "Connection manager initialized with configurable limits"
        );

        // ClusterManager
        let permission_service =
            Some(core.services.room_service.permission_service().clone());

        let cluster_manager = {
            let cluster_config = ClusterConfig {
                redis_client: Some(infra.redis_handles.client.clone()),
                redis_conn: Some(infra.redis_handles.conn_snapshot().await),
                node_id: infra.node_id.clone(),
                dedup_window: Duration::from_secs(infra.config.cluster.catchup_window_secs.saturating_mul(2).max(600)),
                cleanup_interval: Duration::from_secs(30),
                critical_channel_capacity: infra.config.cluster.critical_channel_capacity,
                publish_channel_capacity: infra.config.cluster.publish_channel_capacity,
                key_prefix: infra.config.redis.key_prefix.clone(),
                catchup_window_secs: infra.config.cluster.catchup_window_secs,
                stream_max_length: infra.config.cluster.stream_max_length,
            };
            match ClusterManager::new(
                cluster_config,
                permission_service,
                Some((*core.cache_invalidation).clone()),
            )
            .await
            {
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

        // Wire cluster broadcaster into PlaybackService
        if let Some(ref cm) = cluster_manager {
            core.services
                .room_service
                .set_playback_cluster_broadcaster(Arc::new(ClusterPlaybackBroadcaster {
                    cluster_manager: cm.clone(),
                }));
            info!("PlaybackService wired with cluster broadcaster");
        }

        // Cluster discovery (NodeRegistry, HealthMonitor, LoadBalancer)
        let (node_registry, health_monitor, load_balancer, dns_refresh_handle) =
            if let Some(ref cm) = cluster_manager {
                init_cluster_discovery(
                    &infra.config,
                    &infra.redis_handles,
                    cm,
                    &connection_manager,
                )
                .await
            } else {
                (None, None, None, None)
            };

        // Track DNS refresh task
        if let Some(handle) = dns_refresh_handle {
            shutdown.register_task("dns_refresh", handle);
        }

        let redis_publish_tx = cluster_manager
            .as_ref()
            .and_then(|cm| cm.redis_publish_tx().cloned());

        ClusterState {
            cluster_manager,
            connection_manager,
            redis_publish_tx,
            node_registry,
            health_monitor,
            load_balancer,
        }
    }

    // -- Phase 7: Server components ---------------------------------------------

    async fn init_servers(
        infra: &Infrastructure,
        core: &CoreState,
        shutdown: &mut ShutdownCoordinator,
    ) -> Result<ServerComponents> {
        // Livestream
        let (livestream_state, live_infra, background_handles) =
            init_livestream(&infra.config, &core.services, &infra.redis_handles, &infra.node_id)
                .await?;
        for (i, handle) in background_handles.into_iter().enumerate() {
            shutdown.register_task(
                // Use a static string per convention; the index disambiguates in logs
                if i == 0 {
                    "livestream_lifecycle"
                } else {
                    "livestream_background"
                },
                handle,
            );
        }

        // WebRTC (STUN server)
        let stun_server = init_webrtc(&infra.config).await;

        // Media providers
        let pim = core.services.provider_instance_manager.clone();
        let alist_provider = Arc::new(AlistProvider::new(pim.clone()));
        let bilibili_provider = Arc::new(BilibiliProvider::new(pim.clone()));
        let emby_provider = Arc::new(EmbyProvider::new(pim));

        Ok(ServerComponents {
            livestream_state,
            live_infra,
            stun_server,
            alist_provider,
            bilibili_provider,
            emby_provider,
        })
    }

    // -- Assembly ---------------------------------------------------------------

    fn assemble(
        infra: Infrastructure,
        core: CoreState,
        cluster: ClusterState,
        servers: ServerComponents,
        shutdown: ShutdownCoordinator,
    ) -> Self {
        let services = Services {
            user_service: core.services.user_service.clone(),
            room_service: core.services.room_service.clone(),
            jwt_service: core.services.jwt_service.clone(),
            cluster_manager: cluster.cluster_manager,
            redis_publish_tx: cluster.redis_publish_tx,
            rate_limiter: core.services.rate_limiter.clone(),
            rate_limit_config: core.services.rate_limit_config.clone(),
            content_filter: core.services.content_filter.clone(),
            connection_manager: cluster.connection_manager,
            providers_manager: core.services.providers_manager.clone(),
            provider_instance_manager: core.services.provider_instance_manager.clone(),
            provider_instance_repository: core.services.provider_instance_repo.clone(),
            user_provider_credential_repository: core
                .services
                .user_provider_credential_repo
                .clone(),
            alist_provider: servers.alist_provider,
            bilibili_provider: servers.bilibili_provider,
            emby_provider: servers.emby_provider,
            oauth2_service: core.services.oauth2_service.clone(),
            settings_service: core.services.settings_service.clone(),
            settings_registry: core.services.settings_registry.clone(),
            email_service: core.services.email_service.clone(),
            email_token_service: core.services.email_token_service.clone(),
            publish_key_service: core.services.publish_key_service.clone(),
            notification_service: Some(core.services.notification_service.clone()),
            audit_service: core.services.audit_service.clone(),
            live_streaming_infrastructure: servers.live_infra,
            stun_server: servers.stun_server,
            node_registry: cluster.node_registry,
            health_monitor: cluster.health_monitor,
            load_balancer: cluster.load_balancer,
            redis_client: infra.redis_handles.client.clone(),
            redis_conn: core.services.redis_conn.clone(),
            credential_encryption: core.services.credential_encryption.clone(),
        };

        Self {
            config: infra.config,
            pool: infra.pool,
            services,
            livestream_state: servers.livestream_state,
            shutdown,
        }
    }
}
