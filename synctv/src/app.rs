//! Application lifecycle management.
//!
//! `Application` encapsulates the entire `SyncTV` startup sequence as a series
//! of named phases, each producing a typed output. This replaces the
//! monolithic `main()` function with a readable, maintainable structure.

use std::sync::Arc;
use std::time::Duration;
use std::{future::Future, pin::Pin};

use anyhow::Result;
use sqlx::PgPool;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use synctv_cluster::leader::{LeaderRuntime, LeaderRuntimeBuilder, LeadershipEvent};
use synctv_cluster::sync::{ClusterConfig, ClusterManager, ConnectionLimits, ConnectionManager};
use synctv_core::{
    bootstrap::{
        bootstrap_root_user, database::init_database_with_cancel, has_any_admin_users, init_redis,
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
    leader_runtime: Arc<dyn LeaderRuntime>,
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
    providers: synctv_core::provider::ProviderSet,
}

type AsyncOnceTaskFactory = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// The assembled application, ready to be started.
pub struct Application {
    config: Config,
    pool: PgPool,
    services: Services,
    livestream_state: Option<LivestreamState>,
    shutdown: ShutdownCoordinator,
}

fn cluster_runtime_enabled(config: &Config) -> bool {
    config.cluster_runtime_enabled()
}

fn should_start_cache_invalidation_listener(config: &Config, has_redis: bool) -> bool {
    cluster_runtime_enabled(config) && has_redis
}

fn should_run_startup_partition_initialization(_config: &Config) -> bool {
    true
}

const fn should_continue_startup_after_root_bootstrap_failure(has_admin_user: bool) -> bool {
    has_admin_user
}

fn partition_startup_error(kind: &str, error: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!(
        "Failed to initialize {kind} during startup: {error}. \
         Startup must not continue before required partitions exist."
    )
}

fn spawn_on_leadership_gain(
    name: &'static str,
    leader_runtime: Arc<dyn LeaderRuntime>,
    cancel: tokio_util::sync::CancellationToken,
    task_factory: AsyncOnceTaskFactory,
) -> tokio::task::JoinHandle<()> {
    synctv_core::spawn::spawn_monitored(name, async move {
        let run_once = |task_factory: &AsyncOnceTaskFactory| task_factory();
        let mut last_ran_epoch = None;

        if leader_runtime.is_leader() {
            last_ran_epoch = Some(leader_runtime.leader_epoch());
            run_once(&task_factory).await;
        }

        let mut rx = leader_runtime.subscribe();

        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    info!("{name} cancelled while waiting for leadership transitions");
                    return;
                }
                event = rx.recv() => {
                    match event {
                        Ok(LeadershipEvent::Gained { epoch }) => {
                            if last_ran_epoch != Some(epoch) {
                                run_once(&task_factory).await;
                                last_ran_epoch = Some(epoch);
                            }
                        }
                        Ok(LeadershipEvent::Lost | LeadershipEvent::Vacancy) => {
                            last_ran_epoch = None;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let epoch = leader_runtime.leader_epoch();
                            if leader_runtime.is_leader() && last_ran_epoch != Some(epoch) {
                                run_once(&task_factory).await;
                                last_ran_epoch = Some(epoch);
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    }
                }
            }
        }
    })
}

fn build_connection_manager(
    limits: ConnectionLimits,
    redis_conn: Option<Arc<RwLock<redis::aio::ConnectionManager>>>,
    redis_key_prefix: &str,
    cluster_mode: bool,
) -> Result<ConnectionManager> {
    let manager = if cluster_mode {
        let conn = redis_conn.ok_or_else(|| {
            anyhow::anyhow!("cluster.enabled=true requires Redis-backed ConnectionManager wiring")
        })?;
        ConnectionManager::new(limits).with_shared_redis(conn, redis_key_prefix)
    } else {
        let manager = ConnectionManager::new(limits);
        manager.start();
        manager
    };
    Ok(manager)
}

async fn build_local_cluster_manager(
    config: &Config,
    node_id: &str,
    connection_manager: &ConnectionManager,
    cache_invalidation: Arc<CacheInvalidationService>,
    permission_service: Option<synctv_core::service::PermissionService>,
) -> Result<Arc<ClusterManager>> {
    let cluster_config = ClusterConfig {
        redis_client: None,
        redis_conn: None,
        cluster_enabled: false,
        node_id: node_id.to_string(),
        dedup_window: Duration::from_secs(
            config
                .cluster
                .catchup_window_secs
                .saturating_mul(3)
                .max(900),
        ),
        cleanup_interval: Duration::from_secs(30),
        critical_channel_capacity: config.cluster.critical_channel_capacity,
        publish_channel_capacity: config.cluster.publish_channel_capacity,
        key_prefix: config.redis.key_prefix.clone(),
        catchup_window_secs: config.cluster.catchup_window_secs,
        stream_max_length: config.cluster.stream_max_length,
        shared_redis_conn: None,
        parent_cancel_token: None,
    };

    let mut cluster_manager = ClusterManager::new(
        cluster_config,
        permission_service,
        Some((*cache_invalidation).clone()),
    )
    .await
    .map_err(|e| anyhow::anyhow!("Failed to create local ClusterManager: {e}"))?;
    cluster_manager.set_connection_manager(connection_manager.clone());

    Ok(Arc::new(cluster_manager))
}

fn require_cluster_redis_conn<'a>(
    redis_conn: Option<&'a Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>>,
) -> Result<&'a Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>> {
    redis_conn.ok_or_else(|| {
        anyhow::anyhow!(
            "startup invariant violated: cluster runtime reached without Redis connection wiring"
        )
    })
}

fn require_cluster_redis_handles<'a>(
    redis_handles: Option<&'a RedisHandles>,
) -> Result<&'a RedisHandles> {
    redis_handles.ok_or_else(|| {
        anyhow::anyhow!(
            "startup invariant violated: cluster runtime reached without Redis handle wiring"
        )
    })
}

fn validate_startup_config(config: &Config) -> Result<()> {
    config
        .validate()
        .map_err(|errors| anyhow::anyhow!(errors.join("; ")))
}

impl Application {
    /// Build the application through phased initialization.
    ///
    /// If any phase fails, all resources created in earlier phases are cleaned up
    /// via the `ShutdownCoordinator` before returning the error.
    pub async fn build(config: Config) -> Result<Self> {
        validate_startup_config(&config)?;

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
        let cluster = match Self::init_cluster(&infra, &core, &leader, &mut shutdown).await {
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
        let migration_lock: Arc<dyn synctv_core::service::MigrationLock> =
            if let Some(ref rh) = infra.redis_handles {
                info!("Using Redis distributed lock for migrations");
                let is_sentinel = matches!(
                    infra.config.redis.deployment_mode,
                    synctv_core::config::RedisDeploymentMode::Sentinel
                );
                Arc::new(synctv_core::service::DistributedLock::new_shared_with_mode(
                    rh.conn.clone(),
                    is_sentinel,
                ))
            } else {
                info!("Using PostgreSQL advisory lock for migrations");
                Arc::new(synctv_core::service::PgAdvisoryMigrationLock::new(
                    infra.pool.clone(),
                ))
            };
        crate::migrations::run_migrations(
            &infra.pool,
            migration_lock,
            &infra.config.redis.key_prefix,
            infra.config.cluster_runtime_enabled(),
        )
        .await?;

        // Bootstrap root user
        info!("Checking root user bootstrap...");
        if let Err(e) = bootstrap_root_user(&infra.pool, &infra.config.bootstrap).await {
            // Startup can continue only if the system already has an active
            // administrator account that can manage it.
            if should_continue_startup_after_root_bootstrap_failure(
                has_any_admin_users(&infra.pool).await,
            ) {
                warn!("Failed to bootstrap root user: {}", e);
                warn!("Existing administrator found, continuing startup");
            } else {
                return Err(anyhow::anyhow!(
                    "Failed to bootstrap root user and no active administrator exists: {e}. \
                     The system cannot operate without at least one administrator."
                ));
            }
        }

        if should_run_startup_partition_initialization(&infra.config) {
            info!("Initializing audit log partitions during startup...");
            synctv_core::service::ensure_audit_partitions_on_startup(&infra.pool)
                .await
                .map_err(|e| partition_startup_error("audit partitions", e))?;
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
        if should_start_cache_invalidation_listener(&infra.config, infra.redis_handles.is_some()) {
            if let Err(e) = cache_invalidation.start().await {
                // When cluster mode is explicitly enabled, cache invalidation failure
                // is a fatal error - the cluster cannot maintain cache consistency without it.
                // In standalone mode, we can continue with local-only caching.
                if cluster_runtime_enabled(&infra.config) {
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

        if should_run_startup_partition_initialization(&infra.config) {
            // Initialize chat message partitions (needs settings_registry from services)
            info!("Initializing chat message partitions during startup...");
            synctv_core::service::ensure_chat_partitions_on_startup(
                &infra.pool,
                synctv_services.settings_registry.clone(),
            )
            .await
            .map_err(|e| partition_startup_error("chat partitions", e))?;

            // Initialize notification partitions (monthly granularity)
            info!("Initializing notification partitions during startup...");
            synctv_core::service::ensure_notification_partitions_on_startup(&infra.pool)
                .await
                .map_err(|e| partition_startup_error("notification partitions", e))?;
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
        if !cluster_runtime_enabled(&infra.config) {
            info!("Cluster mode disabled — using unified standalone leader runtime");
            synctv_core::metrics::cluster::LEADER_ELECTION_MODE.set(0);
            synctv_core::metrics::cluster::LEADER_ELECTION_STATE.set(1);
            synctv_core::metrics::cluster::LEADER_ELECTION_EPOCH.set(0);
            synctv_core::metrics::cluster::LEADER_ELECTION_CONSECUTIVE_FAILURES.set(0);
            return Ok(LeaderState {
                leader_runtime: Arc::new(synctv_core::service::AlwaysLeader),
            });
        }

        let redis_conn = require_cluster_redis_conn(core.services.redis_conn.as_ref())?;

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

        let leader_mode = infra.config.cluster.leader_election_mode.as_str();
        let plain_conn = redis_conn.read().await.clone();
        let is_sentinel = matches!(
            infra.config.redis.deployment_mode,
            synctv_core::config::RedisDeploymentMode::Sentinel
        );

        let leader_elector = LeaderRuntimeBuilder::new(
            true,
            leader_mode,
            &infra.node_id,
            Some(plain_conn),
            &infra.config.redis.key_prefix,
            is_sentinel,
        )
        .build()
        .await
        .map_err(|e| {
            error!(error = %e, mode = leader_mode, "CRITICAL: leader election initialization failed");
            e
        })?;

        match leader_mode {
            "redis" => {
                synctv_core::metrics::cluster::LEADER_ELECTION_MODE.set(1);
                info!(
                    "Redis-based leader election started (node_id={})",
                    infra.node_id
                );
            }
            "k8s_lease" => {
                synctv_core::metrics::cluster::LEADER_ELECTION_MODE.set(2);
                info!("K8s Lease-based leader election started");
            }
            _ => unreachable!(
                "cluster.leader_election_mode is validated before startup: {leader_mode}"
            ),
        }

        let leader_election_handle = Some(leader_elector.start(leader_cancel.clone()));

        // Track leader election background task
        if let Some(handle) = leader_election_handle {
            shutdown.register_task("leader_election", handle);
        }

        // leader_elector is guaranteed to be Some here because we return early on error.
        // This eliminates the unsafe AlwaysLeader fallback that could cause split-brain.
        let leader_runtime: Arc<dyn LeaderRuntime> = Arc::new(leader_elector);

        Ok(LeaderState { leader_runtime })
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
            leader.leader_runtime.clone(),
        );
        shutdown.register_task(
            "audit_partition",
            audit_manager.start_auto_management(24, singleton_cancel.clone()),
        );
        info!("Audit log partition management started (leader-gated with fencing)");

        let chat_partition_manager = synctv_core::service::ChatPartitionManager::new(
            infra.pool.clone(),
            core.services.settings_registry.clone(),
            leader.leader_runtime.clone(),
        );
        shutdown.register_task(
            "chat_partition",
            chat_partition_manager.start_auto_management(24, singleton_cancel.clone()),
        );
        info!("Chat message partition management started (leader-gated with fencing, check interval: 24 hours)");

        let notification_partition_manager =
            synctv_core::service::NotificationPartitionManager::new(
                infra.pool.clone(),
                leader.leader_runtime.clone(),
            );
        shutdown.register_task(
            "notification_partition",
            notification_partition_manager.start_auto_management(24, singleton_cancel.clone()),
        );
        info!("Notification partition management started (leader-gated, monthly granularity, check interval: 24 hours)");

        let cleanup_service = synctv_core::service::CleanupService::new(
            infra.pool.clone(),
            synctv_core::service::cleanup::CleanupConfig::default(),
            leader.leader_runtime.clone(),
        )
        .with_settings_registry(core.services.settings_registry.clone());
        shutdown.register_task(
            "data_cleanup",
            cleanup_service.start_periodic(24, singleton_cancel.clone()),
        );
        info!("Periodic data cleanup started (leader-gated with fencing, interval: 24 hours, dynamic settings from registry)");

        let db_maintenance = synctv_core::service::DatabaseMaintenanceService::new(
            infra.pool.clone(),
            leader.leader_runtime.clone(),
        )
        .with_settings_registry(core.services.settings_registry.clone());
        shutdown.register_task(
            "db_maintenance",
            db_maintenance.spawn_maintenance_loop(singleton_cancel),
        );
        info!("Database maintenance service started (leader-gated: partitions every 12h, cleanups every 1h)");

        if cluster_runtime_enabled(&infra.config) {
            let pool = infra.pool.clone();
            let settings_registry = core.services.settings_registry.clone();
            let leader_runtime = leader.leader_runtime.clone();
            let cancel = shutdown.register_token("cluster_leader_startup_work");
            let task_factory: AsyncOnceTaskFactory = Arc::new(move || {
                let pool = pool.clone();
                let settings_registry = settings_registry.clone();
                Box::pin(async move {
                    info!("Leadership gained after startup; running deferred singleton maintenance");

                    let cleanup_service = synctv_core::service::CleanupService::new(
                        pool.clone(),
                        synctv_core::service::cleanup::CleanupConfig::default(),
                        Arc::new(synctv_core::service::AlwaysLeader),
                    )
                    .with_settings_registry(settings_registry.clone());
                    let cleanup_result = cleanup_service.run_all().await;
                    info!(
                        users_purged = cleanup_result.users_purged,
                        rooms_purged = cleanup_result.rooms_purged,
                        rooms_expired = cleanup_result.rooms_expired,
                        tokens_deleted = cleanup_result.tokens_deleted,
                        credentials_deleted = cleanup_result.credentials_deleted,
                        notifications_deleted = cleanup_result.notifications_deleted,
                        chat_messages_deleted = cleanup_result.chat_messages_deleted,
                        token_blacklist_deleted = cleanup_result.token_blacklist_deleted,
                        "Deferred cleanup completed after leadership gain"
                    );

                    let db_maintenance = synctv_core::service::DatabaseMaintenanceService::new(
                        pool,
                        Arc::new(synctv_core::service::AlwaysLeader),
                    )
                    .with_settings_registry(settings_registry);
                    db_maintenance.run_all_maintenance().await;
                    info!("Deferred database maintenance completed after leadership gain");
                })
            });

            shutdown.register_task(
                "cluster_leader_startup_work",
                spawn_on_leadership_gain(
                    "cluster_leader_startup_work",
                    leader_runtime,
                    cancel,
                    task_factory,
                ),
            );
        }
    }

    // -- Phase 6: Cluster infrastructure ----------------------------------------

    async fn init_cluster(
        infra: &Infrastructure,
        core: &CoreState,
        leader: &LeaderState,
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
        let connection_manager = build_connection_manager(
            connection_limits,
            infra.redis_handles.as_ref().map(|rh| rh.conn.clone()),
            &infra.config.redis.key_prefix,
            cluster_runtime_enabled(&infra.config),
        )?;
        info!(
            max_per_user = infra.config.connection_limits.max_per_user,
            max_per_room = infra.config.connection_limits.max_per_room,
            max_total = infra.config.connection_limits.max_total,
            "Connection manager initialized with configurable limits"
        );

        if !cluster_runtime_enabled(&infra.config) {
            let cluster_manager = build_local_cluster_manager(
                &infra.config,
                &infra.node_id,
                &connection_manager,
                core.cache_invalidation.clone(),
                Some(core.services.room_service.permission_service().clone()),
            )
            .await?;
            core.services
                .room_service
                .set_playback_cluster_broadcaster(Arc::new(ClusterPlaybackBroadcaster {
                    cluster_manager: cluster_manager.clone(),
                }));
            info!("Cluster mode disabled — initialized local-only ClusterManager");
            return Ok(ClusterState {
                cluster_manager: Some(cluster_manager),
                connection_manager,
                redis_publish_tx: None,
                node_registry: None,
                health_monitor: None,
            });
        }

        // ClusterManager (requires Redis)
        let permission_service = Some(core.services.room_service.permission_service().clone());
        let redis_handles = require_cluster_redis_handles(infra.redis_handles.as_ref())?;

        // Create a cancellation token for the cluster manager that is a child
        // of the ShutdownCoordinator's token, so coordinator shutdown also
        // cancels all cluster background tasks.
        let cluster_cancel = shutdown.register_token("cluster_manager");

        let cluster_config = ClusterConfig {
            redis_client: Some(redis_handles.client.clone()),
            redis_conn: Some(redis_handles.conn_snapshot().await),
            shared_redis_conn: Some(redis_handles.conn.clone()),
            cluster_enabled: infra.config.cluster.enabled,
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
            parent_cancel_token: Some(cluster_cancel.clone()),
        };
        let mut cluster_manager = match ClusterManager::new(
            cluster_config,
            permission_service,
            Some((*core.cache_invalidation).clone()),
        )
        .await
        {
            Ok(manager) => {
                info!("ClusterManager initialized with cross-replica cache invalidation");
                manager
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to create ClusterManager (cluster mode): {e}. \
                     ClusterManager is required when cluster.enabled=true."
                ));
            }
        };
        cluster_manager.set_connection_manager(connection_manager.clone());
        cluster_manager.set_leader_elector(leader.leader_runtime.clone());
        let cluster_manager = Arc::new(cluster_manager);

        // Wire cluster broadcaster into PlaybackService
        core.services
            .room_service
            .set_playback_cluster_broadcaster(Arc::new(ClusterPlaybackBroadcaster {
                cluster_manager: cluster_manager.clone(),
            }));
        info!("PlaybackService wired with cluster broadcaster");

        // Cluster discovery (NodeRegistry, HealthMonitor) — requires Redis
        // D1 fix: When cluster is explicitly enabled, discovery failures are fatal.
        let (node_registry, health_monitor, _load_balancer, dns_refresh_handle, dns_bridge_handle) =
            init_cluster_discovery(
                &infra.config,
                redis_handles,
                &cluster_manager,
                &connection_manager,
                cluster_cancel.clone(),
            )
            .await?;

        // Track DNS refresh task
        if let Some(handle) = dns_refresh_handle {
            shutdown.register_task("dns_refresh", handle);
        }
        if let Some(handle) = dns_bridge_handle {
            shutdown.register_task("dns_bridge", handle);
        }

        let redis_publish_tx = cluster_manager.redis_publish_tx().cloned();

        Ok(ClusterState {
            cluster_manager: Some(cluster_manager),
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
        let providers = synctv_core::provider::ProviderSet {
            alist: Arc::new(AlistProvider::new(pim.clone())),
            bilibili: Arc::new(BilibiliProvider::new(pim.clone())),
            emby: Arc::new(EmbyProvider::new(pim)),
            direct_url: Arc::new(synctv_core::provider::DirectUrlProvider::new()),
            rtmp: Arc::new(synctv_core::provider::RtmpProvider::new()),
            live_proxy: Arc::new(synctv_core::provider::LiveProxyProvider::new()),
        };

        Ok(ServerComponents {
            livestream_state,
            live_infra,
            stun_server: webrtc_components.stun_server,
            turn_health_checker: webrtc_components.turn_health_checker,
            providers,
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
            providers: servers.providers,
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

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_core::config::{
        BootstrapConfig, BufferSizesConfig, CacheConfig, ClusterChannelConfig,
        ConnectionLimitsConfig, DatabaseConfig, EmailConfig, GrpcRateLimitConfig,
        HttpRateLimitConfig, JwtConfig, LivestreamConfig, LoggingConfig, MediaProvidersConfig,
        OAuth2Config, PasswordComplexityConfig, RedisConfig, ServerConfig, WebRTCConfig,
    };
    use tokio::sync::broadcast;

    fn minimal_valid_startup_config() -> Config {
        Config {
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                grpc_port: 50051,
                http_port: 8080,
                enable_reflection: false,
                metrics_enabled: false,
                metrics_bearer_token: String::new(),
                grpc_max_message_size_bytes: 16 * 1024 * 1024,
                trusted_proxies: Vec::new(),
                cors_allowed_origins: Vec::new(),
                cluster_secret: String::new(),
                advertise_host: String::new(),
                shutdown_drain_timeout_seconds: 30,
            },
            database: DatabaseConfig::default(),
            redis: RedisConfig {
                url: "redis://127.0.0.1:6379".to_string(),
                ..RedisConfig::default()
            },
            jwt: JwtConfig {
                secret: "test-jwt-secret-key-for-testing-minimum-length".to_string(),
                ..JwtConfig::default()
            },
            logging: LoggingConfig::default(),
            livestream: LivestreamConfig {
                hls_shared_storage: true,
                hls_storage_path: "/var/lib/synctv/hls".to_string(),
                ..LivestreamConfig::default()
            },
            oauth2: OAuth2Config::default(),
            email: EmailConfig::default(),
            media_providers: MediaProvidersConfig::default(),
            webrtc: WebRTCConfig {
                stun_external_addr: "203.0.113.1:3478".to_string(),
                ..WebRTCConfig::default()
            },
            connection_limits: ConnectionLimitsConfig::default(),
            bootstrap: BootstrapConfig {
                create_root_user: false,
                root_username: String::new(),
                root_password: String::new(),
            },
            cluster: ClusterChannelConfig::default(),
            password_complexity: PasswordComplexityConfig::default(),
            buffer_sizes: BufferSizesConfig::default(),
            cache: CacheConfig::default(),
            messaging_rate_limits: synctv_core::config::MessagingRateLimitConfig::default(),
            http_rate_limits: HttpRateLimitConfig::default(),
            grpc_rate_limits: GrpcRateLimitConfig::default(),
        }
    }

    struct TestLeaderRuntime {
        is_leader: std::sync::atomic::AtomicBool,
        tx: broadcast::Sender<LeadershipEvent>,
    }

    impl TestLeaderRuntime {
        fn new(is_leader: bool) -> Self {
            let (tx, _rx) = broadcast::channel(8);
            Self {
                is_leader: std::sync::atomic::AtomicBool::new(is_leader),
                tx,
            }
        }

        fn gain_leadership(&self) {
            self.is_leader
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = self.tx.send(LeadershipEvent::Gained { epoch: 1 });
        }
    }

    impl synctv_cluster::leader::LeaderElect for TestLeaderRuntime {
        fn subscribe(&self) -> broadcast::Receiver<LeadershipEvent> {
            self.tx.subscribe()
        }
    }

    impl synctv_core::service::LeaderCheck for TestLeaderRuntime {
        fn is_leader(&self) -> bool {
            self.is_leader.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl synctv_cluster::leader::LeaderRuntime for TestLeaderRuntime {
        fn current_leader_identity(&self) -> Option<String> {
            self.is_leader
                .load(std::sync::atomic::Ordering::SeqCst)
                .then(|| "test-node".to_string())
        }

        fn leader_epoch(&self) -> u64 {
            if self.is_leader.load(std::sync::atomic::Ordering::SeqCst) {
                1
            } else {
                0
            }
        }

        async fn resign(&self) {
            self.is_leader
                .store(false, std::sync::atomic::Ordering::SeqCst);
            let _ = self.tx.send(LeadershipEvent::Lost);
        }
    }

    #[test]
    fn test_cluster_runtime_enabled_depends_only_on_cluster_flag() {
        let mut config = Config::default();
        config.server.cluster_secret = "shared-secret".to_string();

        assert!(
            !cluster_runtime_enabled(&config),
            "cluster_secret alone must not activate cluster runtime"
        );

        config.cluster.enabled = true;
        assert!(
            cluster_runtime_enabled(&config),
            "cluster.enabled=true must activate cluster runtime"
        );
    }

    #[test]
    fn test_cache_invalidation_listener_requires_cluster_mode_and_redis() {
        let mut config = Config::default();
        assert!(!should_start_cache_invalidation_listener(&config, false));
        assert!(!should_start_cache_invalidation_listener(&config, true));

        config.cluster.enabled = true;
        assert!(!should_start_cache_invalidation_listener(&config, false));
        assert!(should_start_cache_invalidation_listener(&config, true));
    }

    #[test]
    fn test_startup_partition_initialization_runs_in_all_modes() {
        let mut config = Config::default();
        config.server.cluster_secret = "shared-secret".to_string();

        assert!(
            should_run_startup_partition_initialization(&config),
            "standalone mode must initialize required partitions during startup"
        );

        config.cluster.enabled = true;
        assert!(
            should_run_startup_partition_initialization(&config),
            "cluster mode must also initialize required partitions before serving traffic"
        );
    }

    #[test]
    fn test_root_bootstrap_failure_only_allows_existing_admins() {
        assert!(
            should_continue_startup_after_root_bootstrap_failure(true),
            "existing admins should allow startup to continue"
        );
        assert!(
            !should_continue_startup_after_root_bootstrap_failure(false),
            "non-admin user presence must not mask bootstrap failure"
        );
    }

    #[test]
    fn test_validate_startup_config_rejects_cluster_mode_without_redis_before_bootstrap() {
        let mut config = minimal_valid_startup_config();
        config.cluster.enabled = true;
        config.server.cluster_secret = "test-cluster-secret-key-1234567890".to_string();
        config.redis.url.clear();

        let error = validate_startup_config(&config)
            .expect_err("startup preflight must reject cluster mode without Redis");

        assert!(
            error
                .to_string()
                .contains("cluster mode requires Redis to be configured"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn test_build_local_cluster_manager_supports_single_node_realtime_paths() {
        let config = minimal_valid_startup_config();
        let connection_manager =
            build_connection_manager(ConnectionLimits::default(), None, "test-local:", false)
                .expect("local connection manager should initialize");
        let cache_invalidation = Arc::new(CacheInvalidationService::new(
            None,
            "test-node".to_string(),
            "test-local:cache:invalidate".to_string(),
        ));

        let cluster_manager = build_local_cluster_manager(
            &config,
            "test-node",
            &connection_manager,
            cache_invalidation,
            None,
        )
        .await
        .expect("standalone mode should still wire a local ClusterManager");

        let metrics = cluster_manager.metrics();
        assert!(
            metrics.has_connection_manager,
            "single-node realtime paths need a wired connection manager"
        );
        assert!(
            !metrics.redis_enabled,
            "local-only cluster manager must not require Redis"
        );
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_init_cluster_injects_runtime_dependencies_into_cluster_manager() {
        let (_redis, client) = synctv_core_testing::start_redis_with_client().await;
        let conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .expect("Redis connection manager should be created");
        let shared_conn = Arc::new(RwLock::new(conn.clone()));

        let connection_manager = build_connection_manager(
            ConnectionLimits::default(),
            Some(shared_conn),
            "test-cluster:",
            true,
        )
        .expect("cluster mode should require and accept Redis-backed connection manager");

        let cluster_config = ClusterConfig {
            redis_client: Some(client),
            redis_conn: Some(conn),
            shared_redis_conn: None,
            cluster_enabled: true,
            node_id: "test-node".to_string(),
            dedup_window: Duration::from_secs(30),
            cleanup_interval: Duration::from_secs(30),
            critical_channel_capacity: 100,
            publish_channel_capacity: 100,
            key_prefix: "test-cluster:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 1000,
            parent_cancel_token: None,
        };

        let mut cluster_manager = ClusterManager::new(cluster_config, None, None)
            .await
            .expect("ClusterManager should initialize");
        let metrics = cluster_manager.metrics();
        assert!(!metrics.has_connection_manager);
        assert!(!metrics.has_leader_elector);

        cluster_manager.set_connection_manager(connection_manager);
        cluster_manager.set_leader_elector(Arc::new(synctv_core::service::AlwaysLeader));

        let metrics = cluster_manager.metrics();
        assert!(metrics.has_connection_manager);
        assert!(metrics.has_leader_elector);
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_build_connection_manager_wires_redis_in_cluster_mode() {
        use redis::AsyncCommands;
        use synctv_core::models::{RoomId, UserId};
        let (_redis, client) = synctv_core_testing::start_redis_with_client().await;
        let app_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .expect("App connection manager should be created");
        let shared_conn = Arc::new(RwLock::new(app_conn));

        let manager = build_connection_manager(
            ConnectionLimits::default(),
            Some(shared_conn),
            "test-app:",
            true,
        )
        .expect("cluster mode should build Redis-backed connection manager");

        manager
            .register(
                "conn-1".to_string(),
                UserId::from_string("user-1".to_string()),
            )
            .await
            .expect("Connection registration should succeed");
        manager
            .join_room("conn-1", RoomId::from_string("room-1".to_string()))
            .await
            .expect("Room join should succeed");

        let mut verify_conn = redis::aio::ConnectionManager::new(client)
            .await
            .expect("Verification connection should be created");
        let count: i64 = verify_conn
            .get("test-app:connections:room:room-1")
            .await
            .expect("Redis room counter should exist when ConnectionManager is wired");

        assert_eq!(
            count, 1,
            "Distributed room counter should be written to Redis"
        );

        manager.shutdown();
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_build_connection_manager_uses_shared_redis_handle_in_cluster_mode() {
        use redis::AsyncCommands;
        use synctv_core::models::{RoomId, UserId};

        let (_redis, client) = synctv_core_testing::start_redis_with_client().await;
        let first_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .expect("initial app connection manager should be created");
        let shared_conn = Arc::new(RwLock::new(first_conn));

        let manager = build_connection_manager(
            ConnectionLimits::default(),
            Some(shared_conn.clone()),
            "test-shared-app:",
            true,
        )
        .expect("cluster mode should preserve shared Redis handle wiring");

        manager
            .register(
                "conn-1".to_string(),
                UserId::from_string("user-1".to_string()),
            )
            .await
            .expect("connection registration should succeed");

        let replacement_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .expect("replacement app connection manager should be created");
        *shared_conn.write().await = replacement_conn;

        manager
            .join_room("conn-1", RoomId::from_string("room-2".to_string()))
            .await
            .expect("room join after shared connection swap should succeed");

        let mut verify_conn = redis::aio::ConnectionManager::new(client)
            .await
            .expect("verification connection should be created");
        let count: i64 = verify_conn
            .get("test-shared-app:connections:room:room-2")
            .await
            .expect("swapped shared Redis handle should still write distributed room counters");

        assert_eq!(
            count, 1,
            "cluster ConnectionManager must continue using the shared Redis handle after a hot swap"
        );

        manager.shutdown();
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_build_connection_manager_keeps_standalone_mode_local_even_with_redis() {
        use redis::AsyncCommands;
        use synctv_core::models::{RoomId, UserId};
        let (_redis, client) = synctv_core_testing::start_redis_with_client().await;
        let app_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .expect("App connection manager should be created");

        let manager = build_connection_manager(
            ConnectionLimits::default(),
            Some(Arc::new(RwLock::new(app_conn))),
            "test-standalone:",
            false,
        )
        .expect("standalone mode should build local connection manager");

        manager
            .register(
                "conn-1".to_string(),
                UserId::from_string("user-1".to_string()),
            )
            .await
            .expect("Standalone registration should succeed");
        manager
            .join_room("conn-1", RoomId::from_string("room-1".to_string()))
            .await
            .expect("Standalone room join should succeed");

        let mut verify_conn = redis::aio::ConnectionManager::new(client)
            .await
            .expect("Verification connection should be created");
        let count: Option<i64> = verify_conn
            .get("test-standalone:connections:room:room-1")
            .await
            .expect("Redis lookup should succeed");

        assert!(
            count.is_none(),
            "Standalone mode must not write distributed room counters just because Redis exists"
        );

        manager.shutdown();
    }

    #[test]
    fn test_build_connection_manager_returns_error_without_redis_in_cluster_mode() {
        let error = match build_connection_manager(ConnectionLimits::default(), None, "test:", true)
        {
            Ok(_) => panic!("cluster mode without Redis wiring must return an error"),
            Err(error) => error,
        };

        assert!(
            error
                .to_string()
                .contains("cluster.enabled=true requires Redis-backed ConnectionManager wiring"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_require_cluster_redis_handles_returns_error_instead_of_panicking() {
        let error = require_cluster_redis_handles(None)
            .expect_err("missing Redis handles in cluster runtime must return an error");

        assert!(
            error.to_string().contains(
                "startup invariant violated: cluster runtime reached without Redis handle wiring"
            ),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_require_cluster_redis_conn_returns_error_instead_of_panicking() {
        let error = require_cluster_redis_conn(None)
            .expect_err("missing Redis connection in cluster runtime must return an error");

        assert!(
            error
                .to_string()
                .contains("startup invariant violated: cluster runtime reached without Redis connection wiring"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn deferred_startup_work_runs_once_when_leadership_is_gained() {
        let leader_runtime = Arc::new(TestLeaderRuntime::new(false));
        let cancel = tokio_util::sync::CancellationToken::new();
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ran_clone = ran.clone();

        let handle = spawn_on_leadership_gain(
            "test_leadership_gain",
            leader_runtime.clone(),
            cancel.clone(),
            Arc::new(move || {
                let ran = ran_clone.clone();
                Box::pin(async move {
                    ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                })
            }),
        );

        leader_runtime.gain_leadership();
        tokio::task::yield_now().await;
        cancel.cancel();
        handle.await.expect("task should join");

        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn deferred_startup_work_runs_immediately_when_already_leader() {
        let leader_runtime = Arc::new(TestLeaderRuntime::new(true));
        let cancel = tokio_util::sync::CancellationToken::new();
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ran_clone = ran.clone();

        let handle = spawn_on_leadership_gain(
            "test_already_leader",
            leader_runtime,
            cancel.clone(),
            Arc::new(move || {
                let ran = ran_clone.clone();
                Box::pin(async move {
                    ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                })
            }),
        );

        tokio::task::yield_now().await;
        cancel.cancel();
        handle.await.expect("task should join");

        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn deferred_startup_work_runs_again_when_leadership_is_regained() {
        let leader_runtime = Arc::new(TestLeaderRuntime::new(false));
        let cancel = tokio_util::sync::CancellationToken::new();
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ran_clone = ran.clone();

        let handle = spawn_on_leadership_gain(
            "test_leadership_regained",
            leader_runtime.clone(),
            cancel.clone(),
            Arc::new(move || {
                let ran = ran_clone.clone();
                Box::pin(async move {
                    ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                })
            }),
        );

        leader_runtime.gain_leadership();
        tokio::task::yield_now().await;
        leader_runtime.resign().await;
        leader_runtime.gain_leadership();
        tokio::task::yield_now().await;

        cancel.cancel();
        handle.await.expect("task should join");

        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
