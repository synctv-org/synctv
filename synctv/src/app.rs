//! Application lifecycle management.
//!
//! `Application` encapsulates the entire `SyncTV` startup sequence as a series
//! of named phases, each producing a typed output. This replaces the
//! monolithic `main()` function with a readable, maintainable structure.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use std::{future::Future, pin::Pin};

use anyhow::Result;
use sqlx::PgPool;
use tracing::{error, info, warn};

use synctv_cluster::leader::{build_managed_leader_runtime, LeaderRuntime, LeadershipEvent};
use synctv_cluster::sync::{
    build_connection_runtime as build_cluster_connection_runtime,
    build_room_message_runtime as build_cluster_room_message_runtime, ClusterConfig,
    ClusterManager, ConnectionLimits, RoomMessageRuntime,
};
use synctv_core::{
    bootstrap::{
        bootstrap_root_user,
        database::init_database_with_cancel,
        has_any_admin_users, init_redis,
        services::{init_services_with_options, InitServicesOptions},
    },
    cache::{CacheInvalidationRuntime, KeyBuilder},
    provider::{AlistProvider, BilibiliProvider, EmbyProvider},
    service::auth::PasswordHasherService,
    Config, RedisConnectionRuntime,
};
use synctv_management::lifecycle::ManagementLifecycleController;

use synctv_api::cluster_fanout::{default_cluster_fanout_service, ClusterFanoutService};
use synctv_api::runtime::{
    ClusterRealtimeEventService, RealtimeConnectionService, RealtimeEventService,
};

use crate::bootstrap::cluster::{
    build_cluster_coordination_provider, init_cluster_discovery, ClusterCoordinationProvider,
    ClusterNodeActivator, DefaultClusterNodeActivator,
};
use crate::bootstrap::livestream::init_livestream;
use crate::bootstrap::node_id::generate_node_id;
use crate::bootstrap::webrtc::init_webrtc;
use crate::cluster_bridge::{
    room_event_to_cluster_event, ClusterMemberEventBroadcaster, ClusterPlaybackBroadcaster,
    ClusterPlaylistBroadcaster, LocalPlaylistBroadcaster,
};
use crate::server::{LivestreamState, Services, SyncTvServer};
use crate::shutdown::{
    AuditFlushHook, CacheInvalidationStopHook, ClusterManagerShutdownHook,
    HealthMonitorShutdownHook, PermissionServiceShutdownHook, PlaybackServiceShutdownHook,
    ProviderInvalidationHook, RoomSettingsServiceShutdownHook, SettingsListenHook,
    ShutdownCoordinator,
};

/// Infrastructure: Redis (optional), Database, `NodeID`.
struct Infrastructure {
    config: Config,
    pool: PgPool,
    shared_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
    cluster_coordination_provider: Option<Arc<dyn ClusterCoordinationProvider>>,
    node_id: String,
}

/// Core services from `synctv-core`.
struct CoreState {
    services: synctv_core::bootstrap::services::Services,
    cache_invalidation: Arc<dyn CacheInvalidationRuntime>,
}

/// Leader election and singleton background tasks.
struct LeaderState {
    leader_runtime: Arc<dyn LeaderRuntime>,
}

/// Cluster infrastructure.
struct ClusterState {
    cluster_fanout_service: Arc<dyn ClusterFanoutService>,
    realtime_connection_service: Arc<dyn RealtimeConnectionService>,
    realtime_event_service: Option<Arc<dyn RealtimeEventService>>,
    node_registry: Option<Arc<dyn synctv_cluster::discovery::ClusterNodeDirectory>>,
    health_monitor: Option<Arc<dyn synctv_cluster::discovery::ClusterHealthRuntime>>,
    cluster_activation: Option<Arc<dyn ClusterNodeActivator>>,
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

#[derive(Clone, Default)]
pub struct ApplicationBuildOptions {
    pub provider_test_address_overrides: HashMap<String, SocketAddr>,
    pub credential_encryption_hex_key_override: Option<String>,
    pub password_hasher_override: Option<Arc<dyn PasswordHasherService>>,
}

impl std::fmt::Debug for ApplicationBuildOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApplicationBuildOptions")
            .field(
                "provider_test_address_overrides",
                &self.provider_test_address_overrides,
            )
            .field(
                "credential_encryption_hex_key_override",
                &self
                    .credential_encryption_hex_key_override
                    .as_ref()
                    .map(|_| "<redacted>"),
            )
            .field(
                "password_hasher_override",
                &self.password_hasher_override.as_ref().map(|_| "<injected>"),
            )
            .finish()
    }
}

const fn cluster_runtime_enabled(config: &Config) -> bool {
    config.cluster_runtime_enabled()
}

const fn should_start_cache_invalidation_listener(config: &Config, has_redis: bool) -> bool {
    cluster_runtime_enabled(config) && has_redis
}

const fn should_run_startup_partition_initialization(_config: &Config) -> bool {
    true
}

const fn should_continue_startup_after_root_bootstrap_failure(has_admin_user: bool) -> bool {
    has_admin_user
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationLockStartupStrategy {
    Distributed,
    PgAdvisory,
    PgAdvisorySentinel,
}

const fn migration_lock_startup_strategy(
    deployment_mode: &synctv_core::config::RedisDeploymentMode,
    has_shared_runtime: bool,
) -> MigrationLockStartupStrategy {
    if matches!(
        deployment_mode,
        synctv_core::config::RedisDeploymentMode::Sentinel
    ) {
        MigrationLockStartupStrategy::PgAdvisorySentinel
    } else if has_shared_runtime {
        MigrationLockStartupStrategy::Distributed
    } else {
        MigrationLockStartupStrategy::PgAdvisory
    }
}

fn partition_startup_error(kind: &str, error: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!(
        "Failed to initialize {kind} during startup: {error}. \
         Startup must not continue before required partitions exist."
    )
}

async fn abort_running_leadership_task(running_task: &mut Option<tokio::task::JoinHandle<()>>) {
    if let Some(handle) = running_task.take() {
        handle.abort();
        let _ = handle.await;
    }
}

fn register_cache_invalidation_shutdown_hook(
    shutdown: &mut ShutdownCoordinator,
    service: Arc<dyn CacheInvalidationRuntime>,
) -> Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>> {
    let listener_task = Arc::new(tokio::sync::Mutex::new(None));
    shutdown.register_hook(CacheInvalidationStopHook {
        service,
        listener_task: listener_task.clone(),
    });
    listener_task
}

async fn ensure_administrator_bootstrap_precondition(
    pool: &PgPool,
    bootstrap_config: &synctv_core::config::BootstrapConfig,
) -> Result<()> {
    if bootstrap_config.create_root_user || has_any_admin_users(pool).await {
        return Ok(());
    }

    Err(anyhow::anyhow!(
        "bootstrap.create_root_user=false but no active administrator exists. \
         The system cannot operate without at least one administrator."
    ))
}

fn spawn_on_leadership_gain(
    name: &'static str,
    leader_runtime: Arc<dyn LeaderRuntime>,
    cancel: tokio_util::sync::CancellationToken,
    task_factory: AsyncOnceTaskFactory,
) -> tokio::task::JoinHandle<()> {
    synctv_core::spawn::spawn_monitored(name, async move {
        let spawn_task = |task_factory: &AsyncOnceTaskFactory| {
            let task_factory = task_factory.clone();
            synctv_core::spawn::spawn_monitored(name, async move {
                task_factory().await;
            })
        };
        let mut last_ran_epoch = None;
        let mut running_epoch = None;
        let mut running_task = None;

        if leader_runtime.is_leader() {
            let epoch = leader_runtime.leader_epoch();
            last_ran_epoch = Some(epoch);
            running_epoch = Some(epoch);
            running_task = Some(spawn_task(&task_factory));
        }

        let mut rx = leader_runtime.subscribe();

        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    abort_running_leadership_task(&mut running_task).await;
                    info!("{name} cancelled while waiting for leadership transitions");
                    return;
                }
                result = async {
                    match running_task.as_mut() {
                        Some(handle) => Some(handle.await),
                        None => None,
                    }
                }, if running_task.is_some() => {
                    running_epoch = None;
                    running_task = None;
                    if let Some(Err(err)) = result {
                        if err.is_panic() {
                            std::panic::resume_unwind(err.into_panic());
                        }
                        warn!(task = name, error = %err, "leadership-gated task ended unexpectedly");
                    }
                }
                event = rx.recv() => {
                    match event {
                        Ok(LeadershipEvent::Gained { epoch }) => {
                            if running_epoch == Some(epoch) || last_ran_epoch == Some(epoch) {
                                continue;
                            }
                            abort_running_leadership_task(&mut running_task).await;
                            last_ran_epoch = Some(epoch);
                            running_epoch = Some(epoch);
                            running_task = Some(spawn_task(&task_factory));
                        }
                        Ok(LeadershipEvent::Lost | LeadershipEvent::Vacancy) => {
                            last_ran_epoch = None;
                            running_epoch = None;
                            abort_running_leadership_task(&mut running_task).await;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let epoch = leader_runtime.leader_epoch();
                            if !leader_runtime.is_leader() {
                                last_ran_epoch = None;
                                running_epoch = None;
                                abort_running_leadership_task(&mut running_task).await;
                                continue;
                            }
                            if running_epoch == Some(epoch) || last_ran_epoch == Some(epoch) {
                                continue;
                            }
                            abort_running_leadership_task(&mut running_task).await;
                            last_ran_epoch = Some(epoch);
                            running_epoch = Some(epoch);
                            running_task = Some(spawn_task(&task_factory));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            abort_running_leadership_task(&mut running_task).await;
                            return;
                        }
                    }
                }
            }
        }
    })
}

fn build_realtime_state_profile(
    shared_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
    redis_key_prefix: &str,
    cluster_mode: bool,
) -> synctv_core::SharedStateProfile {
    synctv_core::SharedStateProfile::from_runtime(
        shared_runtime,
        redis_key_prefix,
        cluster_mode,
    )
}

fn build_connection_manager(
    limits: ConnectionLimits,
    profile: &synctv_core::SharedStateProfile,
) -> Result<Arc<dyn RealtimeConnectionService>> {
    build_cluster_connection_runtime(limits, profile)
        .map_err(|error| anyhow::anyhow!("Failed to initialize realtime connection runtime: {error}"))
}

fn build_room_message_runtime(
    profile: &synctv_core::SharedStateProfile,
) -> Result<Arc<dyn RoomMessageRuntime>> {
    build_cluster_room_message_runtime(profile)
        .map_err(|error| anyhow::anyhow!("Failed to initialize realtime message runtime: {error}"))
}

fn wire_room_service_cluster_broadcasters(
    room_service: &Arc<synctv_core::service::RoomService>,
    cluster_manager: Arc<ClusterManager>,
    playlist_broadcaster: Option<Arc<dyn synctv_core::service::PlaylistBroadcaster>>,
) {
    room_service.set_playback_cluster_broadcaster(Arc::new(ClusterPlaybackBroadcaster {
        cluster_manager: cluster_manager.clone(),
    }));
    if let Some(playlist_broadcaster) = playlist_broadcaster {
        room_service.set_playlist_cluster_broadcaster(playlist_broadcaster);
    }
    room_service
        .set_member_event_broadcaster(Arc::new(ClusterMemberEventBroadcaster { cluster_manager }));
}

fn start_room_notification_bridge(
    notification_service: Arc<synctv_core::service::NotificationService>,
    cluster_manager: Arc<ClusterManager>,
    shutdown: &mut ShutdownCoordinator,
) {
    let cancel = shutdown.register_token("room_notification_bridge");
    shutdown.register_task(
        "room_notification_bridge",
        synctv_core::spawn::spawn_monitored("room_notification_bridge", async move {
            let mut rx = notification_service.subscribe();
            loop {
                tokio::select! {
                    () = cancel.cancelled() => return,
                    event = rx.recv() => {
                        match event {
                            Ok((room_id, room_event)) => {
                                if let Some(cluster_event) = room_event_to_cluster_event(&room_id, &room_event) {
                                    let _ = cluster_manager.broadcast(cluster_event);
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                warn!(skipped, "room notification bridge lagged behind realtime events");
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                        }
                    }
                }
            }
        }),
    );
}

async fn build_local_cluster_manager(
    config: &Config,
    node_id: &str,
    connection_manager: Arc<dyn RealtimeConnectionService>,
    message_runtime: Arc<dyn RoomMessageRuntime>,
    cache_invalidation: Arc<dyn CacheInvalidationRuntime>,
    permission_service: Option<synctv_core::service::PermissionService>,
) -> Result<Arc<ClusterManager>> {
    let cluster_config = ClusterConfig {
        distributed_transport_factory: None,
        message_runtime,
        cluster_enabled: false,
        node_id: node_id.to_string(),
        dedup_window: Duration::from_secs(
            config
                .cluster
                .catchup_window_secs
                .saturating_mul(3)
                .max(900),
        ),
        critical_channel_capacity: config.cluster.critical_channel_capacity,
        publish_channel_capacity: config.cluster.publish_channel_capacity,
        key_prefix: config.redis.key_prefix.clone(),
        catchup_window_secs: config.cluster.catchup_window_secs,
        stream_max_length: config.cluster.stream_max_length,
        parent_cancel_token: None,
    };

    let mut cluster_manager = ClusterManager::new(
        cluster_config,
        permission_service,
        Some(cache_invalidation.clone()),
    )
    .await
    .map_err(|e| anyhow::anyhow!("Failed to create local ClusterManager: {e}"))?;
    cluster_manager.set_connection_manager(connection_manager);

    Ok(Arc::new(cluster_manager))
}

fn require_cluster_coordination_provider(
    provider: Option<&Arc<dyn ClusterCoordinationProvider>>,
) -> Result<Arc<dyn ClusterCoordinationProvider>> {
    provider.cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "startup invariant violated: cluster runtime reached without distributed backend wiring"
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
        Box::pin(Self::build_with_options(
            config,
            ApplicationBuildOptions::default(),
        ))
        .await
    }

    /// Build the application with explicit runtime wiring options.
    pub async fn build_with_options(
        config: Config,
        options: ApplicationBuildOptions,
    ) -> Result<Self> {
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
        let core = match Self::init_core_services(&infra, &mut shutdown, &options).await {
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
        let server = SyncTvServer::new(
            self.config,
            self.services,
            self.livestream_state,
            self.pool,
            Arc::new(ManagementLifecycleController::new()),
        );
        Box::pin(server.start_with_coordinator(self.shutdown)).await
    }

    /// Start all servers and stop when the supplied future resolves.
    ///
    /// This provides a deterministic shutdown mechanism for integration tests
    /// without changing the production startup path.
    ///
    /// Note: This method is intentionally kept even though it's only used by
    /// E2E tests in the `synctv/tests/` directory. The `#[allow(dead_code)]`
    /// attribute prevents warnings since tests are compiled separately.
    #[allow(dead_code)]
    pub async fn run_with_shutdown_signal<F>(self, shutdown_signal: F) -> Result<()>
    where
        F: Future<Output = ()> + Send,
    {
        let server = SyncTvServer::new(
            self.config,
            self.services,
            self.livestream_state,
            self.pool,
            Arc::new(ManagementLifecycleController::new()),
        );
        server
            .start_with_coordinator_and_shutdown_signal(self.shutdown, shutdown_signal)
            .await
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
        let redis_init = init_redis(&config, Some(sentinel_cancel)).await?;
        let shared_runtime = redis_init.connection_runtime();
        let cluster_coordination_provider = redis_init
            .coordination_runtime()
            .map(build_cluster_coordination_provider);
        if let Some(task) = redis_init.sentinel_health_check_task {
            shutdown.register_task("sentinel_master_health_check", task);
        }

        if shared_runtime.is_some() {
            info!("Redis connected");
        } else {
            info!("Running without Redis (standalone mode)");
        }
        if cluster_coordination_provider.is_some() {
            info!("Cluster coordination backend initialized");
        }

        // Database (with cancellable pool metrics task)
        let db_metrics_cancel = shutdown.register_token("db_pool_metrics");
        let db_init = init_database_with_cancel(&config, Some(db_metrics_cancel)).await?;
        if let Some(task) = db_init.metrics_task {
            shutdown.register_task("db_pool_metrics", task);
        }
        let pool = db_init.pool;

        Ok(Infrastructure {
            config,
            pool,
            shared_runtime,
            cluster_coordination_provider,
            node_id,
        })
    }

    // -- Phase 2: Schema --------------------------------------------------------

    async fn init_schema(infra: &Infrastructure) -> Result<()> {
        // Run migrations with appropriate lock strategy:
        // - Standalone Redis: Redis distributed lock
        // - Sentinel / no Redis: PostgreSQL advisory lock
        //
        // Sentinel failover can drop single-instance Redis locks, so correctness-
        // critical startup migrations must not rely on that path.
        let migration_lock = synctv_core::bootstrap::build_migration_lock(
            infra.pool.clone(),
            &infra.config,
            infra.shared_runtime.clone(),
        );
        match migration_lock_startup_strategy(
            &infra.config.redis.deployment_mode,
            infra.shared_runtime.is_some(),
        ) {
            MigrationLockStartupStrategy::PgAdvisorySentinel => {
                warn!(
                    "Redis Sentinel deployment detected; using PostgreSQL advisory lock for \
                     migrations because single-instance Redis locks are unsafe during failover"
                );
            }
            MigrationLockStartupStrategy::Distributed => {
                info!("Using distributed migration coordination lock");
            }
            MigrationLockStartupStrategy::PgAdvisory => {
                info!("Using PostgreSQL advisory lock for migrations");
            }
        }
        crate::migrations::run_migrations(
            &infra.pool,
            migration_lock,
            &infra.config.redis.key_prefix,
            infra.config.cluster_runtime_enabled(),
        )
        .await?;

        ensure_administrator_bootstrap_precondition(&infra.pool, &infra.config.bootstrap).await?;

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
        options: &ApplicationBuildOptions,
    ) -> Result<CoreState> {
        // Initialize CacheInvalidationService early (before init_services).
        // Uses the cluster node_id so invalidation messages are correctly attributed.
        // When Redis is not configured, cache invalidation operates in no-op mode.
        let key_builder = KeyBuilder::from_config(&infra.config);
        let cache_shared_state_profile = synctv_core::SharedStateProfile::from_runtime(
            infra.shared_runtime.clone(),
            &infra.config.redis.key_prefix,
            cluster_runtime_enabled(&infra.config),
        );
        let cache_invalidation = synctv_core::cache::cache_invalidation_runtime_from_shared_state_profile(
            &cache_shared_state_profile,
            infra.node_id.clone(),
            key_builder.cache_invalidation_stream(),
        )?;
        let cache_invalidation_listener_task =
            register_cache_invalidation_shutdown_hook(shutdown, cache_invalidation.clone());

        // Start the cache invalidation Redis subscriber BEFORE init_services.
        // Issue #44: subscriber must be running before any service publishes an
        // invalidation event to avoid dropped messages during initialization.
        if should_start_cache_invalidation_listener(&infra.config, infra.shared_runtime.is_some()) {
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
        let synctv_services = init_services_with_options(
            infra.pool.clone(),
            &infra.config,
            infra.shared_runtime.clone(),
            cache_invalidation.clone(),
            cache_invalidation_listener_task,
            InitServicesOptions {
                provider_test_address_overrides: options.provider_test_address_overrides.clone(),
                credential_encryption_hex_key_override: options
                    .credential_encryption_hex_key_override
                    .clone(),
                password_hasher_override: options.password_hasher_override.clone(),
            },
        )
        .await?;

        if synctv_services
            .room_service
            .permission_service()
            .has_invalidation_service()
        {
            synctv_services
                .room_service
                .permission_service()
                .start()
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Failed to start PermissionService invalidation runtime: {e}")
                })?;
            shutdown.register_hook(PermissionServiceShutdownHook {
                service: synctv_services.room_service.permission_service().clone(),
            });
        }

        if synctv_services
            .room_service
            .playback_service()
            .has_invalidation_service()
        {
            synctv_services
                .room_service
                .playback_service()
                .start()
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Failed to start PlaybackService invalidation runtime: {e}")
                })?;
            shutdown.register_hook(PlaybackServiceShutdownHook {
                service: synctv_services.room_service.playback_service().clone(),
            });
        }

        if synctv_services
            .room_service
            .room_settings_service()
            .has_invalidation_service()
        {
            synctv_services
                .room_service
                .room_settings_service()
                .start()
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to start RoomService room settings invalidation runtime: {e}"
                    )
                })?;
            shutdown.register_hook(RoomSettingsServiceShutdownHook {
                service: synctv_services.room_service.room_settings_service().clone(),
            });
        }

        if synctv_services
            .chat_service
            .room_settings_service()
            .has_invalidation_service()
        {
            synctv_services
                .chat_service
                .room_settings_service()
                .start()
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Failed to start RoomSettingsService invalidation runtime: {e}")
                })?;
            shutdown.register_hook(RoomSettingsServiceShutdownHook {
                service: synctv_services.chat_service.room_settings_service().clone(),
            });
        }

        // Track settings cancellation token and listen task in shutdown coordinator
        shutdown.track_token("settings", synctv_services.settings_cancel.clone());
        shutdown.register_hook(AuditFlushHook {
            handle: synctv_services.audit_flush_handle.clone(),
        });
        shutdown.register_hook(SettingsListenHook {
            task: synctv_services.settings_listen_task.clone(),
        });
        shutdown.register_hook(ProviderInvalidationHook {
            cancel: synctv_services.provider_invalidation_cancel.clone(),
            task: synctv_services.provider_invalidation_task.clone(),
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
        _core: &CoreState,
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

        let shared_state_profile = synctv_core::SharedStateProfile::from_runtime(
            infra.shared_runtime.clone(),
            &infra.config.redis.key_prefix,
            true,
        );

        #[cfg(feature = "k8s")]
        let leader_runtime = build_managed_leader_runtime(
            &infra.config,
            &infra.node_id,
            &shared_state_profile,
        )
        .await
        .map_err(|e| {
            error!(
                error = %e,
                mode = %infra.config.cluster.leader_election_mode,
                "CRITICAL: leader election initialization failed"
            );
            e
        })?;

        #[cfg(not(feature = "k8s"))]
        let leader_runtime =
            build_managed_leader_runtime(&infra.config, &infra.node_id, &shared_state_profile)
        .map_err(|e| {
            error!(
                error = %e,
                mode = %infra.config.cluster.leader_election_mode,
                "CRITICAL: leader election initialization failed"
            );
            e
        })?;

        match leader_runtime.mode_label() {
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
                "leader runtime mode is validated before startup: {}",
                leader_runtime.mode_label()
            ),
        }

        let leader_election_handle = Some(leader_runtime.start(leader_cancel.clone()));

        // Track leader election background task
        if let Some(handle) = leader_election_handle {
            shutdown.register_task("leader_election", handle);
        }

        let leader_runtime: Arc<dyn LeaderRuntime> = leader_runtime;

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

        let cleanup_config = synctv_core::service::cleanup::CleanupConfig::default();
        let cleanup_service = synctv_core::service::CleanupService::new(
            infra.pool.clone(),
            cleanup_config.clone(),
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
        .with_cleanup_config(cleanup_config.clone())
        .with_settings_registry(core.services.settings_registry.clone());
        shutdown.register_task(
            "db_maintenance",
            db_maintenance.spawn_maintenance_loop(singleton_cancel),
        );
        info!("Database maintenance service started (leader-gated cleanup tasks every 1h)");

        if cluster_runtime_enabled(&infra.config) {
            let pool = infra.pool.clone();
            let settings_registry = core.services.settings_registry.clone();
            let leader_runtime = leader.leader_runtime.clone();
            let deferred_leader_runtime = leader_runtime.clone();
            let deferred_cleanup_config = cleanup_config;
            let cancel = shutdown.register_token("cluster_leader_startup_work");
            let task_factory: AsyncOnceTaskFactory = Arc::new(move || {
                let pool = pool.clone();
                let settings_registry = settings_registry.clone();
                let cleanup_config = deferred_cleanup_config.clone();
                let leader_runtime = deferred_leader_runtime.clone();
                Box::pin(async move {
                    info!(
                        "Leadership gained after startup; running deferred singleton maintenance"
                    );

                    let cleanup_service = synctv_core::service::CleanupService::new(
                        pool.clone(),
                        cleanup_config.clone(),
                        leader_runtime.clone(),
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

                    let db_maintenance =
                        synctv_core::service::DatabaseMaintenanceService::new(pool, leader_runtime)
                            .with_cleanup_config(cleanup_config.clone())
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
        let cluster_runtime = cluster_runtime_enabled(&infra.config);
        let realtime_profile = build_realtime_state_profile(
            infra.shared_runtime.clone(),
            &infra.config.redis.key_prefix,
            cluster_runtime,
        );
        let realtime_connection_service =
            build_connection_manager(connection_limits, &realtime_profile)?;
        info!(
            max_per_user = infra.config.connection_limits.max_per_user,
            max_per_room = infra.config.connection_limits.max_per_room,
            max_total = infra.config.connection_limits.max_total,
            "Connection manager initialized with configurable limits"
        );

        if !cluster_runtime {
            let local_realtime_profile =
                build_realtime_state_profile(None, &infra.config.redis.key_prefix, false);
            let cluster_manager = build_local_cluster_manager(
                &infra.config,
                &infra.node_id,
                realtime_connection_service.clone(),
                build_room_message_runtime(&local_realtime_profile)?,
                core.cache_invalidation.clone(),
                Some(core.services.room_service.permission_service().clone()),
            )
            .await?;
            wire_room_service_cluster_broadcasters(
                &core.services.room_service,
                cluster_manager.clone(),
                Some(Arc::new(ClusterPlaylistBroadcaster {
                    cluster_manager: cluster_manager.clone(),
                })),
            );
            start_room_notification_bridge(
                core.services.room_notification_service.clone(),
                cluster_manager.clone(),
                shutdown,
            );
            info!("Cluster mode disabled — initialized local-only ClusterManager");
            return Ok(ClusterState {
                cluster_fanout_service: default_cluster_fanout_service(None, false),
                realtime_connection_service: realtime_connection_service.clone(),
                realtime_event_service: Some(Arc::new(ClusterRealtimeEventService::new(
                    cluster_manager,
                ))),
                node_registry: None,
                health_monitor: None,
                cluster_activation: None,
            });
        }

        // ClusterManager (requires Redis)
        let permission_service = Some(core.services.room_service.permission_service().clone());
        let cluster_backend =
            require_cluster_coordination_provider(infra.cluster_coordination_provider.as_ref())?;

        // Create a cancellation token for the cluster manager that is a child
        // of the ShutdownCoordinator's token, so coordinator shutdown also
        // cancels all cluster background tasks.
        let cluster_cancel = shutdown.register_token("cluster_manager");

        let cluster_config = ClusterConfig {
            distributed_transport_factory: Some(cluster_backend.distributed_transport_factory()),
            message_runtime: build_room_message_runtime(&realtime_profile)?,
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
            Some(core.cache_invalidation.clone()),
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
        cluster_manager.set_connection_manager(realtime_connection_service.clone());
        cluster_manager.set_leader_elector(leader.leader_runtime.clone());
        let cluster_manager = Arc::new(cluster_manager);
        shutdown.register_hook(ClusterManagerShutdownHook {
            manager: cluster_manager.clone(),
        });

        // Wire cluster broadcaster into PlaybackService
        wire_room_service_cluster_broadcasters(
            &core.services.room_service,
            cluster_manager.clone(),
            Some(Arc::new(LocalPlaylistBroadcaster {
                cluster_manager: cluster_manager.clone(),
            })),
        );
        info!("PlaybackService wired with cluster broadcaster");

        // Cluster discovery (NodeRegistry, HealthMonitor) — requires Redis
        // D1 fix: When cluster is explicitly enabled, discovery failures are fatal.
        let discovery = init_cluster_discovery(
            &infra.config,
            &cluster_backend.node_directory_factory(),
            &cluster_manager,
            realtime_connection_service.clone(),
            cluster_cancel.clone(),
        )
        .await?;

        for task in discovery.background_tasks {
            shutdown.register_task(task.name, task.handle);
        }
        shutdown.register_hook(HealthMonitorShutdownHook {
            monitor: discovery.health_monitor.clone(),
        });

        Ok(ClusterState {
            cluster_fanout_service: default_cluster_fanout_service(
                cluster_manager.redis_publish_tx().cloned(),
                true,
            ),
            realtime_connection_service: realtime_connection_service.clone(),
            realtime_event_service: Some(Arc::new(ClusterRealtimeEventService::new(
                cluster_manager.clone(),
            ))),
            node_registry: Some(discovery.registry.clone()),
            health_monitor: Some(discovery.health_monitor.clone()),
            cluster_activation: Some(Arc::new(DefaultClusterNodeActivator::new(
                infra.config.clone(),
                cluster_manager.clone(),
                realtime_connection_service.clone(),
                discovery.registry,
                discovery.health_monitor,
            )) as Arc<dyn ClusterNodeActivator>),
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
            infra.shared_runtime.clone(),
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
            cluster_fanout_service: cluster.cluster_fanout_service,
            rate_limiter: core.services.rate_limiter.clone(),
            rate_limit_config: core.services.rate_limit_config.clone(),
            content_filter: core.services.content_filter.clone(),
            realtime_connection_service: cluster.realtime_connection_service,
            realtime_event_service: cluster.realtime_event_service,
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
            ws_ticket_service: core.services.ws_ticket_service.clone(),
            publish_key_service: core.services.publish_key_service.clone(),
            notification_service: Some(core.services.notification_service.clone()),
            chat_service: core.services.chat_service.clone(),
            audit_service: core.services.audit_service.clone(),
            user_cache: core.services.user_cache.clone(),
            live_streaming_infrastructure: servers.live_infra,
            stun_server: servers.stun_server,
            turn_health_checker: servers.turn_health_checker,
            node_registry: cluster.node_registry,
            health_monitor: cluster.health_monitor,
            cluster_activation: cluster.cluster_activation,
            redis_runtime: core.services.redis_runtime(),
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
    use crate::bootstrap::cluster::{ClusterNodeActivator, DefaultClusterNodeActivator};
    use synctv_core::config::{
        BootstrapConfig, BufferSizesConfig, CacheConfig, ClusterChannelConfig,
        ConnectionLimitsConfig, DatabaseConfig, EmailConfig, GrpcRateLimitConfig,
        HttpRateLimitConfig, JwtConfig, LivestreamConfig, LoggingConfig, MediaProvidersConfig,
        OAuth2Config, PasswordComplexityConfig, RedisConfig, ServerConfig, WebRTCConfig,
    };
    use synctv_core::{
        cache::{KeyBuilder, UsernameCache},
        models::{SignupMethod, User, UserRole, UserStatus},
        repository::UserRepository,
        RedisConnectionRuntime, SharedRedisConnectionRuntime,
        service::{
            auth::hash_password, BruteForceProtection, InMemoryTokenBlacklistStore, UserService,
        },
    };
    use synctv_core_testing::test_redis_key_prefix;
    use tokio::sync::{broadcast, RwLock};

    #[tokio::test]
    async fn test_activate_cluster_node_registers_only_when_called() {
        let cluster_manager = Arc::new(
            ClusterManager::new(
                ClusterConfig {
                    distributed_transport_factory: None,
                    message_runtime: build_room_message_runtime(
                        &synctv_core::SharedStateProfile::from_runtime(
                            None,
                            "activation-test:",
                            false,
                        ),
                    )
                    .expect("local message runtime should initialize"),
                    cluster_enabled: false,
                    node_id: "activation-test-node".to_string(),
                    dedup_window: Duration::from_secs(1),
                    critical_channel_capacity: 16,
                    publish_channel_capacity: 16,
                    key_prefix: "activation-test:".to_string(),
                    catchup_window_secs: 60,
                    stream_max_length: 100,
                    parent_cancel_token: None,
                },
                None,
                None,
            )
            .await
            .expect("cluster manager should initialize"),
        );
        let connection_manager: Arc<dyn RealtimeConnectionService> = Arc::new(
            synctv_cluster::sync::ConnectionManager::new(ConnectionLimits::default()),
        );
        let registry = Arc::new(
            synctv_cluster::discovery::NodeRegistry::new_local_only(
                "activation-test-node".to_string(),
                30,
                "activation-test:",
            )
            .expect("local registry"),
        );
        let health_monitor = Arc::new(
            synctv_cluster::discovery::HealthMonitor::with_cancellation_token_and_probe_config(
                registry.clone(),
                15,
                &cluster_manager.cancel_token(),
                synctv_cluster::discovery::health_monitor::HealthProbeConfig::default(),
            ),
        );
        let mut config = minimal_valid_startup_config();
        config.server.advertise_host = "127.0.0.1".to_string();

        let before = registry
            .get_all_nodes()
            .await
            .expect("registry query should succeed");
        assert!(before.is_empty(), "registry should start empty");

        let registry_runtime: Arc<dyn synctv_cluster::ClusterNodeDirectory> = registry.clone();
        let health_runtime: Arc<dyn synctv_cluster::ClusterHealthRuntime> =
            health_monitor.clone();

        DefaultClusterNodeActivator::new(
            config,
            cluster_manager.clone(),
            connection_manager,
            registry_runtime,
            health_runtime,
        )
        .activate()
        .await
        .expect("activation should succeed");

        let after = registry
            .get_all_nodes()
            .await
            .expect("registry query should succeed");
        assert_eq!(after.len(), 1, "activation should register the local node");

        health_monitor.shutdown().await;
        cluster_manager.shutdown().await;
    }

    fn minimal_valid_startup_config() -> Config {
        Config {
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                port: 8080,
                enable_reflection: false,
                grpc_max_message_size_bytes: 16 * 1024 * 1024,
                trusted_proxies: Vec::new(),
                cors_allowed_origins: Vec::new(),
                cluster_secret: String::new(),
                advertise_host: String::new(),
                shutdown_drain_timeout_seconds: 30,
            },
            time: synctv_core::config::TimeConfig::default(),
            metrics: synctv_core::config::MetricsConfig::default(),
            management: synctv_core::config::ManagementConfig {
                enabled: false,
                ..synctv_core::config::ManagementConfig::default()
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
                root_email: String::new(),
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
        epoch: std::sync::atomic::AtomicU64,
        tx: broadcast::Sender<LeadershipEvent>,
    }

    impl TestLeaderRuntime {
        fn new(is_leader: bool) -> Self {
            let (tx, _rx) = broadcast::channel(8);
            Self {
                is_leader: std::sync::atomic::AtomicBool::new(is_leader),
                epoch: std::sync::atomic::AtomicU64::new(u64::from(is_leader)),
                tx,
            }
        }

        fn gain_leadership(&self) {
            let epoch = self.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            self.is_leader
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = self.tx.send(LeadershipEvent::Gained { epoch });
        }
    }

    impl synctv_cluster::leader::LeaderElect for TestLeaderRuntime {
        fn subscribe(&self) -> broadcast::Receiver<LeadershipEvent> {
            self.tx.subscribe()
        }
    }

    fn make_test_user_service(pool: PgPool) -> UserService {
        let jwt_service =
            synctv_core::service::JwtService::new("test-jwt-secret-key-for-testing-minimum-length")
                .expect("jwt service");
        let username_cache = UsernameCache::local_only("test:username:".to_string(), 64, 60);

        UserService::new(
            pool,
            jwt_service,
            username_cache,
            PasswordComplexityConfig::default(),
            Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
            KeyBuilder::new("test"),
            BruteForceProtection::in_memory("test".to_string()),
        )
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
            self.epoch.load(std::sync::atomic::Ordering::SeqCst)
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

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn startup_requires_existing_admin_when_root_bootstrap_is_disabled() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool_with_options_and_label(
            "synctv_test",
            "startup-admin-precondition-missing",
            20,
            Duration::from_secs(30),
        )
        .await;

        let error = ensure_administrator_bootstrap_precondition(
            &pool,
            &BootstrapConfig {
                create_root_user: false,
                root_username: "root".to_string(),
                root_email: String::new(),
                root_password: String::new(),
            },
        )
        .await
        .expect_err("startup must fail when no active administrator exists");

        assert!(
            error.to_string().contains("no active administrator exists"),
            "unexpected error: {error}"
        );
        pool.close().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn startup_allows_disabled_root_bootstrap_when_admin_already_exists() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool_with_options_and_label(
            "synctv_test",
            "startup-admin-precondition-existing",
            20,
            Duration::from_secs(30),
        )
        .await;

        let password_hash = hash_password("StrongPwd12345!")
            .await
            .expect("password hashing should succeed");
        let mut admin = User::new(
            "existing-admin".to_string(),
            Some("existing-admin@example.com".to_string()),
            password_hash,
            SignupMethod::AdminCreated,
        );
        admin.role = UserRole::Admin;
        admin.status = UserStatus::Active;
        UserRepository::new(pool.clone())
            .create(&admin)
            .await
            .expect("existing admin should be inserted");

        ensure_administrator_bootstrap_precondition(
            &pool,
            &BootstrapConfig {
                create_root_user: false,
                root_username: "root".to_string(),
                root_email: String::new(),
                root_password: String::new(),
            },
        )
        .await
        .expect("existing active admin should satisfy startup precondition");
        pool.close().await;
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

    #[test]
    fn test_sentinel_mode_uses_pg_advisory_migration_lock_strategy() {
        assert!(
            matches!(
                migration_lock_startup_strategy(
                    &synctv_core::config::RedisDeploymentMode::Sentinel,
                    true,
                ),
                MigrationLockStartupStrategy::PgAdvisorySentinel
            ),
            "Sentinel deployments must avoid single-instance Redis migration locks"
        );
    }

    #[tokio::test]
    async fn test_build_local_cluster_manager_supports_single_node_realtime_paths() {
        let config = minimal_valid_startup_config();
        let realtime_profile =
            synctv_core::SharedStateProfile::from_runtime(None, "test-local:", false);
        let connection_manager = build_connection_manager(ConnectionLimits::default(), &realtime_profile)
            .expect("local connection manager should initialize");
        let cache_invalidation = Arc::new(synctv_core::cache::CacheInvalidationService::new("test-node".to_string(),
            "test-local:cache:invalidate".to_string(),
        ));

        let cluster_manager = build_local_cluster_manager(
            &config,
            "test-node",
            connection_manager,
            build_room_message_runtime(&realtime_profile)
                .expect("local message runtime should initialize"),
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
            !metrics.distributed_enabled,
            "local-only cluster manager must not require Redis"
        );

        cluster_manager.shutdown().await;
    }

    #[tokio::test]
    async fn test_wire_room_service_cluster_broadcasters_sets_member_runtime_bridge() {
        let pool = PgPool::connect_lazy("postgresql://test").expect("lazy pool should build");
        let room_service = Arc::new(synctv_core::service::RoomService::new(
            pool.clone(),
            make_test_user_service(pool),
        ));
        let cluster_manager = Arc::new(
            ClusterManager::new(
                ClusterConfig {
                    distributed_transport_factory: None,
                    message_runtime: build_room_message_runtime(
                        &synctv_core::SharedStateProfile::from_runtime(None, "test:", false),
                    )
                    .expect("local message runtime should initialize"),
                    cluster_enabled: false,
                    node_id: "test-node".to_string(),
                    dedup_window: Duration::from_secs(30),
                    critical_channel_capacity: 16,
                    publish_channel_capacity: 16,
                    key_prefix: "test:".to_string(),
                    catchup_window_secs: 30,
                    stream_max_length: 128,
                    parent_cancel_token: None,
                },
                None,
                None,
            )
            .await
            .expect("local cluster manager should build"),
        );

        wire_room_service_cluster_broadcasters(
            &room_service,
            cluster_manager.clone(),
            Some(Arc::new(ClusterPlaylistBroadcaster {
                cluster_manager: cluster_manager.clone(),
            })),
        );

        assert!(
            room_service.has_member_event_broadcaster(),
            "cluster broadcaster wiring must cover member kicks/bans in addition to playback"
        );
        assert!(
            room_service.has_playlist_cluster_broadcaster(),
            "cluster broadcaster wiring must cover playlist lifecycle broadcasts"
        );

        cluster_manager.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_init_cluster_injects_runtime_dependencies_into_cluster_manager() {
        let (_redis, client) = synctv_core_testing::start_redis_with_client().await;
        let conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .expect("Redis connection manager should be created");
        let shared_conn = Arc::new(RwLock::new(conn.clone()));
        let shared_runtime: Arc<dyn RedisConnectionRuntime> =
            Arc::new(SharedRedisConnectionRuntime::new(shared_conn.clone()));
        let realtime_profile =
            synctv_core::SharedStateProfile::from_runtime(Some(shared_runtime), "test-cluster:", true);

        let connection_manager = build_connection_manager(
            ConnectionLimits::default(),
            &realtime_profile,
        )
        .expect("cluster mode should require and accept shared realtime connection state");

        let cluster_config = ClusterConfig {
            distributed_transport_factory: Some(
                synctv_cluster::build_cluster_message_transport_factory(
                    synctv_core::coordination_runtime_from_client(client),
                ),
            ),
            message_runtime: build_room_message_runtime(&realtime_profile)
                .expect("shared message runtime should initialize"),
            cluster_enabled: true,
            node_id: "test-node".to_string(),
            dedup_window: Duration::from_secs(30),
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

        cluster_manager.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_build_connection_manager_wires_redis_in_cluster_mode() {
        use redis::AsyncCommands;
        use synctv_core::models::{RoomId, UserId};
        let (_redis, client) = synctv_core_testing::start_redis_with_client().await;
        let prefix = test_redis_key_prefix("conn-mgr-wires");
        let app_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .expect("App connection manager should be created");
        let shared_conn = Arc::new(RwLock::new(app_conn));
        let shared_runtime: Arc<dyn RedisConnectionRuntime> =
            Arc::new(SharedRedisConnectionRuntime::new(shared_conn));
        let realtime_profile =
            synctv_core::SharedStateProfile::from_runtime(Some(shared_runtime), &prefix, true);

        let manager = build_connection_manager(ConnectionLimits::default(), &realtime_profile)
            .expect("cluster mode should build shared realtime connection manager");

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
            .get(format!("{prefix}connections:room:room-1"))
            .await
            .expect("Redis room counter should exist when ConnectionManager is wired");

        assert_eq!(
            count, 1,
            "Distributed room counter should be written to Redis"
        );

        manager.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_build_connection_manager_uses_shared_redis_handle_in_cluster_mode() {
        use redis::AsyncCommands;
        use synctv_core::models::{RoomId, UserId};

        let (_redis, client) = synctv_core_testing::start_redis_with_client().await;
        let prefix = test_redis_key_prefix("conn-mgr-shared");
        let first_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .expect("initial app connection manager should be created");
        let shared_conn = Arc::new(RwLock::new(first_conn));
        let shared_runtime: Arc<dyn RedisConnectionRuntime> =
            Arc::new(SharedRedisConnectionRuntime::new(shared_conn.clone()));
        let realtime_profile =
            synctv_core::SharedStateProfile::from_runtime(Some(shared_runtime), &prefix, true);

        let manager = build_connection_manager(ConnectionLimits::default(), &realtime_profile)
            .expect("cluster mode should preserve shared runtime wiring");

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
            .get(format!("{prefix}connections:room:room-2"))
            .await
            .expect("swapped shared Redis handle should still write distributed room counters");

        assert_eq!(
            count, 1,
            "cluster ConnectionManager must continue using the shared Redis handle after a hot swap"
        );

        manager.shutdown().await;
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
        let realtime_profile = synctv_core::SharedStateProfile::from_runtime(
            Some(Arc::new(SharedRedisConnectionRuntime::new(Arc::new(RwLock::new(
                app_conn,
            ))))),
            "test-standalone:",
            false,
        );

        let manager = build_connection_manager(ConnectionLimits::default(), &realtime_profile)
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

        manager.shutdown().await;
    }

    #[test]
    fn test_build_connection_manager_returns_error_without_redis_in_cluster_mode() {
        let realtime_profile = synctv_core::SharedStateProfile::from_runtime(None, "test:", true);
        let Err(error) = build_connection_manager(ConnectionLimits::default(), &realtime_profile)
        else {
            panic!("cluster mode without Redis wiring must return an error");
        };

        assert!(
            error
                .to_string()
                .contains("cluster runtime requires shared realtime connection state"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_require_cluster_coordination_provider_returns_error_instead_of_panicking() {
        let Err(error) = require_cluster_coordination_provider(None) else {
            panic!("missing distributed backends in cluster runtime must return an error");
        };

        assert!(
            error.to_string().contains(
                "startup invariant violated: cluster runtime reached without distributed backend wiring"
            ),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn deferred_startup_work_runs_once_when_leadership_is_gained() {
        let leader_runtime = Arc::new(TestLeaderRuntime::new(false));
        let cancel = tokio_util::sync::CancellationToken::new();
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let completed = Arc::new(tokio::sync::Notify::new());
        let ran_clone = ran.clone();
        let completed_clone = completed.clone();

        let handle = spawn_on_leadership_gain(
            "test_leadership_gain",
            leader_runtime.clone(),
            cancel.clone(),
            Arc::new(move || {
                let ran = ran_clone.clone();
                let completed = completed_clone.clone();
                Box::pin(async move {
                    ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    completed.notify_one();
                })
            }),
        );

        leader_runtime.gain_leadership();
        tokio::time::timeout(Duration::from_secs(1), completed.notified())
            .await
            .expect("leadership gain should run deferred startup work");
        cancel.cancel();
        handle.await.expect("task should join");

        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn deferred_startup_work_runs_immediately_when_already_leader() {
        let leader_runtime = Arc::new(TestLeaderRuntime::new(true));
        let cancel = tokio_util::sync::CancellationToken::new();
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let completed = Arc::new(tokio::sync::Notify::new());
        let ran_clone = ran.clone();
        let completed_clone = completed.clone();

        let handle = spawn_on_leadership_gain(
            "test_already_leader",
            leader_runtime,
            cancel.clone(),
            Arc::new(move || {
                let ran = ran_clone.clone();
                let completed = completed_clone.clone();
                Box::pin(async move {
                    ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    completed.notify_one();
                })
            }),
        );

        tokio::time::timeout(Duration::from_secs(1), completed.notified())
            .await
            .expect("already-leader startup work should run immediately");
        cancel.cancel();
        handle.await.expect("task should join");

        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn deferred_startup_work_runs_again_when_leadership_is_regained() {
        let leader_runtime = Arc::new(TestLeaderRuntime::new(false));
        let cancel = tokio_util::sync::CancellationToken::new();
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let completed = Arc::new(tokio::sync::Notify::new());
        let ran_clone = ran.clone();
        let completed_clone = completed.clone();

        let handle = spawn_on_leadership_gain(
            "test_leadership_regained",
            leader_runtime.clone(),
            cancel.clone(),
            Arc::new(move || {
                let ran = ran_clone.clone();
                let completed = completed_clone.clone();
                Box::pin(async move {
                    ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    completed.notify_one();
                })
            }),
        );

        leader_runtime.gain_leadership();
        tokio::time::timeout(Duration::from_secs(1), completed.notified())
            .await
            .expect("first leadership gain should run deferred startup work");
        leader_runtime.resign().await;
        leader_runtime.gain_leadership();
        tokio::time::timeout(Duration::from_secs(1), completed.notified())
            .await
            .expect("leadership regain should rerun deferred startup work");

        cancel.cancel();
        handle.await.expect("task should join");

        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn deferred_startup_work_is_aborted_when_leadership_is_lost_mid_run() {
        struct DropSignal(Arc<tokio::sync::Notify>);

        impl Drop for DropSignal {
            fn drop(&mut self) {
                self.0.notify_one();
            }
        }

        let leader_runtime = Arc::new(TestLeaderRuntime::new(true));
        let cancel = tokio_util::sync::CancellationToken::new();
        let started = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(tokio::sync::Notify::new());
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let started_clone = started.clone();
        let dropped_clone = dropped.clone();
        let completed_clone = completed.clone();

        let handle = spawn_on_leadership_gain(
            "test_leadership_loss_mid_run",
            leader_runtime.clone(),
            cancel.clone(),
            Arc::new(move || {
                let started = started_clone.clone();
                let dropped = dropped_clone.clone();
                let completed = completed_clone.clone();
                Box::pin(async move {
                    let _drop_signal = DropSignal(dropped);
                    started.notify_one();
                    std::future::pending::<()>().await;
                    completed.store(true, std::sync::atomic::Ordering::SeqCst);
                })
            }),
        );

        started.notified().await;
        leader_runtime.resign().await;
        tokio::time::timeout(Duration::from_millis(200), dropped.notified())
            .await
            .expect("running startup work must be aborted on leadership loss");

        cancel.cancel();
        handle.await.expect("task should join");

        assert!(
            !completed.load(std::sync::atomic::Ordering::SeqCst),
            "deferred startup work must not complete after leadership loss"
        );
    }

    #[tokio::test]
    async fn deferred_startup_work_aborts_previous_epoch_before_replacement_starts() {
        struct ActiveGuard(Arc<std::sync::atomic::AtomicUsize>);

        impl Drop for ActiveGuard {
            fn drop(&mut self) {
                self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        }

        let leader_runtime = Arc::new(TestLeaderRuntime::new(true));
        let cancel = tokio_util::sync::CancellationToken::new();
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Notify::new());

        let handle = spawn_on_leadership_gain(
            "test_leadership_epoch_replacement",
            leader_runtime.clone(),
            cancel.clone(),
            Arc::new({
                let active = active.clone();
                let max_active = max_active.clone();
                let starts = starts.clone();
                let started = started.clone();
                move || {
                    let active = active.clone();
                    let max_active = max_active.clone();
                    let starts = starts.clone();
                    let started = started.clone();
                    Box::pin(async move {
                        let current = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                        max_active.fetch_max(current, std::sync::atomic::Ordering::SeqCst);
                        starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let _guard = ActiveGuard(active);
                        started.notify_waiters();
                        std::future::pending::<()>().await;
                    })
                }
            }),
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            while starts.load(std::sync::atomic::Ordering::SeqCst) < 1 {
                started.notified().await;
            }
        })
        .await
        .expect("initial leader task should start");

        tokio::task::yield_now().await;
        leader_runtime.gain_leadership();

        tokio::time::timeout(Duration::from_secs(1), async {
            while starts.load(std::sync::atomic::Ordering::SeqCst) < 2 {
                started.notified().await;
            }
        })
        .await
        .expect("replacement leader task should start for the new epoch");

        cancel.cancel();
        handle.await.expect("task should join");

        assert_eq!(
            max_active.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "leadership-gated work must fence the previous epoch before starting the replacement"
        );
    }

    #[tokio::test]
    async fn cache_invalidation_shutdown_hook_registered_before_listener_start_still_cleans_up() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct DropFlag(Arc<AtomicBool>);

        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let service = Arc::new(synctv_core::cache::CacheInvalidationService::new("test-node".to_string(),
            "test:cache:invalidate".to_string(),
        ));
        let mut shutdown = ShutdownCoordinator::new(Duration::from_millis(50));
        let listener_task =
            register_cache_invalidation_shutdown_hook(&mut shutdown, service.clone());
        let dropped = Arc::new(AtomicBool::new(false));
        let dropped_for_task = Arc::clone(&dropped);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _drop_flag = DropFlag(dropped_for_task);
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        });
        *listener_task.lock().await = Some(task);

        started_rx
            .await
            .expect("cache invalidation listener task should signal startup before shutdown");
        shutdown.shutdown().await;

        tokio::time::timeout(Duration::from_secs(1), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cache invalidation listener task should be aborted when startup cleanup runs");

        assert!(
            dropped.load(Ordering::SeqCst),
            "startup-registered cache invalidation shutdown hook must abort listener tasks even if the task is attached later"
        );
    }
}
