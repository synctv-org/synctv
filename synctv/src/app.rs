//! Application lifecycle management.
//!
//! `Application` encapsulates the entire `SyncTV` startup sequence as a series
//! of named phases, each producing a typed output. This replaces the
//! monolithic `main()` function with a readable, maintainable structure.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use sqlx::PgPool;
use tracing::{error, info, warn};

use synctv_cluster::leader::LeaderElector;
#[cfg(feature = "k8s")]
use synctv_cluster::leader::{K8sLeaderElector, K8sLeaderElectorConfig};
use synctv_cluster::sync::{ClusterConfig, ClusterManager, ConnectionLimits, ConnectionManager};
use synctv_core::{
    bootstrap::{
        bootstrap_root_user, database::init_database_with_cancel, has_any_users, init_redis,
        init_services, RedisHandles,
    },
    cache::{CacheInvalidationService, KeyBuilder},
    provider::{AlistProvider, BilibiliProvider, EmbyProvider},
    Config,
};

use crate::bootstrap::cluster::init_cluster_discovery;
use crate::bootstrap::livestream::init_livestream;
use crate::bootstrap::node_id::generate_node_id;
use crate::bootstrap::webrtc::init_webrtc;
use crate::cluster_bridge::ClusterPlaybackBroadcaster;
use crate::server::{LivestreamState, Services, SyncTvServer};
use crate::shutdown::{
    AuditFlushHook, CacheInvalidationStopHook, SettingsListenHook, ShutdownCoordinator,
};

/// Infrastructure: Redis (optional), Database, `NodeID`.
struct Infrastructure {
    config: Config,
    pool: PgPool,
    redis_handles: Option<RedisHandles>,
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
}

/// Server components (livestream, WebRTC, providers).
struct ServerComponents {
    livestream_state: Option<LivestreamState>,
    live_infra: Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    stun_server: Option<Arc<synctv_core::service::StunServer>>,
    turn_health_checker: Option<Arc<synctv_core::service::TurnHealthChecker>>,
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
    ///
    /// If any phase fails, all resources created in earlier phases are cleaned up
    /// via the `ShutdownCoordinator` before returning the error.
    pub async fn build(config: Config) -> Result<Self> {
        // Validate configuration before any initialization (fail fast on misconfigurations)
        if let Err(errors) = config.validate() {
            return Err(anyhow::anyhow!(
                "Configuration validation failed: {}",
                errors.join("; ")
            ));
        }

        let shutdown_budget =
            std::time::Duration::from_secs(config.server.shutdown_drain_timeout_seconds);
        let mut shutdown = ShutdownCoordinator::new(shutdown_budget);

        // Phase 1: Infrastructure (Redis, Database, NodeID)
        let infra = match Self::init_infrastructure(config, &mut shutdown).await {
            Ok(infra) => infra,
            Err(e) => {
                shutdown.shutdown().await;
                return Err(e);
            }
        };

        // Phase 2: Schema (migrations, root user, partitions)
        if let Err(e) = Self::init_schema(&infra).await {
            shutdown.shutdown().await;
            return Err(e);
        }

        // Phase 3: Core services
        let core = match Self::init_core_services(&infra, &mut shutdown).await {
            Ok(core) => core,
            Err(e) => {
                shutdown.shutdown().await;
                return Err(e);
            }
        };

        // Phase 4: Leader election and singleton tasks
        let leader = match Self::init_leader_election(&infra, &core, &mut shutdown).await {
            Ok(leader) => leader,
            Err(e) => {
                shutdown.shutdown().await;
                return Err(e);
            }
        };

        // Phase 5: Singleton background tasks
        Self::start_singleton_tasks(&infra, &core, &leader, &mut shutdown);

        // Phase 6: Cluster infrastructure
        let cluster = match Self::init_cluster(&infra, &core, &mut shutdown).await {
            Ok(cluster) => cluster,
            Err(e) => {
                shutdown.shutdown().await;
                return Err(e);
            }
        };

        // Phase 7: Server components (livestream, WebRTC, providers)
        let servers = match Self::init_servers(&infra, &core, &mut shutdown).await {
            Ok(servers) => servers,
            Err(e) => {
                shutdown.shutdown().await;
                return Err(e);
            }
        };

        // Assemble
        Ok(Self::assemble(infra, core, cluster, servers, shutdown))
    }

    /// Start all servers and wait for shutdown.
    pub async fn run(self) -> Result<()> {
        let server =
            SyncTvServer::new(self.config, self.services, self.livestream_state, self.pool);
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

        // Redis (optional in standalone mode, mandatory in cluster mode)
        let sentinel_cancel = shutdown.register_token("sentinel_health_check");
        let redis_handles = init_redis(&config, Some(sentinel_cancel)).await?;

        if redis_handles.is_some() {
            info!("Redis connected");
        } else {
            info!("Running without Redis (standalone mode)");
        }

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
        // Run migrations with appropriate lock strategy:
        // - Redis available: distributed lock (safe for multi-replica)
        // - No Redis: PostgreSQL advisory lock (safe for single-node)
        let migration_lock: Box<dyn synctv_core::service::MigrationLock> =
            if let Some(ref rh) = infra.redis_handles {
                info!("Using Redis distributed lock for migrations");
                let is_sentinel = matches!(
                    infra.config.redis.deployment_mode,
                    synctv_core::config::RedisDeploymentMode::Sentinel
                );
                Box::new(synctv_core::service::DistributedLock::new_with_mode(
                    rh.conn_snapshot().await,
                    is_sentinel,
                ))
            } else {
                info!("Using PostgreSQL advisory lock for migrations");
                Box::new(synctv_core::service::PgAdvisoryMigrationLock::new(
                    infra.pool.clone(),
                ))
            };
        crate::migrations::run_migrations(
            &infra.pool,
            migration_lock.as_ref(),
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
                    "Failed to bootstrap root user on first deployment (no users exist): {e}. \
                     The system cannot operate without at least one user."
                ));
            }
        }

        // Initialize audit log partitions (non-fatal)
        info!("Initializing audit log partitions...");
        if let Err(e) = synctv_core::service::ensure_audit_partitions_on_startup(&infra.pool).await
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
        // When Redis is not configured, cache invalidation operates in no-op mode.
        let key_builder = KeyBuilder::from_config(&infra.config);
        let redis_client_for_cache = infra.redis_handles.as_ref().map(|h| h.client.clone());
        let cache_invalidation_svc = CacheInvalidationService::new(
            redis_client_for_cache,
            infra.node_id.clone(),
            key_builder.cache_invalidation_stream(),
        );
        // H9 fix: wire the shared Redis handle so the cache invalidation service
        // follows Sentinel failover instead of creating an independent connection.
        let cache_invalidation_svc = if let Some(ref rh) = infra.redis_handles {
            cache_invalidation_svc.with_shared_conn(rh.conn.clone())
        } else {
            cache_invalidation_svc
        };
        let cache_invalidation = Arc::new(cache_invalidation_svc);

        // Start the cache invalidation Redis subscriber BEFORE init_services.
        // Issue #44: subscriber must be running before any service publishes an
        // invalidation event to avoid dropped messages during initialization.
        if infra.redis_handles.is_some() {
            if let Err(e) = cache_invalidation.start().await {
                // When cluster mode is explicitly enabled, cache invalidation failure
                // is a fatal error - the cluster cannot maintain cache consistency without it.
                // In standalone mode, we can continue with local-only caching.
                if infra.config.cluster.enabled {
                    return Err(anyhow::anyhow!(
                        "Failed to start cache invalidation listener (cluster mode): {e}. \
                         Cache consistency is required when cluster.enabled=true."
                    ));
                }
                warn!("Failed to start cache invalidation listener (continuing in standalone mode): {}", e);
            }
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
        shutdown.register_hook(CacheInvalidationStopHook {
            service: cache_invalidation.clone(),
        });
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

        // Initialize notification partitions (monthly granularity)
        info!("Initializing notification partitions...");
        if let Err(e) =
            synctv_core::service::ensure_notification_partitions_on_startup(&infra.pool).await
        {
            error!(
                "Failed to initialize notification partitions (non-fatal, continuing startup): {}",
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
        // Without Redis, use AlwaysLeader (single node = always the leader)
        // This is safe because without Redis, cluster mode is disabled and
        // there's only one node.
        let Some(ref redis_conn) = core.services.redis_conn else {
            info!("No Redis configured — using AlwaysLeader (single node)");
            // Set metrics: standalone mode (0), always leader
            synctv_core::metrics::cluster::LEADER_ELECTION_MODE.set(0);
            synctv_core::metrics::cluster::LEADER_ELECTION_STATE.set(1);
            synctv_core::metrics::cluster::LEADER_ELECTION_EPOCH.set(0);
            synctv_core::metrics::cluster::LEADER_ELECTION_CONSECUTIVE_FAILURES.set(0);
            return Ok(LeaderState {
                leader_check: Arc::new(synctv_core::service::AlwaysLeader),
            });
        };

        // With Redis configured, cluster mode may be active.
        // Leader election failure in this scenario would be catastrophic:
        // multiple nodes could all believe they are the leader and run
        // singleton tasks (partition management, cleanup) simultaneously,
        // causing database corruption or inconsistent state.
        //
        // Therefore, we MUST NOT silently fall back to AlwaysLeader here.
        // Instead, we require a working leader elector and fail fast if
        // initialization fails.

        let leader_cancel = shutdown.register_token("leader_election");

        let (leader_elector, leader_election_handle) = {
            let leader_mode = infra.config.cluster.leader_election_mode.as_str();
            match leader_mode {
                #[cfg(feature = "k8s")]
                "k8s_lease" => {
                    info!("Using K8s Lease-based leader election");
                    // Set metrics: K8s mode (2)
                    synctv_core::metrics::cluster::LEADER_ELECTION_MODE.set(2);
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
                            // CRITICAL: Do not fall back to AlwaysLeader here!
                            // In cluster mode, this would cause split-brain.
                            // Instead, fail fast and let the operator fix the issue.
                            error!(
                                error = %e,
                                pod_name = %pod_name,
                                namespace = %namespace,
                                "CRITICAL: K8s leader election initialization failed"
                            );
                            error!(
                                "Required env vars: POD_NAME={}, POD_NAMESPACE={}",
                                std::env::var("POD_NAME")
                                    .unwrap_or_else(|_| "<not set>".to_string()),
                                std::env::var("POD_NAMESPACE")
                                    .unwrap_or_else(|_| "<not set>".to_string()),
                            );
                            error!(
                                "Ensure the service account has RBAC permissions: \
                                 verbs [get,create,update] on resource 'leases' in group 'coordination.k8s.io'"
                            );
                            error!(
                                "Refusing to start with AlwaysLeader fallback to prevent split-brain. \
                                 Fix the K8s configuration or switch to 'redis' leader election mode."
                            );
                            return Err(anyhow::anyhow!(
                                "K8s leader election initialization failed: {e}. \
                                 Cannot safely continue in cluster mode. \
                                 Either fix K8s RBAC/env vars or set cluster.leader_election_mode='redis'"
                            ));
                        }
                    }
                }
                #[cfg(not(feature = "k8s"))]
                "k8s_lease" => {
                    // CRITICAL: Feature not compiled in, cannot proceed.
                    // This is a configuration error that must be fixed.
                    error!(
                        "K8s Lease-based leader election requested but the 'k8s' feature \
                         was not compiled in. Rebuild with: cargo build --features k8s"
                    );
                    return Err(anyhow::anyhow!(
                        "K8s leader election mode 'k8s_lease' requires the 'k8s' feature. \
                         Rebuild with: cargo build --features k8s, or set cluster.leader_election_mode='redis'"
                    ));
                }
                _ => {
                    // Set metrics: Redis mode (1)
                    synctv_core::metrics::cluster::LEADER_ELECTION_MODE.set(1);
                    if leader_mode != "redis" {
                        warn!(
                            leader_election_mode = %leader_mode,
                            "Unknown leader election mode, falling back to 'redis'"
                        );
                    }

                    let plain_conn = redis_conn.read().await.clone();
                    let is_sentinel = matches!(
                        infra.config.redis.deployment_mode,
                        synctv_core::config::RedisDeploymentMode::Sentinel
                    );
                    let elector = LeaderElector::new(
                        plain_conn,
                        infra.node_id.clone(),
                        &infra.config.redis.key_prefix,
                        is_sentinel,
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

        // leader_elector is guaranteed to be Some here because we return early on error.
        // This eliminates the unsafe AlwaysLeader fallback that could cause split-brain.
        let leader_check: Arc<dyn synctv_core::service::LeaderCheck> = match &leader_elector {
            Some(elector) => Arc::new(elector.clone()),
            None => {
                // This branch should never be reached because all code paths either
                // set leader_elector to Some or return early with Err.
                // However, if a logic bug causes us to reach here, fail gracefully
                // instead of panicking, to avoid split-brain in cluster mode.
                error!(
                    "CRITICAL: leader_elector is None after initialization - this indicates a logic bug"
                );
                return Err(anyhow::anyhow!(
                    "leader_elector is None after successful initialization - this is a bug"
                ));
            }
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

        let notification_partition_manager =
            synctv_core::service::NotificationPartitionManager::new(
                infra.pool.clone(),
                leader.leader_check.clone(),
            );
        shutdown.register_task(
            "notification_partition",
            notification_partition_manager.start_auto_management(24, singleton_cancel.clone()),
        );
        info!("Notification partition management started (leader-gated, monthly granularity, check interval: 24 hours)");

        let cleanup_service = synctv_core::service::CleanupService::new(
            infra.pool.clone(),
            synctv_core::service::cleanup::CleanupConfig {
                room_ttl_seconds: core
                    .services
                    .settings_registry
                    .room_ttl
                    .get()
                    .unwrap_or(172800),
                ..synctv_core::service::cleanup::CleanupConfig::default()
            },
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
        )
        .with_settings_registry(core.services.settings_registry.clone());
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
    ) -> Result<ClusterState> {
        // Connection manager
        let connection_limits = ConnectionLimits {
            max_per_user: infra.config.connection_limits.max_per_user,
            max_per_room: infra.config.connection_limits.max_per_room,
            max_total: infra.config.connection_limits.max_total,
            idle_timeout: Duration::from_secs(infra.config.connection_limits.idle_timeout_seconds),
            max_duration: Duration::from_secs(infra.config.connection_limits.max_duration_seconds),
            webrtc_session_timeout: Duration::from_hours(2), // 2 hours (matches ConnectionLimits::default())
        };
        let connection_manager = ConnectionManager::new(connection_limits);
        info!(
            max_per_user = infra.config.connection_limits.max_per_user,
            max_per_room = infra.config.connection_limits.max_per_room,
            max_total = infra.config.connection_limits.max_total,
            "Connection manager initialized with configurable limits"
        );

        // ClusterManager (requires Redis)
        let permission_service = Some(core.services.room_service.permission_service().clone());

        let cluster_manager = if let Some(ref rh) = infra.redis_handles {
            // Create a cancellation token for the cluster manager that is a child
            // of the ShutdownCoordinator's token, so coordinator shutdown also
            // cancels all cluster background tasks.
            let cluster_cancel = shutdown.register_token("cluster_manager");

            let cluster_config = ClusterConfig {
                redis_client: Some(rh.client.clone()),
                redis_conn: Some(rh.conn_snapshot().await),
                node_id: infra.node_id.clone(),
                dedup_window: Duration::from_secs(
                    infra
                        .config
                        .cluster
                        .catchup_window_secs
                        .saturating_mul(3)
                        .max(900),
                ),
                cleanup_interval: Duration::from_secs(30),
                critical_channel_capacity: infra.config.cluster.critical_channel_capacity,
                publish_channel_capacity: infra.config.cluster.publish_channel_capacity,
                key_prefix: infra.config.redis.key_prefix.clone(),
                catchup_window_secs: infra.config.cluster.catchup_window_secs,
                stream_max_length: infra.config.cluster.stream_max_length,
                parent_cancel_token: Some(cluster_cancel),
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
                    // When cluster mode is explicitly enabled, ClusterManager failure
                    // is a fatal error - the cluster cannot operate without it.
                    // In standalone mode, we can continue without cluster features.
                    if infra.config.cluster.enabled {
                        return Err(anyhow::anyhow!(
                            "Failed to create ClusterManager (cluster mode): {e}. \
                             ClusterManager is required when cluster.enabled=true."
                        ));
                    }
                    error!("Failed to create ClusterManager: {}", e);
                    error!("Continuing in single-node mode");
                    None
                }
            }
        } else {
            info!("No Redis configured — skipping ClusterManager (single-node mode)");
            None
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

        // Cluster discovery (NodeRegistry, HealthMonitor) — requires Redis
        // D1 fix: When cluster is explicitly enabled, discovery failures are fatal.
        let (node_registry, health_monitor, _load_balancer, dns_refresh_handle) =
            if let (Some(ref cm), Some(ref rh)) = (&cluster_manager, &infra.redis_handles) {
                init_cluster_discovery(&infra.config, rh, cm, &connection_manager).await?
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

        Ok(ClusterState {
            cluster_manager,
            connection_manager,
            redis_publish_tx,
            node_registry,
            health_monitor,
        })
    }

    // -- Phase 7: Server components ---------------------------------------------

    async fn init_servers(
        infra: &Infrastructure,
        core: &CoreState,
        shutdown: &mut ShutdownCoordinator,
    ) -> Result<ServerComponents> {
        // Livestream
        let livestream_cancel = shutdown.register_token("livestream_tracker_cleanup");
        let (livestream_state, live_infra, background_handles) = init_livestream(
            &infra.config,
            &core.services,
            infra.redis_handles.as_ref(),
            &infra.node_id,
            livestream_cancel,
        )
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

        // WebRTC (STUN server and TURN health checker)
        let webrtc_cancel = shutdown.register_token("webrtc");
        let webrtc_components = init_webrtc(&infra.config, webrtc_cancel).await;

        // Media providers
        let pim = core.services.provider_instance_manager.clone();
        let alist_provider = Arc::new(AlistProvider::new(pim.clone()));
        let bilibili_provider = Arc::new(BilibiliProvider::new(pim.clone()));
        let emby_provider = Arc::new(EmbyProvider::new(pim));

        Ok(ServerComponents {
            livestream_state,
            live_infra,
            stun_server: webrtc_components.stun_server,
            turn_health_checker: webrtc_components.turn_health_checker,
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
            chat_service: core.services.chat_service.clone(),
            audit_service: core.services.audit_service.clone(),
            live_streaming_infrastructure: servers.live_infra,
            stun_server: servers.stun_server,
            turn_health_checker: servers.turn_health_checker,
            node_registry: cluster.node_registry,
            health_monitor: cluster.health_monitor,
            redis_client: infra.redis_handles.as_ref().map(|h| h.client.clone()),
            redis_conn: core.services.redis_conn.clone(), // already Option
            credential_encryption: core.services.credential_encryption,
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
