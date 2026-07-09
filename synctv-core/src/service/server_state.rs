use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::service::{EmailService, UserService, WebRtcRuntimeStatus, WebSocketTicketService};
use crate::{RedisConnectionRuntime, RedisDeploymentMode};

const SERVER_STATE_HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(5);
const SERVER_STATE_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const MEMORY_UNHEALTHY_THRESHOLD_PERCENT: f64 = 90.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServerStateScope {
    Current,
    Node,
    All,
}

impl ServerStateScope {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Node => "node",
            Self::All => "all",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateSelection {
    pub node_id: Option<String>,
    pub all_nodes: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateResponse {
    pub scope: ServerStateScope,
    pub summary: ServerStateSummary,
    pub nodes: Vec<ServerStateNode>,
    pub failures: Vec<ServerStateFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateSummary {
    pub status: ServerStateNodeStatus,
    pub healthy_nodes: i64,
    pub degraded_nodes: i64,
    pub unhealthy_nodes: i64,
    pub failed_nodes: i64,
}

macro_rules! server_state_status {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

server_state_status!(ServerStateNodeStatus {
    Healthy => "healthy",
    Degraded => "degraded",
    Unhealthy => "unhealthy",
});

server_state_status!(ServerStateDatabaseStatus {
    Healthy => "healthy",
    Unhealthy => "unhealthy",
});

server_state_status!(ServerStateRedisStatus {
    Healthy => "healthy",
    NotConfigured => "not_configured",
    Unhealthy => "unhealthy",
});

server_state_status!(ServerStateClusterStatus {
    Healthy => "healthy",
    Unhealthy => "unhealthy",
    Disabled => "disabled",
});

server_state_status!(ServerStateWsTicketStatus {
    Healthy => "healthy",
    Unhealthy => "unhealthy",
});

server_state_status!(ServerStateEmailStatus {
    Configured => "configured",
    NotConfigured => "not_configured",
});

server_state_status!(ServerStateLivestreamStatus {
    Configured => "configured",
    NotConfigured => "not_configured",
});

server_state_status!(ServerStateMemoryStatus {
    Healthy => "healthy",
    Unhealthy => "unhealthy",
    Unknown => "unknown",
});

server_state_status!(ServerStateWebRtcStatus {
    Healthy => "healthy",
    Degraded => "degraded",
    Disabled => "disabled",
});

server_state_status!(ServerStateCpuStatus {
    Healthy => "healthy",
    Degraded => "degraded",
    Unhealthy => "unhealthy",
    Unknown => "unknown",
});

server_state_status!(ServerStateSliceCacheStatus {
    Healthy => "healthy",
    Disabled => "disabled",
});

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateNode {
    pub node_id: String,
    pub status: ServerStateNodeStatus,
    pub updated_at: i64,
    pub version: String,
    pub api_address: String,
    pub realtime: ServerStateRealtime,
    pub database: ServerStateDatabase,
    pub redis: ServerStateRedis,
    pub cluster: ServerStateCluster,
    pub ws_ticket: ServerStateWsTicket,
    pub email: ServerStateEmail,
    pub livestream: ServerStateLivestream,
    pub memory: ServerStateMemory,
    pub webrtc: ServerStateWebRtc,
    pub cpu: ServerStateCpu,
    pub slice_cache: ServerStateSliceCache,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateCpu {
    pub status: ServerStateCpuStatus,
    pub available_parallelism: u32,
    pub current_load_1m: Option<f64>,
    pub load_ratio_1m: Option<f64>,
    pub load_average_1m: Option<f64>,
    pub load_average_5m: Option<f64>,
    pub load_average_15m: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateRealtime {
    pub distributed_enabled: bool,
    pub connection_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateDatabasePool {
    pub size: u32,
    pub idle_connections: u32,
    pub active_connections: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateDatabase {
    pub status: ServerStateDatabaseStatus,
    pub host: String,
    pub port: u32,
    pub database: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub connect_timeout_seconds: u64,
    pub idle_timeout_seconds: u64,
    pub max_lifetime_seconds: u64,
    pub primary_pool: ServerStateDatabasePool,
    pub read_pool_enabled: bool,
    pub read_host: String,
    pub read_port: u32,
    pub read_pool: ServerStateDatabasePool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateRedis {
    pub status: ServerStateRedisStatus,
    pub configured: bool,
    pub deployment_mode: String,
    pub database: i64,
    pub key_prefix: String,
    pub connect_timeout_seconds: u64,
    pub response_timeout_seconds: u64,
    pub pipeline_buffer_size: u64,
    pub sentinel_master_name: String,
    pub sentinel_node_count: u32,
    pub ping_latency_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateCluster {
    pub status: ServerStateClusterStatus,
    pub enabled: bool,
    pub discovery_mode: String,
    pub distributed_realtime_enabled: bool,
    pub node_id_empty: bool,
    pub routable_node_count: u32,
    pub nodes: Vec<ServerStateClusterNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateWsTicket {
    pub status: ServerStateWsTicketStatus,
    pub cross_node_capable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateEmail {
    pub status: ServerStateEmailStatus,
    pub configured: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateLivestream {
    pub status: ServerStateLivestreamStatus,
    pub configured: bool,
    pub active_publisher_count: u64,
    pub active_room_count: u64,
    pub rtmp_port: u32,
    pub public_rtmp_host: String,
    pub gop_cache_size: u32,
    pub gop_cache_max_memory_mb: u64,
    pub stream_timeout_seconds: u64,
    pub hls_storage_backend: String,
    pub hls_storage_path: String,
    pub hls_memory_max_mb: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateMemory {
    pub status: ServerStateMemoryStatus,
    pub used_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub usage_percent: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateWebRtc {
    pub status: ServerStateWebRtcStatus,
    pub mode: String,
    pub builtin_stun_configured: bool,
    pub builtin_stun_state: String,
    pub reason: String,
    pub local_addr: String,
    pub external_addr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateClusterNode {
    pub node_id: String,
    pub api_address: String,
    pub last_heartbeat: i64,
    pub epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateSliceCache {
    pub status: ServerStateSliceCacheStatus,
    pub engine_enabled: bool,
    pub backend: String,
    pub file_cache_dir: String,
    pub slice_size: u64,
    pub max_cache_size: u64,
    pub segment_ttl_secs: u64,
    pub stale_max_age_secs: u64,
    pub stale_while_revalidate: bool,
    pub eviction_interval_secs: u64,
    pub watermark_ratio: f64,
    pub current_size_bytes: u64,
    pub entry_count: u64,
    pub metadata_entries: u64,
    pub updating_entries: u64,
    pub lock_count: u64,
    pub usage_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateFailure {
    pub node_id: String,
    pub error: String,
}

#[derive(Debug, Clone)]
pub struct ServerStateClusterTarget {
    pub node_id: String,
    pub api_address: String,
    pub last_heartbeat: i64,
    pub epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerStateRealtimeMetrics {
    pub distributed_enabled: bool,
    pub connection_count: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerStateMemoryHealth {
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub usage_percent: f64,
    pub status: ServerStateMemoryStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ServerStateLivestreamSnapshot {
    pub active_publisher_count: u64,
    pub active_room_count: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ServerStateError {
    #[error("nodeId and allNodes are mutually exclusive")]
    InvalidSelection,
    #[error("cluster client is unavailable; cannot query status for node '{0}'")]
    ClusterUnavailable(String),
    #[error("{0}")]
    Cluster(String),
    #[error("cluster secret is required for remote status operations")]
    MissingClusterSecret,
    #[error("invalid cluster secret configuration")]
    InvalidClusterSecret,
    #[error("failed to request status from node '{node_id}': {error}")]
    RemoteRequest { node_id: String, error: String },
    #[error("failed to decode status from node '{node_id}': {error}")]
    RemoteDecode { node_id: String, error: String },
}

pub type ServerStateResult<T> = Result<T, ServerStateError>;

#[async_trait]
pub trait ServerStateClusterRuntime: Send + Sync {
    async fn resolve_routable_node(
        &self,
        target_node_id: &str,
    ) -> ServerStateResult<ServerStateClusterTarget>;

    async fn remote_routable_nodes(&self) -> ServerStateResult<Vec<ServerStateClusterTarget>>;
}

#[async_trait]
pub trait ServerStateRemoteClient: Send + Sync {
    async fn remote_node_server_state(
        &self,
        node: &ServerStateClusterTarget,
    ) -> ServerStateResult<ServerStateNode>;
}

pub trait ServerStateRealtimeRuntime: Send + Sync {
    fn metrics(&self) -> ServerStateRealtimeMetrics;

    fn node_id(&self) -> &str;
}

#[async_trait]
pub trait ServerStateLivestreamRuntime: Send + Sync {
    async fn snapshot(&self) -> ServerStateLivestreamSnapshot;
}

pub trait ServerStateSliceCacheRuntime: Send + Sync {
    fn snapshot(&self) -> ServerStateSliceCache;
}

#[derive(Debug, Clone)]
pub struct ServerStateRuntimeParams {
    pub cluster_enabled: bool,
    pub advertise_api_address: String,
    pub cluster: ServerStateClusterOptions,
    pub database: ServerStateDatabaseOptions,
    pub redis: ServerStateRedisOptions,
    pub livestream: ServerStateLivestreamOptions,
}

#[derive(Debug, Clone, Default)]
pub struct ServerStateClusterOptions {
    pub discovery_mode: String,
}

#[derive(Debug, Clone, Default)]
pub struct ServerStateDatabaseOptions {
    pub host: String,
    pub port: u16,
    pub name: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub connect_timeout_seconds: u64,
    pub idle_timeout_seconds: u64,
    pub max_lifetime_seconds: u64,
    pub read_url: String,
    pub read_host: String,
    pub read_port: u16,
}

#[derive(Debug, Clone)]
pub struct ServerStateRedisOptions {
    pub deployment_mode: RedisDeploymentMode,
    pub database: i64,
    pub key_prefix: String,
    pub connect_timeout_seconds: u64,
    pub response_timeout_seconds: u64,
    pub pipeline_buffer_size: usize,
    pub sentinel_master_name: Option<String>,
    pub sentinel_addresses: Vec<String>,
}

impl Default for ServerStateRedisOptions {
    fn default() -> Self {
        Self {
            deployment_mode: RedisDeploymentMode::Standalone,
            database: 0,
            key_prefix: "synctv:".to_string(),
            connect_timeout_seconds: 5,
            response_timeout_seconds: 5,
            pipeline_buffer_size: 512,
            sentinel_master_name: None,
            sentinel_addresses: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ServerStateHlsStorageBackend {
    #[default]
    Memory,
    File,
    SharedFile,
    Oss,
}

#[derive(Debug, Clone, Default)]
pub struct ServerStateHlsStorageOptions {
    pub backend: ServerStateHlsStorageBackend,
    pub path: String,
    pub memory_max_mb: u64,
}

#[derive(Debug, Clone)]
pub struct ServerStateLivestreamOptions {
    pub rtmp_port: u16,
    pub public_rtmp_host: String,
    pub gop_cache_size: u32,
    pub stream_timeout_seconds: u64,
    pub gop_cache_max_memory_mb: u64,
    pub hls_storage: ServerStateHlsStorageOptions,
}

impl Default for ServerStateLivestreamOptions {
    fn default() -> Self {
        Self {
            rtmp_port: 1935,
            public_rtmp_host: String::new(),
            gop_cache_size: 2,
            stream_timeout_seconds: 300,
            gop_cache_max_memory_mb: 100,
            hls_storage: ServerStateHlsStorageOptions::default(),
        }
    }
}

pub struct ServerStateServiceDependencies {
    pub runtime_params: Arc<ServerStateRuntimeParams>,
    pub user_service: Arc<UserService>,
    pub realtime_runtime: Arc<dyn ServerStateRealtimeRuntime>,
    pub ws_ticket_service: Arc<dyn WebSocketTicketService>,
    pub redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
    pub email_service: Option<Arc<EmailService>>,
    pub live_streaming_configured: bool,
    pub livestream_runtime: Option<Arc<dyn ServerStateLivestreamRuntime>>,
    pub cluster_runtime: Option<Arc<dyn ServerStateClusterRuntime>>,
    pub remote_client: Option<Arc<dyn ServerStateRemoteClient>>,
    pub slice_cache_runtime: Arc<dyn ServerStateSliceCacheRuntime>,
    pub webrtc_status: WebRtcRuntimeStatus,
}

#[derive(Clone)]
pub struct ServerStateService {
    runtime_params: Arc<ServerStateRuntimeParams>,
    user_service: Arc<UserService>,
    realtime_runtime: Arc<dyn ServerStateRealtimeRuntime>,
    ws_ticket_service: Arc<dyn WebSocketTicketService>,
    redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
    email_service: Option<Arc<EmailService>>,
    live_streaming_configured: bool,
    livestream_runtime: Option<Arc<dyn ServerStateLivestreamRuntime>>,
    cluster_runtime: Option<Arc<dyn ServerStateClusterRuntime>>,
    remote_client: Option<Arc<dyn ServerStateRemoteClient>>,
    slice_cache_runtime: Arc<dyn ServerStateSliceCacheRuntime>,
    webrtc_status: WebRtcRuntimeStatus,
    latest_local_state: Arc<RwLock<Option<ServerStateNode>>>,
}

impl ServerStateService {
    #[must_use]
    pub fn new(deps: ServerStateServiceDependencies) -> Self {
        Self {
            runtime_params: deps.runtime_params,
            user_service: deps.user_service,
            realtime_runtime: deps.realtime_runtime,
            ws_ticket_service: deps.ws_ticket_service,
            redis_runtime: deps.redis_runtime,
            email_service: deps.email_service,
            live_streaming_configured: deps.live_streaming_configured,
            livestream_runtime: deps.livestream_runtime,
            cluster_runtime: deps.cluster_runtime,
            remote_client: deps.remote_client,
            slice_cache_runtime: deps.slice_cache_runtime,
            webrtc_status: deps.webrtc_status,
            latest_local_state: Arc::new(RwLock::new(None)),
        }
    }

    #[must_use]
    pub const fn refresh_interval() -> Duration {
        SERVER_STATE_REFRESH_INTERVAL
    }

    pub fn spawn_refresh_task(self: Arc<Self>, cancel: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(async move {
            self.refresh_local_server_state().await;
            let mut interval = tokio::time::interval(Self::refresh_interval());
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    _ = interval.tick() => {
                        self.refresh_local_server_state().await;
                    }
                }
            }
        })
    }

    pub async fn refresh_local_server_state(&self) -> ServerStateNode {
        let state = self.collect_local_server_state_now().await;
        *self.latest_local_state.write().await = Some(state.clone());
        state
    }

    pub async fn collect_server_state(
        &self,
        selection: ServerStateSelection,
    ) -> ServerStateResult<ServerStateResponse> {
        let target_node_id =
            validate_server_state_selection(selection.node_id.as_deref(), selection.all_nodes)?;
        if selection.all_nodes {
            return self.collect_all_server_state().await;
        }

        match target_node_id {
            Some(node_id) if node_id != self.realtime_runtime.node_id() => {
                let node = self
                    .require_cluster_runtime(&node_id)?
                    .resolve_routable_node(&node_id)
                    .await?;
                let remote = self
                    .require_remote_client(&node_id)?
                    .remote_node_server_state(&node)
                    .await?;
                Ok(response_for_server_state_nodes(
                    ServerStateScope::Node,
                    vec![remote],
                    Vec::new(),
                ))
            }
            Some(_) | None => {
                let local = self.collect_local_server_state().await;
                Ok(response_for_server_state_nodes(
                    target_node_id.map_or(ServerStateScope::Current, |_| ServerStateScope::Node),
                    vec![local],
                    Vec::new(),
                ))
            }
        }
    }

    pub async fn collect_local_server_state(&self) -> ServerStateNode {
        if let Some(state) = self.latest_local_state.read().await.clone() {
            return state;
        }
        self.refresh_local_server_state().await
    }

    async fn collect_local_server_state_now(&self) -> ServerStateNode {
        let database = self.database_state().await;
        let redis = self.redis_state().await;
        let cluster_enabled = self.runtime_params.cluster_enabled;
        let cluster = self.cluster_state(cluster_enabled).await;
        let ws_ticket = self.ws_ticket_state(cluster_enabled);
        let email = self.email_state();
        let livestream = self.livestream_state().await;
        let memory = memory_state();
        let webrtc = webrtc_state(&self.webrtc_status);
        let cpu = cpu_status();
        let slice_cache = self.slice_cache_runtime.snapshot();
        let realtime = realtime_state(&self.realtime_runtime);
        let status = node_status_from_resources(&[
            database_status_severity(database.status),
            redis_status_severity(redis.status),
            cluster_status_severity(cluster.status),
            ws_ticket_status_severity(ws_ticket.status),
            email_status_severity(email.status),
            livestream_status_severity(livestream.status),
            memory_status_severity(memory.status),
            webrtc_status_severity(webrtc.status),
            cpu_status_severity(cpu.status),
            slice_cache_status_severity(slice_cache.status),
        ]);

        ServerStateNode {
            node_id: self.realtime_runtime.node_id().to_string(),
            status,
            updated_at: unix_timestamp_secs(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            api_address: self.runtime_params.advertise_api_address.clone(),
            realtime,
            database,
            redis,
            cluster,
            ws_ticket,
            email,
            livestream,
            memory,
            webrtc,
            cpu,
            slice_cache,
        }
    }

    async fn collect_all_server_state(&self) -> ServerStateResult<ServerStateResponse> {
        let local = self.collect_local_server_state().await;
        let mut nodes = vec![local];
        let mut failures = Vec::new();

        if let Some(cluster_runtime) = &self.cluster_runtime {
            let remote_nodes = cluster_runtime.remote_routable_nodes().await?;
            let Some(remote_client) = &self.remote_client else {
                return Ok(response_for_server_state_nodes(
                    ServerStateScope::All,
                    nodes,
                    failures,
                ));
            };
            let mut futures = futures::stream::FuturesUnordered::new();
            for node in remote_nodes {
                let remote_client = remote_client.clone();
                futures.push(async move {
                    let node_id = node.node_id.clone();
                    remote_client
                        .remote_node_server_state(&node)
                        .await
                        .map_err(|error| ServerStateFailure {
                            node_id,
                            error: error.to_string(),
                        })
                });
            }
            while let Some(result) = futures.next().await {
                match result {
                    Ok(status) => nodes.push(status),
                    Err(failure) => failures.push(failure),
                }
            }
        }

        Ok(response_for_server_state_nodes(
            ServerStateScope::All,
            nodes,
            failures,
        ))
    }

    fn require_cluster_runtime(
        &self,
        target_node_id: &str,
    ) -> ServerStateResult<&Arc<dyn ServerStateClusterRuntime>> {
        self.cluster_runtime
            .as_ref()
            .ok_or_else(|| ServerStateError::ClusterUnavailable(target_node_id.to_string()))
    }

    fn require_remote_client(
        &self,
        target_node_id: &str,
    ) -> ServerStateResult<&Arc<dyn ServerStateRemoteClient>> {
        self.remote_client
            .as_ref()
            .ok_or_else(|| ServerStateError::ClusterUnavailable(target_node_id.to_string()))
    }

    async fn cluster_state(&self, cluster_enabled: bool) -> ServerStateCluster {
        let metrics = self.realtime_runtime.metrics();
        let node_id_empty = self.realtime_runtime.node_id().is_empty();
        let nodes = self.cluster_nodes().await;
        let status = if !cluster_enabled {
            ServerStateClusterStatus::Disabled
        } else if node_id_empty || !metrics.distributed_enabled {
            ServerStateClusterStatus::Unhealthy
        } else {
            ServerStateClusterStatus::Healthy
        };
        let message = if node_id_empty {
            Some("cluster node ID is empty".to_string())
        } else if cluster_enabled && !metrics.distributed_enabled {
            Some("cluster distributed realtime transport is disconnected".to_string())
        } else {
            None
        };
        ServerStateCluster {
            status,
            enabled: cluster_enabled,
            discovery_mode: self.runtime_params.cluster.discovery_mode.clone(),
            distributed_realtime_enabled: metrics.distributed_enabled,
            node_id_empty,
            routable_node_count: saturating_u32(nodes.len()),
            nodes,
            message,
        }
    }

    async fn cluster_nodes(&self) -> Vec<ServerStateClusterNode> {
        let Some(cluster_runtime) = &self.cluster_runtime else {
            return Vec::new();
        };
        let Ok(nodes) = cluster_runtime.remote_routable_nodes().await else {
            return Vec::new();
        };
        nodes
            .into_iter()
            .map(|node| ServerStateClusterNode {
                node_id: node.node_id,
                api_address: node.api_address,
                last_heartbeat: node.last_heartbeat,
                epoch: node.epoch,
            })
            .collect()
    }

    async fn check_database_health(&self) -> Result<(), String> {
        match tokio::time::timeout(
            SERVER_STATE_HEALTH_CHECK_TIMEOUT,
            self.user_service.health_check(),
        )
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(format!("Database connection failed: {error}")),
            Err(_) => Err(format!(
                "Database health check timed out after {}s",
                SERVER_STATE_HEALTH_CHECK_TIMEOUT.as_secs()
            )),
        }
    }

    async fn database_state(&self) -> ServerStateDatabase {
        let primary_pool = pool_state(self.user_service.pool());
        let read_pool = pool_state(self.user_service.eventually_consistent_pool());
        let health = self.check_database_health().await;
        ServerStateDatabase {
            status: match health {
                Ok(()) => ServerStateDatabaseStatus::Healthy,
                Err(_) => ServerStateDatabaseStatus::Unhealthy,
            },
            host: self.runtime_params.database.host.clone(),
            port: u32::from(self.runtime_params.database.port),
            database: self.runtime_params.database.name.clone(),
            max_connections: self.runtime_params.database.max_connections,
            min_connections: self.runtime_params.database.min_connections,
            connect_timeout_seconds: self.runtime_params.database.connect_timeout_seconds,
            idle_timeout_seconds: self.runtime_params.database.idle_timeout_seconds,
            max_lifetime_seconds: self.runtime_params.database.max_lifetime_seconds,
            primary_pool,
            read_pool_enabled: !self.runtime_params.database.read_url.trim().is_empty()
                || !self.runtime_params.database.read_host.trim().is_empty(),
            read_host: self.runtime_params.database.read_host.clone(),
            read_port: u32::from(self.runtime_params.database.read_port),
            read_pool,
            message: health.err(),
        }
    }

    async fn redis_state(&self) -> ServerStateRedis {
        let checked_at = std::time::Instant::now();
        let status = self.check_redis_health().await;
        let ping_latency_ms = matches!(status, RedisHealthStatus::Healthy)
            .then(|| checked_at.elapsed().as_secs_f64() * 1000.0);
        let configured = self.redis_runtime.is_some();
        let (state, message) = match status {
            RedisHealthStatus::Healthy => (ServerStateRedisStatus::Healthy, None),
            RedisHealthStatus::NotConfigured => (ServerStateRedisStatus::NotConfigured, None),
            RedisHealthStatus::Unhealthy(error) => (ServerStateRedisStatus::Unhealthy, Some(error)),
        };
        ServerStateRedis {
            status: state,
            configured,
            deployment_mode: redis_deployment_mode_name(&self.runtime_params.redis.deployment_mode)
                .to_string(),
            database: self.runtime_params.redis.database,
            key_prefix: self.runtime_params.redis.key_prefix.clone(),
            connect_timeout_seconds: self.runtime_params.redis.connect_timeout_seconds,
            response_timeout_seconds: self.runtime_params.redis.response_timeout_seconds,
            pipeline_buffer_size: saturating_u64(self.runtime_params.redis.pipeline_buffer_size),
            sentinel_master_name: self
                .runtime_params
                .redis
                .sentinel_master_name
                .clone()
                .unwrap_or_default(),
            sentinel_node_count: saturating_u32(self.runtime_params.redis.sentinel_addresses.len()),
            ping_latency_ms,
            message,
        }
    }

    fn ws_ticket_state(&self, cluster_enabled: bool) -> ServerStateWsTicket {
        let cross_node_capable = self.ws_ticket_service.supports_cluster_runtime();
        let status = if cluster_enabled && !cross_node_capable {
            ServerStateWsTicketStatus::Unhealthy
        } else {
            ServerStateWsTicketStatus::Healthy
        };
        ServerStateWsTicket {
            status,
            cross_node_capable,
            message: Some(ws_ticket_health(self.ws_ticket_service.as_ref())),
        }
    }

    fn email_state(&self) -> ServerStateEmail {
        let configured = self
            .email_service
            .as_ref()
            .is_some_and(|service| service.is_configured());
        ServerStateEmail {
            status: if configured {
                ServerStateEmailStatus::Configured
            } else {
                ServerStateEmailStatus::NotConfigured
            },
            configured,
        }
    }

    async fn livestream_state(&self) -> ServerStateLivestream {
        let hls_storage = &self.runtime_params.livestream.hls_storage;
        let runtime = if let Some(runtime) = &self.livestream_runtime {
            runtime.snapshot().await
        } else {
            ServerStateLivestreamSnapshot::default()
        };
        ServerStateLivestream {
            status: if self.live_streaming_configured {
                ServerStateLivestreamStatus::Configured
            } else {
                ServerStateLivestreamStatus::NotConfigured
            },
            configured: self.live_streaming_configured,
            active_publisher_count: runtime.active_publisher_count,
            active_room_count: runtime.active_room_count,
            rtmp_port: u32::from(self.runtime_params.livestream.rtmp_port),
            public_rtmp_host: self.runtime_params.livestream.public_rtmp_host.clone(),
            gop_cache_size: self.runtime_params.livestream.gop_cache_size,
            gop_cache_max_memory_mb: self.runtime_params.livestream.gop_cache_max_memory_mb,
            stream_timeout_seconds: self.runtime_params.livestream.stream_timeout_seconds,
            hls_storage_backend: hls_storage_backend_name(hls_storage.backend).to_string(),
            hls_storage_path: hls_storage.path.clone(),
            hls_memory_max_mb: hls_storage.memory_max_mb,
        }
    }

    async fn check_redis_health(&self) -> RedisHealthStatus {
        let Some(redis_runtime) = &self.redis_runtime else {
            return RedisHealthStatus::NotConfigured;
        };
        let redis_conn =
            match tokio::time::timeout(SERVER_STATE_HEALTH_CHECK_TIMEOUT, redis_runtime.snapshot())
                .await
            {
                Ok(Ok(conn)) => conn,
                Ok(Err(error)) => {
                    return RedisHealthStatus::Unhealthy(format!("Redis snapshot failed: {error}"));
                }
                Err(_) => {
                    return RedisHealthStatus::Unhealthy(format!(
                        "Redis snapshot timed out after {}s",
                        SERVER_STATE_HEALTH_CHECK_TIMEOUT.as_secs()
                    ));
                }
            };
        check_redis_health_from_conn(redis_conn).await
    }
}

pub fn validate_server_state_selection(
    node_id: Option<&str>,
    all_nodes: bool,
) -> ServerStateResult<Option<String>> {
    let node_id = node_id.unwrap_or_default().trim();
    if all_nodes && !node_id.is_empty() {
        return Err(ServerStateError::InvalidSelection);
    }
    Ok((!node_id.is_empty()).then(|| node_id.to_string()))
}

#[must_use]
pub fn response_for_server_state_nodes(
    scope: ServerStateScope,
    nodes: Vec<ServerStateNode>,
    failures: Vec<ServerStateFailure>,
) -> ServerStateResponse {
    ServerStateResponse {
        scope,
        summary: summarize_server_state(&nodes, &failures),
        nodes,
        failures,
    }
}

#[must_use]
pub fn summarize_server_state(
    nodes: &[ServerStateNode],
    failures: &[ServerStateFailure],
) -> ServerStateSummary {
    let healthy_nodes = saturating_i64(
        nodes
            .iter()
            .filter(|node| node.status == ServerStateNodeStatus::Healthy)
            .count(),
    );
    let degraded_nodes = saturating_i64(
        nodes
            .iter()
            .filter(|node| node.status == ServerStateNodeStatus::Degraded)
            .count(),
    );
    let unhealthy_nodes = saturating_i64(
        nodes
            .iter()
            .filter(|node| node.status == ServerStateNodeStatus::Unhealthy)
            .count(),
    );
    let failed_nodes = saturating_i64(failures.len());
    let status = if unhealthy_nodes > 0 || failed_nodes > 0 {
        ServerStateNodeStatus::Unhealthy
    } else if degraded_nodes > 0 {
        ServerStateNodeStatus::Degraded
    } else {
        ServerStateNodeStatus::Healthy
    };
    ServerStateSummary {
        status,
        healthy_nodes,
        degraded_nodes,
        unhealthy_nodes,
        failed_nodes,
    }
}

#[must_use]
pub fn ws_ticket_health(svc: &dyn WebSocketTicketService) -> String {
    if svc.supports_cluster_runtime() {
        "healthy (cross-node capable ticket storage)".to_string()
    } else {
        "healthy (single-node ticket storage)".to_string()
    }
}

#[must_use]
pub fn ws_ticket_backend_is_safe_for_mode(
    svc: &dyn WebSocketTicketService,
    cluster_mode: bool,
) -> bool {
    !cluster_mode || svc.supports_cluster_runtime()
}

#[must_use]
pub fn email_health(svc: &EmailService) -> String {
    if svc.is_configured() {
        "configured".to_string()
    } else {
        "not configured".to_string()
    }
}

#[must_use]
pub fn check_memory_health() -> Option<ServerStateMemoryHealth> {
    #[cfg(target_os = "linux")]
    {
        check_memory_health_linux()
    }
    #[cfg(target_os = "macos")]
    {
        check_memory_health_macos()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RedisHealthStatus {
    Healthy,
    NotConfigured,
    Unhealthy(String),
}

async fn check_redis_health_from_conn(
    mut conn: redis::aio::ConnectionManager,
) -> RedisHealthStatus {
    match tokio::time::timeout(
        SERVER_STATE_HEALTH_CHECK_TIMEOUT,
        redis::cmd("PING").query_async::<String>(&mut conn),
    )
    .await
    {
        Ok(Ok(_)) => RedisHealthStatus::Healthy,
        Ok(Err(error)) => RedisHealthStatus::Unhealthy(format!("Redis ping failed: {error}")),
        Err(_) => RedisHealthStatus::Unhealthy(format!(
            "Redis health check timed out after {}s",
            SERVER_STATE_HEALTH_CHECK_TIMEOUT.as_secs()
        )),
    }
}

fn cpu_status() -> ServerStateCpu {
    let load_average = load_average();
    let available_parallelism = std::thread::available_parallelism()
        .ok()
        .and_then(|value| u32::try_from(value.get()).ok())
        .unwrap_or_default();
    let current_load_1m = load_average.map(|load| load[0]);
    let load_ratio_1m = cpu_load_ratio(available_parallelism, current_load_1m);
    let status = cpu_status_from_load(available_parallelism, current_load_1m);
    ServerStateCpu {
        status,
        available_parallelism,
        current_load_1m,
        load_ratio_1m,
        load_average_1m: current_load_1m,
        load_average_5m: load_average.map(|load| load[1]),
        load_average_15m: load_average.map(|load| load[2]),
    }
}

fn cpu_load_ratio(available_parallelism: u32, load_1m: Option<f64>) -> Option<f64> {
    if available_parallelism == 0 {
        return None;
    }
    load_1m.map(|load| load / f64::from(available_parallelism))
}

fn cpu_status_from_load(available_parallelism: u32, load_1m: Option<f64>) -> ServerStateCpuStatus {
    let Some(load_1m) = load_1m else {
        return ServerStateCpuStatus::Unknown;
    };
    if available_parallelism == 0 {
        return ServerStateCpuStatus::Unknown;
    }
    let capacity = f64::from(available_parallelism);
    if load_1m > capacity * 4.0 {
        ServerStateCpuStatus::Unhealthy
    } else if load_1m > capacity * 2.0 {
        ServerStateCpuStatus::Degraded
    } else {
        ServerStateCpuStatus::Healthy
    }
}

#[cfg(unix)]
fn load_average() -> Option<[f64; 3]> {
    #[cfg(target_os = "linux")]
    {
        load_average_linux()
    }
    #[cfg(target_os = "macos")]
    {
        load_average_macos()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(not(unix))]
fn load_average() -> Option<[f64; 3]> {
    None
}

#[cfg(target_os = "linux")]
fn load_average_linux() -> Option<[f64; 3]> {
    let content = std::fs::read_to_string("/proc/loadavg").ok()?;
    let mut parts = content.split_whitespace();
    Some([
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ])
}

#[cfg(target_os = "macos")]
fn load_average_macos() -> Option<[f64; 3]> {
    let output = std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .ok()?;
    let content = String::from_utf8_lossy(&output.stdout);
    let cleaned = content.replace(['{', '}'], "");
    let mut parts = cleaned.split_whitespace();
    Some([
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ])
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn memory_usage_percent(used_bytes: u64, total_bytes: u64) -> Option<f64> {
    if total_bytes == 0 {
        return None;
    }
    let scaled_percent =
        (u128::from(used_bytes) * 10_000 + u128::from(total_bytes / 2)) / u128::from(total_bytes);
    let scaled_percent = u32::try_from(scaled_percent).ok()?;
    Some(f64::from(scaled_percent) / 100.0)
}

#[cfg(target_os = "linux")]
fn check_memory_health_linux() -> Option<ServerStateMemoryHealth> {
    check_cgroup_memory().or_else(check_proc_meminfo)
}

#[cfg(target_os = "linux")]
fn check_cgroup_memory() -> Option<ServerStateMemoryHealth> {
    use std::fs;

    let (limit, current) = if let (Ok(limit_str), Ok(current_str)) = (
        fs::read_to_string("/sys/fs/cgroup/memory.max"),
        fs::read_to_string("/sys/fs/cgroup/memory.current"),
    ) {
        let limit = limit_str.trim().parse::<u64>().ok()?;
        let current = current_str.trim().parse::<u64>().ok()?;
        (limit, current)
    } else if let (Ok(limit_str), Ok(usage_str)) = (
        fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes"),
        fs::read_to_string("/sys/fs/cgroup/memory/memory.usage_in_bytes"),
    ) {
        let limit = limit_str.trim().parse::<u64>().ok()?;
        let current = usage_str.trim().parse::<u64>().ok()?;
        (limit, current)
    } else {
        return None;
    };

    if limit == 0 || limit >= (1u64 << 62) {
        return None;
    }

    memory_health_from_usage(current, limit)
}

#[cfg(target_os = "linux")]
fn check_proc_meminfo() -> Option<ServerStateMemoryHealth> {
    use std::fs;

    let meminfo = fs::read_to_string("/proc/meminfo").ok()?;
    let mut total_kb: u64 = 0;
    let mut available_kb: u64 = 0;

    for line in meminfo.lines() {
        if line.starts_with("MemTotal:") {
            total_kb = line
                .split(':')
                .nth(1)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()?;
        } else if line.starts_with("MemAvailable:") {
            available_kb = line
                .split(':')
                .nth(1)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()?;
        }
        if total_kb > 0 && available_kb > 0 {
            break;
        }
    }

    if total_kb == 0 {
        return None;
    }
    let used_kb = total_kb.saturating_sub(available_kb);
    memory_health_from_usage(used_kb.saturating_mul(1024), total_kb.saturating_mul(1024))
}

#[cfg(target_os = "macos")]
fn check_memory_health_macos() -> Option<ServerStateMemoryHealth> {
    use std::process::Command;

    let total_output = Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    let total_bytes: u64 = String::from_utf8_lossy(&total_output.stdout)
        .trim()
        .parse()
        .ok()?;

    let vm_stat_output = Command::new("vm_stat").output().ok()?;
    let vm_stat = String::from_utf8_lossy(&vm_stat_output.stdout);

    let mut page_size: u64 = 4096;
    let mut free_pages: u64 = 0;
    let mut inactive_pages: u64 = 0;

    for line in vm_stat.lines() {
        if line.contains("page size of") {
            if let Some(start) = line.find("page size of ") {
                let rest = &line[start + 13..];
                if let Some(end) = rest.find(" bytes") {
                    if let Ok(parsed_page_size) = rest[..end].parse::<u64>() {
                        page_size = parsed_page_size;
                    }
                }
            }
        } else if line.starts_with("Pages free:") {
            let num_str: String = line
                .split(':')
                .nth(1)?
                .trim()
                .trim_end_matches('.')
                .chars()
                .filter(char::is_ascii_digit)
                .collect();
            free_pages = num_str.parse().ok()?;
        } else if line.starts_with("Pages inactive:") {
            let num_str: String = line
                .split(':')
                .nth(1)?
                .trim()
                .trim_end_matches('.')
                .chars()
                .filter(char::is_ascii_digit)
                .collect();
            inactive_pages = num_str.parse().ok()?;
        }
    }

    let available_bytes = (free_pages + inactive_pages) * page_size;
    let used_bytes = total_bytes.saturating_sub(available_bytes);
    memory_health_from_usage(used_bytes, total_bytes)
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn memory_health_from_usage(used_bytes: u64, total_bytes: u64) -> Option<ServerStateMemoryHealth> {
    let usage_percent = memory_usage_percent(used_bytes, total_bytes)?;
    let usage_percent = usage_percent.min(100.0);
    let available_bytes = total_bytes.saturating_sub(used_bytes);
    let status = if usage_percent > MEMORY_UNHEALTHY_THRESHOLD_PERCENT {
        ServerStateMemoryStatus::Unhealthy
    } else {
        ServerStateMemoryStatus::Healthy
    };
    Some(ServerStateMemoryHealth {
        used_bytes,
        total_bytes,
        available_bytes,
        usage_percent: (usage_percent * 100.0).round() / 100.0,
        status,
    })
}

fn memory_state() -> ServerStateMemory {
    check_memory_health().map_or(
        ServerStateMemory {
            status: ServerStateMemoryStatus::Unknown,
            used_bytes: None,
            total_bytes: None,
            available_bytes: None,
            usage_percent: None,
        },
        |memory| ServerStateMemory {
            status: memory.status,
            used_bytes: Some(memory.used_bytes),
            total_bytes: Some(memory.total_bytes),
            available_bytes: Some(memory.available_bytes),
            usage_percent: Some(memory.usage_percent),
        },
    )
}

fn realtime_state(runtime: &Arc<dyn ServerStateRealtimeRuntime>) -> ServerStateRealtime {
    let metrics = runtime.metrics();
    ServerStateRealtime {
        distributed_enabled: metrics.distributed_enabled,
        connection_count: metrics.connection_count,
    }
}

#[must_use]
pub fn livestream_snapshot_from_publishers<I>(
    publishers: I,
    local_node_id: &str,
) -> ServerStateLivestreamSnapshot
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut active_publisher_count = 0u64;
    let mut rooms = HashSet::new();
    for (room_id, node_id) in publishers {
        if local_node_id.is_empty() || node_id == local_node_id {
            active_publisher_count = active_publisher_count.saturating_add(1);
            rooms.insert(room_id);
        }
    }
    ServerStateLivestreamSnapshot {
        active_publisher_count,
        active_room_count: u64::try_from(rooms.len()).unwrap_or(u64::MAX),
    }
}

fn webrtc_state(status: &WebRtcRuntimeStatus) -> ServerStateWebRtc {
    let state = match status.builtin_stun_state {
        crate::service::BuiltinStunRuntimeState::Disabled => ServerStateWebRtcStatus::Disabled,
        crate::service::BuiltinStunRuntimeState::Running => ServerStateWebRtcStatus::Healthy,
        crate::service::BuiltinStunRuntimeState::Degraded => ServerStateWebRtcStatus::Degraded,
    };
    ServerStateWebRtc {
        status: state,
        mode: status.mode.as_str().to_string(),
        builtin_stun_configured: status.builtin_stun_configured,
        builtin_stun_state: status.builtin_stun_state.as_str().to_string(),
        reason: status.reason.as_str().to_string(),
        local_addr: status.local_addr.clone().unwrap_or_default(),
        external_addr: status.external_addr.clone().unwrap_or_default(),
        message: status.message.clone(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceSeverity {
    Healthy,
    Degraded,
    Unhealthy,
}

fn node_status_from_resources(statuses: &[ResourceSeverity]) -> ServerStateNodeStatus {
    if statuses
        .iter()
        .any(|status| matches!(status, ResourceSeverity::Unhealthy))
    {
        ServerStateNodeStatus::Unhealthy
    } else if statuses
        .iter()
        .any(|status| matches!(status, ResourceSeverity::Degraded))
    {
        ServerStateNodeStatus::Degraded
    } else {
        ServerStateNodeStatus::Healthy
    }
}

fn database_status_severity(status: ServerStateDatabaseStatus) -> ResourceSeverity {
    match status {
        ServerStateDatabaseStatus::Healthy => ResourceSeverity::Healthy,
        ServerStateDatabaseStatus::Unhealthy => ResourceSeverity::Unhealthy,
    }
}

fn redis_status_severity(status: ServerStateRedisStatus) -> ResourceSeverity {
    match status {
        ServerStateRedisStatus::Healthy | ServerStateRedisStatus::NotConfigured => {
            ResourceSeverity::Healthy
        }
        ServerStateRedisStatus::Unhealthy => ResourceSeverity::Unhealthy,
    }
}

fn cluster_status_severity(status: ServerStateClusterStatus) -> ResourceSeverity {
    match status {
        ServerStateClusterStatus::Healthy | ServerStateClusterStatus::Disabled => {
            ResourceSeverity::Healthy
        }
        ServerStateClusterStatus::Unhealthy => ResourceSeverity::Unhealthy,
    }
}

fn ws_ticket_status_severity(status: ServerStateWsTicketStatus) -> ResourceSeverity {
    match status {
        ServerStateWsTicketStatus::Healthy => ResourceSeverity::Healthy,
        ServerStateWsTicketStatus::Unhealthy => ResourceSeverity::Unhealthy,
    }
}

fn email_status_severity(_status: ServerStateEmailStatus) -> ResourceSeverity {
    ResourceSeverity::Healthy
}

fn livestream_status_severity(_status: ServerStateLivestreamStatus) -> ResourceSeverity {
    ResourceSeverity::Healthy
}

fn memory_status_severity(status: ServerStateMemoryStatus) -> ResourceSeverity {
    match status {
        ServerStateMemoryStatus::Healthy => ResourceSeverity::Healthy,
        ServerStateMemoryStatus::Unhealthy => ResourceSeverity::Unhealthy,
        ServerStateMemoryStatus::Unknown => ResourceSeverity::Degraded,
    }
}

fn webrtc_status_severity(status: ServerStateWebRtcStatus) -> ResourceSeverity {
    match status {
        ServerStateWebRtcStatus::Healthy | ServerStateWebRtcStatus::Disabled => {
            ResourceSeverity::Healthy
        }
        ServerStateWebRtcStatus::Degraded => ResourceSeverity::Degraded,
    }
}

fn cpu_status_severity(status: ServerStateCpuStatus) -> ResourceSeverity {
    match status {
        ServerStateCpuStatus::Healthy => ResourceSeverity::Healthy,
        ServerStateCpuStatus::Degraded | ServerStateCpuStatus::Unknown => {
            ResourceSeverity::Degraded
        }
        ServerStateCpuStatus::Unhealthy => ResourceSeverity::Unhealthy,
    }
}

fn slice_cache_status_severity(_status: ServerStateSliceCacheStatus) -> ResourceSeverity {
    ResourceSeverity::Healthy
}

fn pool_state(pool: &sqlx::PgPool) -> ServerStateDatabasePool {
    let size = pool.size();
    let idle_connections = saturating_u32(pool.num_idle());
    ServerStateDatabasePool {
        size,
        idle_connections,
        active_connections: size.saturating_sub(idle_connections),
    }
}

fn redis_deployment_mode_name(mode: &RedisDeploymentMode) -> &'static str {
    match mode {
        RedisDeploymentMode::Standalone => "standalone",
        RedisDeploymentMode::Sentinel => "sentinel",
    }
}

fn hls_storage_backend_name(backend: ServerStateHlsStorageBackend) -> &'static str {
    match backend {
        ServerStateHlsStorageBackend::Memory => "memory",
        ServerStateHlsStorageBackend::File => "file",
        ServerStateHlsStorageBackend::SharedFile => "shared_file",
        ServerStateHlsStorageBackend::Oss => "oss",
    }
}

fn unix_timestamp_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

fn saturating_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn saturating_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn saturating_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}
