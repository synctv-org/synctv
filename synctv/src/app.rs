//! Application lifecycle management.
//!
//! `Application` encapsulates the entire `SyncTV` startup sequence as a series
//! of named phases, each producing a typed output. This replaces the
//! monolithic `main()` function with a readable, maintainable structure.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use sqlx::PgPool;
use tracing::{error, info, warn};

use synctv_cluster::leader::{build_managed_leader_runtime, LeaderRuntime, LeadershipEvent};
use synctv_core::{
    cache::{CacheInvalidationRuntime, InvalidationMessage, KeyBuilder},
    repository::realtime_outbox::RealtimeOutboxRepository,
    service::RealtimeOutboxService,
    RedisConnectionRuntime,
};
use synctv_management::lifecycle::ManagementLifecycleController;
use synctv_realtime::sync::{
    build_connection_runtime as build_realtime_connection_runtime,
    build_room_message_runtime as build_realtime_room_message_runtime, CacheTarget,
    ConnectionLimits, RealtimeConfig, RealtimeEvent, RealtimeEventHandler, RealtimeManager,
    RealtimeManagerRuntime, RoomMessageRuntime,
};

use synctv_api::{distributed_realtime_fanout_service, local_realtime_fanout_service};
use synctv_realtime::fanout::{RealtimeEventService, RealtimeFanoutService};
use synctv_realtime::sync::ConnectionRuntime;

use crate::app_config::AppConfig as Config;
use crate::bootstrap::cluster::{
    build_cluster_coordination_provider, init_cluster_discovery, ClusterCoordinationProvider,
    ClusterNodeActivator, DefaultClusterNodeActivator,
};
use crate::bootstrap::livestream::init_livestream;
use crate::bootstrap::node_id::generate_node_id;
use crate::bootstrap::webrtc::init_webrtc;
use crate::bootstrap::{
    bootstrap_root_user, has_any_admin_users, init_database_with_read_pool_and_cancel, init_redis,
    init_services_with_options, DatabasePools, InitServicesOptions, RedisInitOptions,
};
use crate::email_outbox_dispatcher::start_email_outbox_dispatcher;
use crate::realtime_bridge::room_event_to_realtime_event;
use crate::realtime_outbox_dispatcher::start_realtime_outbox_dispatcher;
use crate::resource_options::{
    connection_limits_options, core_services_options, database_init_options,
    leader_runtime_options, redis_connection_options, root_user_bootstrap_options, time_options,
};
use crate::server::{LivestreamState, Services, SyncTvServer};
use crate::shutdown::{
    AuditFlushHook, CacheFenceRepairHook, CacheInvalidationStopHook, ProviderInvalidationHook,
    RoomSettingsServiceShutdownHook, SettingsListenHook, ShutdownCoordinator, SimpleShutdownHook,
};

/// Infrastructure: Redis (optional), Database, `NodeID`.
struct Infrastructure {
    config: Config,
    public_id_codec: Arc<synctv_adapter::PublicIdCodec>,
    pool: PgPool,
    database_pools: DatabasePools,
    shared_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
    cluster_coordination_provider: Option<Arc<dyn ClusterCoordinationProvider>>,
    node_id: String,
}

/// Core services from `synctv-core`.
struct CoreState {
    services: crate::bootstrap::Services,
    cache_invalidation: Arc<dyn CacheInvalidationRuntime>,
}

struct RuntimeModePlan {
    cluster_runtime: bool,
    cache_shared_state_profile: synctv_core::SharedStateProfile,
    realtime_shared_state_profile: synctv_core::SharedStateProfile,
    local_realtime_profile: synctv_core::SharedStateProfile,
    realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
}

impl RuntimeModePlan {
    fn from_infrastructure(infra: &Infrastructure) -> Self {
        let (
            cluster_runtime,
            cache_shared_state_profile,
            realtime_shared_state_profile,
            local_realtime_profile,
        ) = runtime_profiles_from_config(&infra.config, infra.shared_runtime.clone());

        Self {
            cluster_runtime,
            cache_shared_state_profile,
            realtime_shared_state_profile,
            local_realtime_profile,
            realtime_outbox: cluster_runtime
                .then(|| Arc::new(RealtimeOutboxRepository::new(infra.pool.clone()))),
        }
    }

    const fn cluster_runtime(&self) -> bool {
        self.cluster_runtime
    }

    fn realtime_outbox(&self) -> Arc<RealtimeOutboxRepository> {
        self.realtime_outbox
            .clone()
            .expect("cluster runtime requires a realtime outbox")
    }

    fn core_realtime_outbox(&self) -> Option<Arc<RealtimeOutboxRepository>> {
        self.realtime_outbox.clone()
    }
}

fn runtime_profiles_from_config(
    config: &Config,
    shared_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
) -> (
    bool,
    synctv_core::SharedStateProfile,
    synctv_core::SharedStateProfile,
    synctv_core::SharedStateProfile,
) {
    let cluster_runtime = cluster_runtime_enabled(config);
    let cache_shared_state_profile = synctv_core::SharedStateProfile::for_cluster_runtime(
        shared_runtime.clone(),
        &config.redis.key_prefix,
        cluster_runtime,
    );
    let realtime_shared_state_profile =
        build_realtime_state_profile(shared_runtime, &config.redis.key_prefix, cluster_runtime);
    let local_realtime_profile =
        build_realtime_state_profile(None, &config.redis.key_prefix, false);
    (
        cluster_runtime,
        cache_shared_state_profile,
        realtime_shared_state_profile,
        local_realtime_profile,
    )
}

/// Leader election and singleton background tasks.
struct LeaderState {
    leader_runtime: Arc<dyn LeaderRuntime>,
}

struct LeaderRuntimeCheck {
    leader_runtime: Arc<dyn LeaderRuntime>,
}

impl synctv_core::service::LeaderCheck for LeaderRuntimeCheck {
    fn is_leader(&self) -> bool {
        self.leader_runtime.is_leader()
    }
}

struct CoreRealtimeEventHandler {
    permission_service: Option<synctv_core::service::PermissionService>,
    cache_invalidation: Arc<dyn CacheInvalidationRuntime>,
}

impl CoreRealtimeEventHandler {
    fn new(
        cache_invalidation: Arc<dyn CacheInvalidationRuntime>,
        permission_service: Option<synctv_core::service::PermissionService>,
    ) -> Self {
        Self {
            permission_service,
            cache_invalidation,
        }
    }

    fn invalidate_cache_targets(&self, targets: &[CacheTarget]) {
        for target in targets {
            let msg = match target {
                CacheTarget::User { user_id } => InvalidationMessage::User {
                    user_id: user_id.to_string(),
                },
                CacheTarget::Username { user_id } => InvalidationMessage::Username {
                    user_id: user_id.to_string(),
                },
                CacheTarget::Room { room_id } => InvalidationMessage::Room {
                    room_id: room_id.to_string(),
                },
                CacheTarget::All => InvalidationMessage::All,
            };
            if let Err(error) = self.cache_invalidation.broadcast_local(msg) {
                warn!(
                    error = %error,
                    "Failed to dispatch cache invalidation from remote realtime event"
                );
            }
        }
    }
}

#[async_trait::async_trait]
impl RealtimeEventHandler for CoreRealtimeEventHandler {
    async fn handle_remote_event(
        &self,
        room_id: Option<synctv_core::models::RoomId>,
        event: &RealtimeEvent,
    ) {
        if let RealtimeEvent::CacheInvalidate { targets, .. } = event {
            self.invalidate_cache_targets(targets);
            return;
        }

        let Some(room_id) = room_id else {
            return;
        };

        if let Some(permission_service) = &self.permission_service {
            match event {
                RealtimeEvent::PermissionChanged { target_user_id, .. } => {
                    permission_service
                        .invalidate_cache(&room_id, target_user_id)
                        .await;
                }
                RealtimeEvent::UserLeft { user_id, .. } => {
                    permission_service.invalidate_cache(&room_id, user_id).await;
                }
                RealtimeEvent::RoomSettingsChanged { .. }
                | RealtimeEvent::RoomDeleted { .. }
                | RealtimeEvent::RoomBanned { .. }
                | RealtimeEvent::RoomOwnerInactive { .. } => {
                    permission_service.invalidate_room_cache(&room_id).await;
                }
                _ => {}
            }
        }

        match event {
            RealtimeEvent::RoomSettingsChanged { .. } | RealtimeEvent::RoomCreated { .. } => {
                self.invalidate_cache_targets(&[CacheTarget::Room { room_id }]);
            }
            RealtimeEvent::RoomDeleted { .. }
            | RealtimeEvent::RoomBanned { .. }
            | RealtimeEvent::RoomOwnerInactive { .. } => {
                self.invalidate_cache_targets(&[CacheTarget::Room { room_id }]);
                if let Err(error) =
                    self.cache_invalidation
                        .broadcast_local(InvalidationMessage::PlaybackState {
                            room_id: room_id.to_string(),
                        })
                {
                    warn!(
                        error = %error,
                        room_id = %room_id,
                        "Failed to dispatch playback cache invalidation from remote realtime event"
                    );
                }
            }
            _ => {}
        }
    }
}

/// Cluster infrastructure.
struct ClusterState {
    realtime_fanout_service: Arc<dyn RealtimeFanoutService>,
    realtime_connection_service: Arc<dyn ConnectionRuntime>,
    presence_service: Arc<synctv_core::service::OnlinePresenceService>,
    realtime_event_service: Arc<dyn RealtimeEventService>,
    node_registry: Option<Arc<dyn synctv_cluster::discovery::ClusterNodeDirectory>>,
    health_monitor: Option<Arc<dyn synctv_cluster::discovery::ClusterHealthRuntime>>,
    cluster_activation: Option<Arc<dyn ClusterNodeActivator>>,
}

/// Server components (livestream, WebRTC, providers).
struct ServerComponents {
    livestream_state: Option<LivestreamState>,
    live_infra: Option<Arc<synctv_livestream::LiveStreamingInfrastructure>>,
    stun_server: Option<Arc<synctv_core::service::StunServer>>,
    webrtc_status: synctv_core::service::WebRtcRuntimeStatus,
    providers: synctv_core::provider::ProviderSet,
    playback_duration_probe: Arc<synctv_core::service::PlaybackDurationProbeService>,
}

type AsyncOnceTaskFactory = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// The assembled application, ready to be started.
pub struct Application {
    config: Config,
    public_id_codec: Arc<synctv_adapter::PublicIdCodec>,
    clock: Arc<synctv_core::SyncedClock>,
    database_pools: DatabasePools,
    services: Services,
    livestream_state: Option<LivestreamState>,
    shutdown: ShutdownCoordinator,
}

#[derive(Clone, Default)]
pub struct ApplicationBuildOptions {
    pub provider_address_overrides: HashMap<String, SocketAddr>,
    pub credential_encryption_key_override: Option<String>,
    pub allow_password_registration: bool,
    pub public_id_config: synctv_adapter::PublicIdConfig,
}

impl std::fmt::Debug for ApplicationBuildOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApplicationBuildOptions")
            .field(
                "provider_address_overrides",
                &self.provider_address_overrides,
            )
            .field(
                "credential_encryption_key_override",
                &self
                    .credential_encryption_key_override
                    .as_ref()
                    .map(|_| "<redacted>"),
            )
            .field(
                "allow_password_registration",
                &self.allow_password_registration,
            )
            .finish()
    }
}

const fn cluster_runtime_enabled(config: &Config) -> bool {
    config.cluster_runtime_enabled()
}

#[cfg(test)]
const fn should_start_cache_invalidation_listener(config: &Config, has_redis: bool) -> bool {
    cluster_runtime_enabled(config) && has_redis
}

const fn should_start_cache_invalidation_listener_for_runtime(
    plan: &RuntimeModePlan,
    has_redis: bool,
) -> bool {
    plan.cluster_runtime() && has_redis
}

const fn should_run_startup_partition_initialization(_config: &Config) -> bool {
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

async fn abort_running_leadership_task(running_task: &mut Option<tokio::task::JoinHandle<()>>) {
    if let Some(handle) = running_task.take() {
        handle.abort();
        match handle.await {
            Ok(()) => {
                warn!("Leadership-gated startup task completed while abort was requested");
            }
            Err(error) if error.is_cancelled() => {
                tracing::debug!("Leadership-gated startup task cancelled after abort");
            }
            Err(error) => {
                warn!(
                    error = %error,
                    "Leadership-gated startup task failed while aborting"
                );
            }
        }
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
    bootstrap_config: &crate::bootstrap::RootUserBootstrapOptions,
) -> Result<()> {
    if bootstrap_config.create_root_user || has_any_admin_users(pool).await? {
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
    synctv_core::SharedStateProfile::for_cluster_runtime(
        shared_runtime,
        redis_key_prefix,
        cluster_mode,
    )
}

fn build_connection_manager(
    limits: ConnectionLimits,
    profile: &synctv_core::SharedStateProfile,
    presence_service: Arc<synctv_core::service::OnlinePresenceService>,
    node_id: &str,
) -> Result<Arc<dyn ConnectionRuntime>> {
    build_realtime_connection_runtime(limits, profile, presence_service, node_id.to_string())
        .map_err(|error| {
            anyhow::anyhow!("Failed to initialize realtime connection runtime: {error}")
        })
}

fn build_room_message_runtime(
    profile: &synctv_core::SharedStateProfile,
) -> Result<Arc<dyn RoomMessageRuntime>> {
    build_realtime_room_message_runtime(profile)
        .map_err(|error| anyhow::anyhow!("Failed to initialize realtime message runtime: {error}"))
}

struct LocalActivePlaybackRoomSource {
    connection_runtime: Arc<dyn ConnectionRuntime>,
}

#[async_trait::async_trait]
impl synctv_core::service::ActivePlaybackRoomSource for LocalActivePlaybackRoomSource {
    async fn active_room_ids(&self) -> synctv_core::Result<Vec<synctv_core::models::RoomId>> {
        // Playback lifecycle ownership is local process state. A room_creation is active
        // for these workers when this process has at least one realtime
        // connection for it. Presence, hot-room_creation indexes, and cluster-wide room_creation
        // stats are list/analytics inputs; this adapter is the scheduler
        // boundary for duration probing, auto-advance, and live playback
        // resource lifecycle work. Storage locks and playback-state version
        // writes converge duplicate attempts when the same room_creation is active on
        // several nodes.
        //
        // Maintenance contract:
        // docs/src/content/docs/en/develop/implementation-contracts.mdx
        // docs/src/content/docs/en/concepts/playback-background-workers.mdx
        Ok(self.connection_runtime.active_room_ids())
    }
}

fn start_room_notification_bridge(
    notification_service: Arc<synctv_core::service::NotificationService>,
    realtime_manager: Arc<RealtimeManager>,
    shutdown: &mut ShutdownCoordinator,
) {
    let cancel = shutdown.register_token("room_notification_bridge");
    shutdown.register_task(
        "room_notification_bridge",
        synctv_core::spawn::spawn_monitored("room_notification_bridge", async move {
            let mut rx = notification_service.subscribe();
            let mut committed_realtime_rx =
                notification_service.subscribe_committed_realtime_events();
            loop {
                tokio::select! {
                    () = cancel.cancelled() => return,
                    event = rx.recv() => {
                        match event {
                            Ok((room_id, room_event)) => {
                                if let Some(realtime_event) = room_event_to_realtime_event(&room_id, &room_event) {
                                    let _ = realtime_manager.broadcast(realtime_event);
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                warn!(skipped, "room creation notification bridge lagged behind realtime events");
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                        }
                    }
                    event = committed_realtime_rx.recv() => {
                        match event {
                            Ok(event) => {
                                realtime_manager.broadcast_local(event);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                warn!(skipped, "committed realtime bridge lagged behind events");
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                        }
                    }
                }
            }
        }),
    );
}

async fn build_local_realtime_manager(
    config: &Config,
    node_id: &str,
    connection_manager: Arc<dyn ConnectionRuntime>,
    message_runtime: Arc<dyn RoomMessageRuntime>,
    cache_invalidation: Arc<dyn CacheInvalidationRuntime>,
    permission_service: Option<synctv_core::service::PermissionService>,
) -> Result<Arc<RealtimeManager>> {
    let realtime_config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime,
        distributed_enabled: false,
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
        event_handler: Some(Arc::new(CoreRealtimeEventHandler::new(
            cache_invalidation,
            permission_service,
        ))),
        parent_cancel_token: None,
    };

    let realtime_manager = RealtimeManager::new_with_runtime(
        realtime_config,
        RealtimeManagerRuntime {
            connection_runtime: Some(connection_manager),
            leader_runtime: None,
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("Failed to create local RealtimeManager: {e}"))?;

    Ok(Arc::new(realtime_manager))
}

fn require_cluster_coordination_provider(
    provider: Option<&Arc<dyn ClusterCoordinationProvider>>,
) -> Result<Arc<dyn ClusterCoordinationProvider>> {
    provider.cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "startup invariant violated: realtime runtime reached without distributed backend wiring"
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
        let public_id_codec = Arc::new(
            synctv_adapter::PublicIdCodec::from_config(&options.public_id_config)
                .map_err(|error| anyhow::anyhow!("Invalid public ID configuration: {error}"))?,
        );

        let shutdown_budget =
            std::time::Duration::from_secs(config.server.shutdown_drain_timeout_seconds);
        let mut shutdown = ShutdownCoordinator::new(shutdown_budget);

        let clock = Self::init_application_clock(&config, &mut shutdown).await?;

        // Phase 1: Infrastructure (Redis, Database, NodeID)
        let infra = match Self::init_infrastructure(config, public_id_codec, &mut shutdown).await {
            Ok(infra) => infra,
            Err(e) => {
                shutdown.shutdown().await;
                return Err(e);
            }
        };
        let runtime_plan = RuntimeModePlan::from_infrastructure(&infra);

        // Phase 2: Schema (migrations, root user, partitions)
        if let Err(e) = Self::init_schema(&infra).await {
            shutdown.shutdown().await;
            return Err(e);
        }

        // Phase 3: Core services
        let core = match Self::init_core_services(
            &infra,
            &runtime_plan,
            clock.clone(),
            &mut shutdown,
            &options,
        )
        .await
        {
            Ok(core) => core,
            Err(e) => {
                shutdown.shutdown().await;
                return Err(e);
            }
        };
        if options.allow_password_registration {
            let mut settings = core.services.runtime_settings_store.runtime_settings()?;
            settings.user.enable_password_signup = true;
            core.services
                .runtime_settings_store
                .persist_runtime_settings(&settings)
                .await?;
        }

        // Phase 4: Leader election and singleton tasks
        let leader =
            match Self::init_leader_election(&infra, &runtime_plan, &core, &mut shutdown).await {
                Ok(leader) => leader,
                Err(e) => {
                    shutdown.shutdown().await;
                    return Err(e);
                }
            };

        // Phase 5: Singleton background tasks
        Self::start_email_outbox_dispatcher(&infra, &core, &leader, &mut shutdown);
        Self::start_singleton_tasks(&infra, &runtime_plan, &core, &leader, &mut shutdown);

        // Phase 6: Cluster infrastructure
        let cluster =
            match Self::init_cluster(&infra, &runtime_plan, &core, &leader, &mut shutdown).await {
                Ok(cluster) => cluster,
                Err(e) => {
                    shutdown.shutdown().await;
                    return Err(e);
                }
            };

        Self::start_playback_background_tasks(&infra, &core, &cluster, &mut shutdown);

        // Phase 7: Server components (livestream, WebRTC, providers)
        let servers = match Self::init_servers(&infra, &core, &leader, &mut shutdown).await {
            Ok(servers) => servers,
            Err(e) => {
                shutdown.shutdown().await;
                return Err(e);
            }
        };

        // Assemble
        Ok(Self::assemble(
            infra, core, cluster, servers, clock, shutdown,
        ))
    }

    async fn init_application_clock(
        config: &Config,
        shutdown: &mut ShutdownCoordinator,
    ) -> Result<Arc<synctv_core::SyncedClock>> {
        let clock = Arc::new(synctv_core::SyncedClock::from_options(&time_options(
            config,
        )));
        if clock.enabled() {
            clock.sync_once().await.map_err(|error| {
                anyhow::anyhow!("initial application clock synchronization failed: {error}")
            })?;
        }
        let cancel = shutdown.register_token("application_clock_sync");
        if let Some(task) = clock.start(cancel) {
            shutdown.register_task("application_clock_sync", task);
        }
        Ok(clock)
    }

    /// Start all servers and wait for shutdown.
    pub async fn run(self) -> Result<()> {
        let server = SyncTvServer::new(
            self.config,
            self.public_id_codec,
            self.clock,
            self.services,
            self.livestream_state,
            self.database_pools,
            Arc::new(ManagementLifecycleController::new()),
        );
        Box::pin(server.start_with_coordinator(self.shutdown)).await
    }

    /// Start all servers and stop when the supplied future resolves.
    ///
    /// This provides a deterministic shutdown mechanism for integration tests
    /// without changing the production startup path.
    pub async fn run_with_shutdown_signal<F>(self, shutdown_signal: F) -> Result<()>
    where
        F: Future<Output = ()> + Send,
    {
        let server = SyncTvServer::new(
            self.config,
            self.public_id_codec,
            self.clock,
            self.services,
            self.livestream_state,
            self.database_pools,
            Arc::new(ManagementLifecycleController::new()),
        );
        server
            .start_with_coordinator_and_shutdown_signal(self.shutdown, shutdown_signal)
            .await
    }

    async fn init_infrastructure(
        config: Config,
        public_id_codec: Arc<synctv_adapter::PublicIdCodec>,
        shutdown: &mut ShutdownCoordinator,
    ) -> Result<Infrastructure> {
        // Generate node_id once for the entire process
        let node_id = generate_node_id();
        info!("Node ID: {node_id}");

        // Redis (optional in standalone mode, mandatory in distributed mode)
        let sentinel_cancel = shutdown.register_token("sentinel_health_check");
        let redis_init = init_redis(
            &RedisInitOptions {
                redis: redis_connection_options(&config),
            },
            Some(sentinel_cancel),
        )
        .await?;
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
        let db_init = init_database_with_read_pool_and_cancel(
            &database_init_options(&config),
            Some(db_metrics_cancel),
        )
        .await?;
        if let Some(task) = db_init.metrics_task {
            shutdown.register_task("db_pool_metrics", task);
        }
        let pool = db_init.pool;
        let database_pools = db_init.pools;

        Ok(Infrastructure {
            config,
            public_id_codec,
            pool,
            database_pools,
            shared_runtime,
            cluster_coordination_provider,
            node_id,
        })
    }

    async fn init_schema(infra: &Infrastructure) -> Result<()> {
        crate::migrations::run_migrations(&infra.pool).await?;

        let root_bootstrap = root_user_bootstrap_options(&infra.config);
        ensure_administrator_bootstrap_precondition(&infra.pool, &root_bootstrap).await?;

        // Bootstrap root user
        info!("Checking root user bootstrap...");
        if let Err(e) = bootstrap_root_user(
            &infra.pool,
            &root_bootstrap,
            &infra.config.security.opaque_server_setup_secret,
        )
        .await
        {
            // Startup can continue only if the system already has an active
            // administrator account that can manage it.
            if should_continue_startup_after_root_bootstrap_failure(
                has_any_admin_users(&infra.pool).await?,
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

    async fn init_core_services(
        infra: &Infrastructure,
        runtime_plan: &RuntimeModePlan,
        clock: Arc<synctv_core::SyncedClock>,
        shutdown: &mut ShutdownCoordinator,
        options: &ApplicationBuildOptions,
    ) -> Result<CoreState> {
        // Initialize CacheInvalidationService early (before init_services).
        // Uses the cluster node_id so invalidation messages are correctly attributed.
        // When Redis is not configured, cache invalidation operates in no-op mode.
        let key_builder = KeyBuilder::new(infra.config.redis.key_prefix.clone());
        let cache_invalidation: Arc<dyn CacheInvalidationRuntime> = Arc::new(
            synctv_core::cache::CacheInvalidationService::from_shared_state_profile(
                &runtime_plan.cache_shared_state_profile,
                infra.node_id.clone(),
                key_builder.cache_invalidation_stream(),
            )?,
        );
        let cache_invalidation_listener_task =
            register_cache_invalidation_shutdown_hook(shutdown, cache_invalidation.clone());

        // Start the cache invalidation Redis subscriber BEFORE init_services.
        // subscriber must be running before any service publishes an
        // invalidation event to avoid dropped messages during initialization.
        if should_start_cache_invalidation_listener_for_runtime(
            runtime_plan,
            infra.shared_runtime.is_some(),
        ) {
            if let Err(e) = cache_invalidation.start().await {
                // When distributed mode is explicitly enabled, cache invalidation failure
                // is a fatal error - the cluster cannot maintain cache consistency without it.
                // In standalone mode, we can continue with local-only caching.
                if runtime_plan.cluster_runtime() {
                    return Err(anyhow::anyhow!(
                        "Failed to start cache invalidation listener (distributed mode): {e}. \
                         Cache consistency is required when cluster.enabled=true."
                    ));
                }
                warn!("Failed to start cache invalidation listener (continuing in standalone mode): {}", e);
            }
        }

        // Initialize core services
        let credential_encryption_key =
            options
                .credential_encryption_key_override
                .clone()
                .or_else(|| {
                    (!infra.config.security.credential_encryption_key.is_empty())
                        .then(|| infra.config.security.credential_encryption_key.clone())
                });
        let synctv_services = init_services_with_options(
            infra.pool.clone(),
            &core_services_options(&infra.config),
            infra.shared_runtime.clone(),
            cache_invalidation.clone(),
            cache_invalidation_listener_task,
            InitServicesOptions {
                clock,
                provider_address_overrides: options.provider_address_overrides.clone(),
                ssrf_guard: infra.config.security.ssrf_guard(),
                credential_encryption_key,
                realtime_outbox: runtime_plan.core_realtime_outbox(),
                read_pool: Some(infra.database_pools.read_pool()),
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
            shutdown.register_hook(SimpleShutdownHook::new(
                "permission_service",
                Duration::from_secs(10),
                synctv_services.room_service.permission_service().clone(),
            ));
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
            shutdown.register_hook(SimpleShutdownHook::new(
                "playback_service",
                Duration::from_secs(10),
                synctv_services.room_service.playback_service().clone(),
            ));
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
            shutdown.register_hook(RoomSettingsServiceShutdownHook::new(
                "room_service_settings",
                synctv_services.room_service.room_settings_service().clone(),
            ));
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
            shutdown.register_hook(RoomSettingsServiceShutdownHook::new(
                "chat_service_settings",
                synctv_services.chat_service.room_settings_service().clone(),
            ));
        }

        // Track settings cancellation token and listen task in shutdown coordinator
        shutdown.track_token("settings", synctv_services.settings_cancel.clone());
        shutdown.register_hook(AuditFlushHook {
            handle: synctv_services.audit_flush_handle.clone(),
        });
        shutdown.register_hook(SettingsListenHook {
            task: synctv_services.settings_listen_task.clone(),
        });
        shutdown.register_hook(CacheFenceRepairHook {
            cancel: synctv_services.cache_fence_repair_cancel.clone(),
            task: synctv_services.cache_fence_repair_task.clone(),
        });
        shutdown.register_hook(ProviderInvalidationHook {
            cancel: synctv_services.provider_invalidation_cancel.clone(),
            task: synctv_services.provider_invalidation_task.clone(),
        });

        if should_run_startup_partition_initialization(&infra.config) {
            // Initialize daily time partitions used by chat and playback history.
            info!("Initializing time partitions during startup...");
            synctv_core::service::ensure_time_partitions_on_startup(&infra.pool)
                .await
                .map_err(|e| partition_startup_error("time partitions", e))?;

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

    async fn init_leader_election(
        infra: &Infrastructure,
        runtime_plan: &RuntimeModePlan,
        _core: &CoreState,
        shutdown: &mut ShutdownCoordinator,
    ) -> Result<LeaderState> {
        if !runtime_plan.cluster_runtime() {
            info!("Cluster mode disabled — using unified standalone leader runtime");
            synctv_core::metrics::cluster::LEADER_ELECTION_MODE.set(0);
            synctv_core::metrics::cluster::LEADER_ELECTION_STATE.set(1);
            synctv_core::metrics::cluster::LEADER_ELECTION_EPOCH.set(0);
            synctv_core::metrics::cluster::LEADER_ELECTION_CONSECUTIVE_FAILURES.set(0);
            return Ok(LeaderState {
                leader_runtime: Arc::new(synctv_core::service::AlwaysLeader),
            });
        }

        // With Redis configured, distributed mode may be active.
        // Leader election failure in this scenario would be catastrophic:
        // multiple nodes could all believe they are the leader and run
        // singleton tasks (partition management, cleanup) simultaneously,
        // causing database corruption or inconsistent state.
        // Therefore, we MUST NOT silently fall back to AlwaysLeader here.
        // Instead, we require a working leader elector and fail fast if
        // initialization fails.

        let leader_cancel = shutdown.register_token("leader_election");

        let leader_runtime = build_managed_leader_runtime(
            leader_runtime_options(&infra.config)?,
            &infra.node_id,
            &runtime_plan.cache_shared_state_profile,
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
            other => {
                anyhow::bail!("unsupported leader runtime mode after initialization: {other}");
            }
        }

        let leader_election_handle = Some(leader_runtime.start(leader_cancel.clone()));

        // Track leader election background task
        if let Some(handle) = leader_election_handle {
            shutdown.register_task("leader_election", handle);
        }

        let leader_runtime: Arc<dyn LeaderRuntime> = leader_runtime;

        Ok(LeaderState { leader_runtime })
    }

    fn start_singleton_tasks(
        infra: &Infrastructure,
        runtime_plan: &RuntimeModePlan,
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

        let time_partition_manager = synctv_core::service::TimePartitionManager::new(
            infra.pool.clone(),
            leader.leader_runtime.clone(),
        );
        shutdown.register_task(
            "chat_partition",
            time_partition_manager.start_auto_management(24, singleton_cancel.clone()),
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

        let cleanup_config = synctv_core::service::CleanupConfig {
            unreferenced_file_retention_seconds: infra
                .config
                .file_storage
                .unreferenced_object_retention_seconds,
            ..synctv_core::service::CleanupConfig::default()
        };
        let file_storage_service = core.services.chat_service.file_storage_service();
        let cleanup_service = synctv_core::service::CleanupService::new_with_options(
            infra.pool.clone(),
            cleanup_config.clone(),
            leader.leader_runtime.clone(),
            synctv_core::service::CleanupServiceOptions {
                runtime_settings_store: Some(core.services.runtime_settings_store.clone()),
                file_storage_service: Some(file_storage_service.clone()),
            },
        );
        shutdown.register_task(
            "data_cleanup",
            cleanup_service.start_periodic(24, singleton_cancel.clone()),
        );
        info!("Periodic data cleanup started (leader-gated with fencing, interval: 24 hours, dynamic settings from registry)");

        let db_maintenance = synctv_core::service::DatabaseMaintenanceService::new_with_options(
            infra.pool.clone(),
            leader.leader_runtime.clone(),
            synctv_core::service::DatabaseMaintenanceOptions {
                config: cleanup_config.clone(),
                runtime_settings_store: Some(core.services.runtime_settings_store.clone()),
                file_storage_service: Some(file_storage_service.clone()),
            },
        );
        shutdown.register_task(
            "db_maintenance",
            db_maintenance.spawn_maintenance_loop(singleton_cancel.clone()),
        );
        info!("Database maintenance service started (leader-gated cleanup tasks every 1h)");

        if runtime_plan.cluster_runtime() {
            let pool = infra.pool.clone();
            let runtime_settings_store = core.services.runtime_settings_store.clone();
            let leader_runtime = leader.leader_runtime.clone();
            let deferred_leader_runtime = leader_runtime.clone();
            let deferred_cleanup_config = cleanup_config;
            let cancel = shutdown.register_token("cluster_leader_startup_work");
            let task_factory: AsyncOnceTaskFactory = Arc::new(move || {
                let pool = pool.clone();
                let runtime_settings_store = runtime_settings_store.clone();
                let cleanup_config = deferred_cleanup_config.clone();
                let leader_runtime = deferred_leader_runtime.clone();
                let file_storage_service = file_storage_service.clone();
                Box::pin(async move {
                    info!(
                        "Leadership gained after startup; running deferred singleton maintenance"
                    );

                    let cleanup_service = synctv_core::service::CleanupService::new_with_options(
                        pool.clone(),
                        cleanup_config.clone(),
                        leader_runtime.clone(),
                        synctv_core::service::CleanupServiceOptions {
                            runtime_settings_store: Some(runtime_settings_store.clone()),
                            file_storage_service: Some(file_storage_service.clone()),
                        },
                    );
                    let cleanup_result = cleanup_service.run_all().await;
                    info!(
                        users_purged = cleanup_result.users_purged,
                        rooms_purged = cleanup_result.rooms_purged,
                        tokens_deleted = cleanup_result.tokens_deleted,
                        credentials_deleted = cleanup_result.credentials_deleted,
                        notifications_deleted = cleanup_result.notifications_deleted,
                        chat_messages_deleted = cleanup_result.chat_messages_deleted,
                        chat_message_events_deleted = cleanup_result.chat_message_events_deleted,
                        room_resource_events_deleted = cleanup_result.room_resource_events_deleted,
                        playback_history_deleted = cleanup_result.playback_history_deleted,
                        realtime_outbox_deleted = cleanup_result.realtime_outbox_deleted,
                        token_blacklist_deleted = cleanup_result.token_blacklist_deleted,
                        unreferenced_files_deleted = cleanup_result.unreferenced_files_deleted,
                        "Deferred cleanup completed after leadership gain"
                    );

                    let db_maintenance =
                        synctv_core::service::DatabaseMaintenanceService::new_with_options(
                            pool,
                            leader_runtime,
                            synctv_core::service::DatabaseMaintenanceOptions {
                                config: cleanup_config.clone(),
                                runtime_settings_store: Some(runtime_settings_store),
                                file_storage_service: Some(file_storage_service),
                            },
                        );
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

    fn start_email_outbox_dispatcher(
        infra: &Infrastructure,
        core: &CoreState,
        leader: &LeaderState,
        shutdown: &mut ShutdownCoordinator,
    ) {
        let (Some(email_service), Some(email_token_service)) = (
            core.services.email_service.clone(),
            core.services.email_token_service.clone(),
        ) else {
            return;
        };
        let cancel = shutdown.register_token("email_outbox_dispatcher");
        shutdown.register_task(
            "email_outbox_dispatcher",
            start_email_outbox_dispatcher(
                core.services.email_outbox_service.clone(),
                email_service,
                email_token_service,
                core.services.user_service.clone(),
                leader.leader_runtime.clone(),
                infra.node_id.clone(),
                cancel,
            ),
        );
        info!("Email outbox dispatcher started with active-active delivery");
    }

    fn start_playback_background_tasks(
        infra: &Infrastructure,
        core: &CoreState,
        cluster: &ClusterState,
        shutdown: &mut ShutdownCoordinator,
    ) {
        let cancel = shutdown.register_token("playback_background_tasks");
        // Playback background work starts after cluster realtime is initialized
        // because active rooms come from local room_creation connections. These workers
        // run on every node: any replica can be the process currently serving a
        // room_creation. Database claims, SKIP LOCKED, and playback-state version writes
        // serialize the actual work when several nodes host the same room_creation.
        // Leader election remains for global singleton jobs, while presence and
        // hot-room_creation scans remain read models for lists, admin views, and metrics.
        let active_room_source = Arc::new(LocalActivePlaybackRoomSource {
            connection_runtime: cluster.realtime_connection_service.clone(),
        });

        let playback_fanout = synctv_api::playback_fanout::default_playback_fanout_service(
            cluster.realtime_fanout_service.clone(),
        );
        let prepared_fanout = playback_fanout.prepare_system_state_changed_batch_outbox_fanout();
        let publish_fanout = {
            let prepared_fanout = prepared_fanout.clone();
            Arc::new(move || prepared_fanout.publish_after_outbox_commit())
        };
        let playback_auto_advance = synctv_core::service::PlaybackAutoAdvanceService::new(
            core.services.room_service.playback_service().clone(),
            synctv_core::repository::RoomSettingsRepository::new(infra.pool.clone()),
        )
        .with_active_room_source(active_room_source.clone())
        .with_realtime_fanout(
            prepared_fanout.outbox_factory_with_source_changed(true),
            publish_fanout,
        );
        shutdown.register_task(
            "playback_auto_advance",
            playback_auto_advance.spawn(Duration::from_secs(1), cancel.clone()),
        );
        info!(
            "Playback auto-advance background scanner started (active-room scoped, interval: 1s)"
        );

        let playback_duration_probe = synctv_core::service::PlaybackDurationProbeService::new(
            core.services.room_service.playback_service().clone(),
            infra.config.security.ssrf_guard(),
        )
        .with_active_room_source(active_room_source);
        shutdown.register_task(
            "playback_duration_probe",
            playback_duration_probe.spawn(Duration::from_secs(30), cancel),
        );
        info!("Playback duration probe background scanner started (active-room scoped, interval: 30s)");
    }

    async fn init_cluster(
        infra: &Infrastructure,
        runtime_plan: &RuntimeModePlan,
        core: &CoreState,
        leader: &LeaderState,
        shutdown: &mut ShutdownCoordinator,
    ) -> Result<ClusterState> {
        // Connection manager
        let presence_service = Arc::new(
            synctv_core::service::OnlinePresenceService::from_shared_state_profile(
                &runtime_plan.realtime_shared_state_profile,
            )
            .map_err(|error| anyhow::anyhow!("Failed to initialize presence service: {error}"))?,
        );
        let connection_limit_options = connection_limits_options(&infra.config);
        let connection_limits = ConnectionLimits::from(&connection_limit_options);
        let realtime_connection_service = build_connection_manager(
            connection_limits,
            &runtime_plan.realtime_shared_state_profile,
            presence_service.clone(),
            &infra.node_id,
        )?;
        info!(
            max_per_user = infra.config.connection_limits.max_per_user,
            max_per_room = infra.config.connection_limits.max_per_room,
            max_total = infra.config.connection_limits.max_total,
            "Connection manager initialized with configurable limits"
        );

        if !runtime_plan.cluster_runtime() {
            let realtime_manager = build_local_realtime_manager(
                &infra.config,
                &infra.node_id,
                realtime_connection_service.clone(),
                build_room_message_runtime(&runtime_plan.local_realtime_profile)?,
                core.cache_invalidation.clone(),
                Some(core.services.room_service.permission_service().clone()),
            )
            .await?;
            start_room_notification_bridge(
                core.services.room_notification_service.clone(),
                realtime_manager.clone(),
                shutdown,
            );
            info!("Cluster mode disabled — initialized local-only RealtimeManager");
            let realtime_event_service: Arc<dyn RealtimeEventService> = realtime_manager.clone();
            return Ok(ClusterState {
                realtime_fanout_service: local_realtime_fanout_service(
                    realtime_event_service.clone(),
                ),
                realtime_connection_service: realtime_connection_service.clone(),
                presence_service,
                realtime_event_service,
                node_registry: None,
                health_monitor: None,
                cluster_activation: None,
            });
        }

        // RealtimeManager (requires Redis)
        let permission_service = Some(core.services.room_service.permission_service().clone());
        let cluster_backend =
            require_cluster_coordination_provider(infra.cluster_coordination_provider.as_ref())?;

        // Create a cancellation token for the realtime manager that is a child
        // of the ShutdownCoordinator's token, so coordinator shutdown also
        // cancels all cluster background tasks.
        let cluster_cancel = shutdown.register_token("realtime_manager");

        let realtime_config = RealtimeConfig {
            distributed_transport_factory: Some(cluster_backend.distributed_transport_factory()),
            message_runtime: build_room_message_runtime(
                &runtime_plan.realtime_shared_state_profile,
            )?,
            distributed_enabled: infra.config.cluster.enabled,
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
            event_handler: Some(Arc::new(CoreRealtimeEventHandler::new(
                core.cache_invalidation.clone(),
                permission_service,
            ))),
            parent_cancel_token: Some(cluster_cancel.clone()),
        };
        let realtime_manager = match RealtimeManager::new_with_runtime(
            realtime_config,
            RealtimeManagerRuntime {
                connection_runtime: Some(realtime_connection_service.clone()),
                leader_runtime: Some(leader.leader_runtime.clone()),
            },
        )
        .await
        {
            Ok(manager) => {
                info!("RealtimeManager initialized with cross-replica cache invalidation");
                manager
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to create RealtimeManager (distributed mode): {e}. \
                     RealtimeManager is required when cluster.enabled=true."
                ));
            }
        };
        let realtime_manager = Arc::new(realtime_manager);
        shutdown.register_hook(SimpleShutdownHook::new(
            "realtime_manager",
            Duration::from_secs(15),
            realtime_manager.clone(),
        ));
        start_room_notification_bridge(
            core.services.room_notification_service.clone(),
            realtime_manager.clone(),
            shutdown,
        );

        // Cluster discovery (NodeRegistry, HealthMonitor) requires Redis.
        // When cluster is explicitly enabled, discovery failures are fatal.
        let discovery = init_cluster_discovery(
            &infra.config,
            &cluster_backend.node_directory_factory(),
            &realtime_manager,
            realtime_connection_service.clone(),
            cluster_cancel.clone(),
        )
        .await?;

        for task in discovery.background_tasks {
            shutdown.register_task(task.name, task.handle);
        }
        shutdown.register_hook(SimpleShutdownHook::new(
            "health_monitor",
            Duration::from_secs(5),
            discovery.health_monitor.clone(),
        ));

        let realtime_event_service: Arc<dyn RealtimeEventService> = realtime_manager.clone();
        let outbox = runtime_plan.realtime_outbox();
        let outbox_service = Arc::new(RealtimeOutboxService::new(outbox.clone()));
        let outbox_cancel = shutdown.register_token("realtime_outbox_dispatcher");
        shutdown.register_task(
            "realtime_outbox_dispatcher",
            start_realtime_outbox_dispatcher(
                outbox.clone(),
                realtime_manager.clone(),
                infra.node_id.clone(),
                outbox_cancel,
            ),
        );

        Ok(ClusterState {
            realtime_fanout_service: distributed_realtime_fanout_service(
                outbox_service,
                realtime_event_service.clone(),
            ),
            realtime_connection_service: realtime_connection_service.clone(),
            presence_service,
            realtime_event_service,
            node_registry: Some(discovery.registry.clone()),
            health_monitor: Some(discovery.health_monitor.clone()),
            cluster_activation: Some(Arc::new(DefaultClusterNodeActivator::new(
                infra.config.clone(),
                realtime_manager.clone(),
                realtime_connection_service.clone(),
                discovery.registry,
                discovery.health_monitor,
            )) as Arc<dyn ClusterNodeActivator>),
        })
    }

    async fn init_servers(
        infra: &Infrastructure,
        core: &CoreState,
        leader: &LeaderState,
        shutdown: &mut ShutdownCoordinator,
    ) -> Result<ServerComponents> {
        // Livestream
        let (livestream_state, live_infra, background_handles) = init_livestream(
            &infra.config,
            infra.public_id_codec.clone(),
            &core.services,
            infra.shared_runtime.clone(),
            Arc::new(LeaderRuntimeCheck {
                leader_runtime: leader.leader_runtime.clone(),
            }),
            &infra.node_id,
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

        // WebRTC (STUN servers)
        let webrtc_components = init_webrtc(&infra.config).await;

        // Media providers
        let providers = synctv_core::provider::ProviderSet::new_with_ssrf_guard(
            core.services.provider_instance_manager.clone(),
            infra.config.security.ssrf_guard(),
        )?;
        let playback_duration_probe =
            Arc::new(synctv_core::service::PlaybackDurationProbeService::new(
                core.services.room_service.playback_service().clone(),
                infra.config.security.ssrf_guard(),
            ));

        Ok(ServerComponents {
            livestream_state,
            live_infra,
            stun_server: webrtc_components.stun_server,
            webrtc_status: webrtc_components.status,
            providers,
            playback_duration_probe,
        })
    }

    fn assemble(
        infra: Infrastructure,
        core: CoreState,
        cluster: ClusterState,
        servers: ServerComponents,
        clock: Arc<synctv_core::SyncedClock>,
        shutdown: ShutdownCoordinator,
    ) -> Self {
        let services = Services {
            user_service: core.services.user_service.clone(),
            read_pool: infra.database_pools.read_pool(),
            room_service: core.services.room_service.clone(),
            jwt_service: core.services.jwt_service.clone(),
            realtime_fanout_service: cluster.realtime_fanout_service,
            rate_limiter: core.services.rate_limiter.clone(),
            rate_limit_config: core.services.rate_limit_config.clone(),
            content_filter: core.services.content_filter.clone(),
            realtime_connection_service: cluster.realtime_connection_service,
            presence_service: cluster.presence_service,
            realtime_event_service: cluster.realtime_event_service,
            providers_manager: core.services.providers_manager.clone(),
            provider_instance_manager: core.services.provider_instance_manager.clone(),
            user_provider_credential_repository: core
                .services
                .user_provider_credential_repo
                .clone(),
            providers: servers.providers,
            oauth2_service: core.services.oauth2_service.clone(),
            passkey_service: core.services.passkey_service.clone(),
            settings_service: core.services.settings_service.clone(),
            runtime_settings_store: core.services.runtime_settings_store.clone(),
            email_service: core.services.email_service.clone(),
            email_token_service: core.services.email_token_service.clone(),
            email_outbox_service: core.services.email_outbox_service.clone(),
            ws_ticket_service: core.services.ws_ticket_service.clone(),
            publish_key_service: core.services.publish_key_service.clone(),
            notification_service: Some(core.services.notification_service.clone()),
            chat_service: core.services.chat_service.clone(),
            audit_service: core.services.audit_service.clone(),
            user_cache: core.services.user_cache.clone(),
            provider_stores: core.services.provider_stores.clone(),
            playback_duration_probe: servers.playback_duration_probe,
            live_streaming_infrastructure: servers.live_infra,
            stun_server: servers.stun_server,
            webrtc_status: servers.webrtc_status,
            node_registry: cluster.node_registry,
            health_monitor: cluster.health_monitor,
            cluster_activation: cluster.cluster_activation,
            redis_runtime: core.services.redis_runtime(),
            credential_encryption: core.services.credential_encryption,
        };

        Self {
            config: infra.config,
            public_id_codec: infra.public_id_codec,
            clock,
            database_pools: infra.database_pools,
            services,
            livestream_state: servers.livestream_state,
            shutdown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_config::{
        BootstrapConfig, BufferSizesConfig, CacheConfig, ClusterChannelConfig,
        ConnectionLimitsConfig, DatabaseConfig, JwtConfig, LivestreamConfig, LoggingConfig,
        MediaProvidersConfig, PasswordComplexityConfig, ProxySliceCacheConfig, RedisConfig,
        RequestRateLimitConfig, ServerConfig, WebAuthnConfig, WebRTCConfig,
    };
    use crate::bootstrap::cluster::{ClusterNodeActivator, DefaultClusterNodeActivator};
    use synctv_core::{
        models::{SignupMethod, User, UserRole, UserStatus},
        repository::{PasswordCredentialMaterial, UserPasswordRepository, UserRepository},
        service::OpaquePasswordService,
        RedisConnectionRuntime, SharedRedisConnectionRuntime,
    };
    use synctv_core_testing::test_redis_key_prefix;
    use tokio::sync::{broadcast, RwLock};

    #[tokio::test]
    async fn test_activate_cluster_node_registers_only_when_called() {
        let realtime_manager = Arc::new(
            RealtimeManager::new(RealtimeConfig {
                distributed_transport_factory: None,
                message_runtime: build_room_message_runtime(
                    &synctv_core::SharedStateProfile::for_cluster_runtime(
                        None,
                        "activation-test:",
                        false,
                    ),
                )
                .expect("local message runtime should initialize"),
                distributed_enabled: false,
                node_id: "activation-test-node".to_string(),
                dedup_window: Duration::from_secs(1),
                critical_channel_capacity: 16,
                publish_channel_capacity: 16,
                key_prefix: "activation-test:".to_string(),
                catchup_window_secs: 60,
                stream_max_length: 100,
                event_handler: None,
                parent_cancel_token: None,
            })
            .await
            .expect("realtime manager should initialize"),
        );
        let connection_manager: Arc<dyn ConnectionRuntime> = Arc::new(
            synctv_realtime::sync::ConnectionManager::new(ConnectionLimits::default()),
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
                &realtime_manager.cancel_token(),
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
        let health_runtime: Arc<dyn synctv_cluster::ClusterHealthRuntime> = health_monitor.clone();

        DefaultClusterNodeActivator::new(
            config,
            realtime_manager.clone(),
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
        realtime_manager.shutdown().await;
    }

    fn test_presence_service(
        profile: &synctv_core::SharedStateProfile,
    ) -> Arc<synctv_core::service::OnlinePresenceService> {
        Arc::new(
            synctv_core::service::OnlinePresenceService::from_shared_state_profile(profile)
                .expect("presence service should initialize"),
        )
    }

    fn test_connection_manager(
        profile: &synctv_core::SharedStateProfile,
    ) -> Arc<dyn ConnectionRuntime> {
        build_connection_manager(
            ConnectionLimits::default(),
            profile,
            test_presence_service(profile),
            "test-node",
        )
        .expect("connection manager should initialize")
    }

    fn minimal_valid_startup_config() -> Config {
        Config {
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                port: 8080,
                project_url: synctv_api::DEFAULT_PROJECT_URL.to_string(),
                enable_reflection: false,
                grpc_max_message_size_bytes: 16 * 1024 * 1024,
                grpc_compression_enabled: true,
                trusted_proxies: Vec::new(),
                cors_allowed_origins: Vec::new(),
                advertise_host: String::new(),
                shutdown_drain_timeout_seconds: 30,
            },
            time: crate::app_config::TimeConfig::default(),
            data_dir: crate::app_config::default_data_dir().display().to_string(),
            metrics: crate::app_config::MetricsConfig::default(),
            management: crate::app_config::ManagementConfig {
                enabled: false,
                ..crate::app_config::ManagementConfig::default()
            },
            database: DatabaseConfig::default(),
            redis: RedisConfig {
                url: "redis://redis.invalid".to_string(),
                ..RedisConfig::default()
            },
            jwt: JwtConfig {
                secret: "test-jwt-secret-key-for-testing-minimum-length".to_string(),
                ..JwtConfig::default()
            },
            logging: LoggingConfig::default(),
            livestream: LivestreamConfig {
                hls_storage: crate::app_config::HlsStorageConfig::SharedFile(
                    crate::app_config::HlsFileStorageConfig {
                        path: "/var/lib/synctv/hls".to_string(),
                    },
                ),
                ..LivestreamConfig::default()
            },
            file_storage: crate::app_config::FileStorageConfig::default(),
            chat: crate::app_config::ChatConfig::default(),
            webauthn: WebAuthnConfig::default(),
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
            proxy_slice_cache: ProxySliceCacheConfig::default(),
            messaging_rate_limits: crate::app_config::MessagingRateLimitConfig::default(),
            request_rate_limits: RequestRateLimitConfig::default(),
            security: crate::app_config::SecurityConfig {
                opaque_server_setup_secret: "test-opaque-server-setup-secret-for-app-startup-tests"
                    .to_string(),
                ..crate::app_config::SecurityConfig::default()
            },
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

        fn publish_event(&self, event: LeadershipEvent) {
            if let Err(error) = self.tx.send(event) {
                tracing::warn!(
                    ?event,
                    error = %error,
                    "Test leader runtime failed to publish leadership event"
                );
            }
        }

        fn gain_leadership(&self) {
            let epoch = self.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            self.is_leader
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.publish_event(LeadershipEvent::Gained { epoch });
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
            self.epoch.load(std::sync::atomic::Ordering::SeqCst)
        }

        async fn resign(&self) {
            self.is_leader
                .store(false, std::sync::atomic::Ordering::SeqCst);
            self.publish_event(LeadershipEvent::Lost);
        }
    }

    #[test]
    fn test_cluster_runtime_enabled_depends_only_on_cluster_flag() {
        let mut config = Config::default();
        config.cluster.secret = "shared-secret".to_string();

        assert!(
            !cluster_runtime_enabled(&config),
            "cluster.secret alone must not activate realtime runtime"
        );

        config.cluster.enabled = true;
        assert!(
            cluster_runtime_enabled(&config),
            "cluster.enabled=true must activate realtime runtime"
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
    fn test_runtime_mode_plan_keeps_standalone_local_state() {
        let mut config = minimal_valid_startup_config();
        config.cluster.enabled = false;

        let (
            cluster_runtime,
            cache_shared_state_profile,
            realtime_shared_state_profile,
            local_realtime_profile,
        ) = runtime_profiles_from_config(&config, None);

        assert!(!cluster_runtime);
        assert_eq!(
            cache_shared_state_profile.state_mode(),
            synctv_core::SharedStateMode::LocalOnly
        );
        assert_eq!(
            realtime_shared_state_profile.state_mode(),
            synctv_core::SharedStateMode::LocalOnly
        );
        assert_eq!(
            local_realtime_profile.state_mode(),
            synctv_core::SharedStateMode::LocalOnly
        );
    }

    #[test]
    fn test_runtime_mode_plan_uses_cluster_shared_state() {
        let mut config = minimal_valid_startup_config();
        config.cluster.enabled = true;

        let (
            cluster_runtime,
            cache_shared_state_profile,
            realtime_shared_state_profile,
            local_realtime_profile,
        ) = runtime_profiles_from_config(&config, None);

        assert!(cluster_runtime);
        assert_eq!(
            cache_shared_state_profile.state_mode(),
            synctv_core::SharedStateMode::SharedRequired
        );
        assert_eq!(
            realtime_shared_state_profile.state_mode(),
            synctv_core::SharedStateMode::SharedRequired
        );
        assert_eq!(
            local_realtime_profile.state_mode(),
            synctv_core::SharedStateMode::LocalOnly
        );
    }

    #[test]
    fn test_startup_partition_initialization_runs_in_all_modes() {
        let mut config = Config::default();
        config.cluster.secret = "shared-secret".to_string();

        assert!(
            should_run_startup_partition_initialization(&config),
            "standalone mode must initialize required partitions during startup"
        );

        config.cluster.enabled = true;
        assert!(
            should_run_startup_partition_initialization(&config),
            "distributed mode must also initialize required partitions before serving traffic"
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
            &crate::bootstrap::RootUserBootstrapOptions {
                create_root_user: false,
                root_username: "root".to_string(),
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

        let opaque_record =
            OpaquePasswordService::derive_from_secret(b"startup-admin-precondition-existing")
                .register_password(b"synctv:test:existing-admin", "StrongPwd12345!")
                .expect("password credential should be generated");
        let mut admin = User::new("existing-admin".to_string(), SignupMethod::AdminCreated);
        admin.role = UserRole::Admin;
        admin.status = UserStatus::Active;
        let user_repo = UserRepository::new(pool.clone());
        let user_password_repo = UserPasswordRepository::new(pool.clone());
        let mut tx = pool.begin().await.expect("test tx should start");
        let admin = user_repo
            .create_with_executor(&admin, &mut *tx)
            .await
            .expect("existing admin should be inserted");
        user_password_repo
            .create_for_user_with_executor(
                &admin,
                PasswordCredentialMaterial::opaque_only(&opaque_record),
                &mut *tx,
            )
            .await
            .expect("existing admin password should be inserted");
        tx.commit().await.expect("test tx should commit");

        ensure_administrator_bootstrap_precondition(
            &pool,
            &crate::bootstrap::RootUserBootstrapOptions {
                create_root_user: false,
                root_username: "root".to_string(),
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
        config.cluster.secret = "test-cluster-secret-key-1234567890".to_string();
        config.redis.url.clear();

        let error = validate_startup_config(&config)
            .expect_err("startup preflight must reject distributed mode without Redis");

        assert!(
            error
                .to_string()
                .contains("distributed mode requires Redis to be configured"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn test_build_local_realtime_manager_supports_single_node_realtime_paths() {
        let config = minimal_valid_startup_config();
        let realtime_profile =
            synctv_core::SharedStateProfile::for_cluster_runtime(None, "test-local:", false);
        let connection_manager = test_connection_manager(&realtime_profile);
        let cache_invalidation = Arc::new(synctv_core::cache::CacheInvalidationService::new(
            "test-node".to_string(),
            "test-local:cache:invalidate".to_string(),
        ));

        let realtime_manager = build_local_realtime_manager(
            &config,
            "test-node",
            connection_manager,
            build_room_message_runtime(&realtime_profile)
                .expect("local message runtime should initialize"),
            cache_invalidation,
            None,
        )
        .await
        .expect("standalone mode should still wire a local RealtimeManager");

        let metrics = realtime_manager.metrics();
        assert!(
            metrics.has_connection_manager,
            "single-node realtime paths need a wired connection manager"
        );
        assert!(
            !metrics.distributed_enabled,
            "local-only realtime manager must not require Redis"
        );

        realtime_manager.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_init_cluster_injects_runtime_dependencies_into_realtime_manager() {
        let (_redis, client) = synctv_core_testing::start_redis_with_client().await;
        let conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .expect("Redis connection manager should be created");
        let shared_conn = Arc::new(RwLock::new(conn.clone()));
        let shared_runtime: Arc<dyn RedisConnectionRuntime> =
            Arc::new(SharedRedisConnectionRuntime::new(shared_conn.clone()));
        let realtime_profile = synctv_core::SharedStateProfile::for_cluster_runtime(
            Some(shared_runtime),
            "test-cluster:",
            true,
        );

        let connection_manager = test_connection_manager(&realtime_profile);

        let realtime_config = RealtimeConfig {
            distributed_transport_factory: Some(
                synctv_realtime::sync::build_realtime_message_transport_factory(
                    synctv_core::coordination_runtime_from_client(client),
                ),
            ),
            message_runtime: build_room_message_runtime(&realtime_profile)
                .expect("shared message runtime should initialize"),
            distributed_enabled: true,
            node_id: "test-node".to_string(),
            dedup_window: Duration::from_secs(30),
            critical_channel_capacity: 100,
            publish_channel_capacity: 100,
            key_prefix: "test-cluster:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 1000,
            event_handler: None,
            parent_cancel_token: None,
        };

        let realtime_manager = RealtimeManager::new_with_runtime(
            realtime_config,
            RealtimeManagerRuntime {
                connection_runtime: Some(connection_manager),
                leader_runtime: Some(Arc::new(synctv_core::service::AlwaysLeader)),
            },
        )
        .await
        .expect("RealtimeManager should initialize");
        let metrics = realtime_manager.metrics();
        assert!(metrics.has_connection_manager);
        assert!(metrics.has_leader_elector);

        realtime_manager.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_build_connection_manager_wires_redis_in_cluster_mode() {
        use synctv_core::models::{RoomId, UserId};
        let (_redis, client) = synctv_core_testing::start_redis_with_client().await;
        let prefix = test_redis_key_prefix("conn-mgr-wires");
        let app_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .expect("App connection manager should be created");
        let shared_conn = Arc::new(RwLock::new(app_conn));
        let shared_runtime: Arc<dyn RedisConnectionRuntime> =
            Arc::new(SharedRedisConnectionRuntime::new(shared_conn));
        let realtime_profile = synctv_core::SharedStateProfile::for_cluster_runtime(
            Some(shared_runtime),
            &prefix,
            true,
        );

        let presence_service = test_presence_service(&realtime_profile);
        let manager = build_connection_manager(
            ConnectionLimits::default(),
            &realtime_profile,
            presence_service.clone(),
            "test-node",
        )
        .expect("distributed mode should build shared realtime connection manager");

        manager
            .register("conn-1".to_string(), UserId::expect_positive(111_001))
            .await
            .expect("Connection registration should succeed");
        let room_id = RoomId::expect_positive(111_002);
        manager
            .join_room("conn-1", RoomId::expect_positive(111_002))
            .await
            .expect("Room join should succeed");

        let count = presence_service
            .room_stats(room_id)
            .await
            .expect("presence room stats should load")
            .connection_count;

        assert_eq!(count, 1, "Distributed room presence should be tracked");

        manager.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_build_connection_manager_uses_shared_redis_handle_in_cluster_mode() {
        use synctv_core::models::{RoomId, UserId};

        let (_redis, client) = synctv_core_testing::start_redis_with_client().await;
        let prefix = test_redis_key_prefix("conn-mgr-shared");
        let first_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .expect("initial app connection manager should be created");
        let shared_conn = Arc::new(RwLock::new(first_conn));
        let shared_runtime: Arc<dyn RedisConnectionRuntime> =
            Arc::new(SharedRedisConnectionRuntime::new(shared_conn.clone()));
        let realtime_profile = synctv_core::SharedStateProfile::for_cluster_runtime(
            Some(shared_runtime),
            &prefix,
            true,
        );

        let presence_service = test_presence_service(&realtime_profile);
        let manager = build_connection_manager(
            ConnectionLimits::default(),
            &realtime_profile,
            presence_service.clone(),
            "test-node",
        )
        .expect("distributed mode should preserve shared runtime wiring");

        manager
            .register("conn-1".to_string(), UserId::expect_positive(111_001))
            .await
            .expect("connection registration should succeed");

        let replacement_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .expect("replacement app connection manager should be created");
        *shared_conn.write().await = replacement_conn;

        let room_id = RoomId::expect_positive(111_003);
        manager
            .join_room("conn-1", RoomId::expect_positive(111_003))
            .await
            .expect("room join after shared connection swap should succeed");

        let count = presence_service
            .room_stats(room_id)
            .await
            .expect("presence room stats should load after shared runtime swap")
            .connection_count;

        assert_eq!(
            count, 1,
            "cluster ConnectionManager must continue using the shared runtime after a hot swap"
        );

        manager.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_build_connection_manager_keeps_standalone_mode_local_even_with_redis() {
        use synctv_core::models::{RoomId, UserId};
        let (_redis, client) = synctv_core_testing::start_redis_with_client().await;
        let app_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .expect("App connection manager should be created");
        let realtime_profile = synctv_core::SharedStateProfile::for_cluster_runtime(
            Some(Arc::new(SharedRedisConnectionRuntime::new(Arc::new(
                RwLock::new(app_conn),
            )))),
            "test-standalone:",
            false,
        );

        let presence_service = test_presence_service(&realtime_profile);
        let manager = build_connection_manager(
            ConnectionLimits::default(),
            &realtime_profile,
            presence_service.clone(),
            "test-node",
        )
        .expect("standalone mode should build local connection manager");

        manager
            .register("conn-1".to_string(), UserId::expect_positive(111_001))
            .await
            .expect("Standalone registration should succeed");
        manager
            .join_room("conn-1", RoomId::expect_positive(111_002))
            .await
            .expect("Standalone room join should succeed");

        let count = presence_service
            .room_stats(RoomId::expect_positive(111_002))
            .await
            .expect("local presence room stats should load")
            .connection_count;

        assert!(count == 1, "Standalone mode should track presence locally");

        manager.shutdown().await;
    }

    #[test]
    fn test_build_connection_manager_returns_error_without_redis_in_cluster_mode() {
        let realtime_profile =
            synctv_core::SharedStateProfile::for_cluster_runtime(None, "test:", true);
        let Err(error) = build_connection_manager(
            ConnectionLimits::default(),
            &realtime_profile,
            Arc::new(synctv_core::service::OnlinePresenceService::local()),
            "test-node",
        ) else {
            panic!("distributed mode without Redis wiring must return an error");
        };

        assert!(
            error
                .to_string()
                .contains("distributed runtime requires shared realtime connection state"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_require_cluster_coordination_provider_returns_error_instead_of_panicking() {
        let Err(error) = require_cluster_coordination_provider(None) else {
            panic!("missing distributed backends in realtime runtime must return an error");
        };

        assert!(
            error.to_string().contains(
                "startup invariant violated: realtime runtime reached without distributed backend wiring"
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

        let service = Arc::new(synctv_core::cache::CacheInvalidationService::new(
            "test-node".to_string(),
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
