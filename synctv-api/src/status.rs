use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tonic::codec::CompressionEncoding;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Response, Status};

use crate::http::AppError;
use crate::runtime::RealtimeEventService;
pub use synctv_core::service::{
    livestream_snapshot_from_publishers, response_for_server_state_nodes as response_for_nodes,
    validate_server_state_selection, ServerStateCluster as ClusterStatus,
    ServerStateClusterNode as ClusterNodeStatus, ServerStateClusterRuntime,
    ServerStateClusterStatus, ServerStateClusterTarget, ServerStateCpu as CpuStatus,
    ServerStateCpuStatus, ServerStateDatabase as DatabaseStatus,
    ServerStateDatabasePool as DatabasePoolStatus, ServerStateDatabaseStatus,
    ServerStateEmail as EmailStatus, ServerStateEmailStatus, ServerStateError, ServerStateFailure,
    ServerStateLivestream as LivestreamStatus, ServerStateLivestreamRuntime,
    ServerStateLivestreamSnapshot, ServerStateLivestreamStatus, ServerStateMemory as MemoryStatus,
    ServerStateMemoryStatus, ServerStateNode, ServerStateNodeStatus,
    ServerStateRealtime as RealtimeStatus, ServerStateRealtimeMetrics, ServerStateRealtimeRuntime,
    ServerStateRedis as RedisStatus, ServerStateRedisStatus, ServerStateRemoteClient,
    ServerStateResponse, ServerStateResult, ServerStateScope, ServerStateSelection,
    ServerStateService as ServerStateRuntime, ServerStateServiceDependencies,
    ServerStateSliceCache as SliceCacheStatus, ServerStateSliceCacheRuntime,
    ServerStateSliceCacheStatus, ServerStateSummary, ServerStateWebRtc as WebRtcStatus,
    ServerStateWebRtcStatus, ServerStateWsTicket as WsTicketStatus, ServerStateWsTicketStatus,
};
use synctv_realtime::sync::ConnectionRuntime;

const SERVER_STATE_REMOTE_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const SERVER_STATE_REMOTE_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

type ClusterServerStateClient =
    synctv_cluster::grpc::ServerStateServiceClient<tonic::transport::Channel>;

impl From<ServerStateError> for AppError {
    fn from(error: ServerStateError) -> Self {
        match error {
            ServerStateError::InvalidSelection => AppError::bad_request(error.to_string()),
            ServerStateError::ClusterUnavailable(_)
            | ServerStateError::MissingClusterSecret
            | ServerStateError::InvalidClusterSecret
            | ServerStateError::Cluster(_)
            | ServerStateError::RemoteRequest { .. }
            | ServerStateError::RemoteDecode { .. } => AppError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                error.to_string(),
            ),
        }
    }
}

#[must_use]
pub fn server_state_runtime_from_router_config(
    config: &crate::http::RouterConfig,
) -> ServerStateRuntime {
    let cluster_runtime = config.cluster_client.as_ref().map(|client| {
        Arc::new(ApiServerStateClusterRuntime {
            client: client.clone(),
        }) as Arc<dyn ServerStateClusterRuntime>
    });
    let remote_client = config.cluster_client.as_ref().map(|_| {
        Arc::new(ApiServerStateRemoteClient {
            config: config.config.clone(),
        }) as Arc<dyn ServerStateRemoteClient>
    });

    ServerStateRuntime::new(ServerStateServiceDependencies {
        config: config.config.clone(),
        user_service: config.user_service.clone(),
        realtime_runtime: Arc::new(ApiServerStateRealtimeRuntime {
            event_service: config.event_service.clone(),
            connection_manager: config.connection_manager.clone(),
        }),
        ws_ticket_service: config.ws_ticket_service.clone(),
        redis_runtime: config.redis_runtime.clone(),
        email_service: config.email_service.clone(),
        live_streaming_configured: config.live_streaming_infrastructure.is_some(),
        livestream_runtime: config
            .live_streaming_infrastructure
            .as_ref()
            .map(|infrastructure| {
                Arc::new(ApiServerStateLivestreamRuntime {
                    infrastructure: infrastructure.clone(),
                    local_node_id: config.event_service.node_id().to_string(),
                }) as Arc<dyn ServerStateLivestreamRuntime>
            }),
        cluster_runtime,
        remote_client,
        slice_cache_runtime: Arc::new(ApiServerStateSliceCacheRuntime {
            cache: config.proxy_slice_cache.clone(),
        }),
        webrtc_status: config.webrtc_status.clone(),
    })
}

pub async fn collect_server_state(
    runtime: &ServerStateRuntime,
    selection: ServerStateSelection,
) -> ServerStateResult<ServerStateResponse> {
    runtime.collect_server_state(selection).await
}

pub async fn collect_local_server_state(runtime: &ServerStateRuntime) -> ServerStateNode {
    runtime.collect_local_server_state().await
}

struct ApiServerStateRealtimeRuntime {
    event_service: Arc<dyn RealtimeEventService>,
    connection_manager: Arc<dyn ConnectionRuntime>,
}

impl ServerStateRealtimeRuntime for ApiServerStateRealtimeRuntime {
    fn metrics(&self) -> ServerStateRealtimeMetrics {
        let metrics = self.event_service.metrics();
        ServerStateRealtimeMetrics {
            distributed_enabled: metrics.distributed_enabled,
            connection_count: saturating_u64(self.connection_manager.connection_count()),
        }
    }

    fn node_id(&self) -> &str {
        self.event_service.node_id()
    }
}

struct ApiServerStateLivestreamRuntime {
    infrastructure: Arc<synctv_livestream::LiveStreamingInfrastructure>,
    local_node_id: String,
}

#[async_trait]
impl ServerStateLivestreamRuntime for ApiServerStateLivestreamRuntime {
    async fn snapshot(&self) -> ServerStateLivestreamSnapshot {
        match self.infrastructure.list_active_publishers().await {
            Ok(publishers) => livestream_snapshot_from_publishers(
                publishers
                    .into_iter()
                    .map(|publisher| (publisher.room_id, publisher.publisher.node_id)),
                &self.local_node_id,
            ),
            Err(error) => {
                tracing::warn!(%error, "failed to collect livestream publisher metrics");
                ServerStateLivestreamSnapshot::default()
            }
        }
    }
}

struct ApiServerStateClusterRuntime {
    client: Arc<synctv_cluster::grpc::ClusterClient>,
}

#[async_trait]
impl ServerStateClusterRuntime for ApiServerStateClusterRuntime {
    async fn resolve_routable_node(
        &self,
        target_node_id: &str,
    ) -> ServerStateResult<ServerStateClusterTarget> {
        let node = self
            .client
            .resolve_routable_node(target_node_id)
            .await
            .map_err(|error| ServerStateError::Cluster(error.to_string()))?;
        Ok(cluster_node_to_target(node))
    }

    async fn remote_routable_nodes(&self) -> ServerStateResult<Vec<ServerStateClusterTarget>> {
        let nodes = self
            .client
            .remote_routable_nodes()
            .await
            .map_err(|error| ServerStateError::Cluster(error.to_string()))?;
        Ok(nodes.into_iter().map(cluster_node_to_target).collect())
    }
}

struct ApiServerStateRemoteClient {
    config: Arc<synctv_core::Config>,
}

#[async_trait]
impl ServerStateRemoteClient for ApiServerStateRemoteClient {
    async fn remote_node_server_state(
        &self,
        node: &ServerStateClusterTarget,
    ) -> ServerStateResult<ServerStateNode> {
        let mut request =
            Request::new(synctv_cluster::grpc::synctv::cluster::GetServerStateRequest {});
        attach_cluster_secret(&mut request, &self.config.cluster.secret)?;
        let mut client = server_state_client(&self.config, &node.api_address).await?;
        let response = client.get_server_state(request).await.map_err(|error| {
            ServerStateError::RemoteRequest {
                node_id: node.node_id.clone(),
                error: error.to_string(),
            }
        })?;
        cluster_proto_to_server_state_node(response.into_inner()).map_err(|error| {
            ServerStateError::RemoteDecode {
                node_id: node.node_id.clone(),
                error,
            }
        })
    }
}

struct ApiServerStateSliceCacheRuntime {
    cache: Arc<synctv_proxy::slice_cache::SliceCache>,
}

impl ServerStateSliceCacheRuntime for ApiServerStateSliceCacheRuntime {
    fn snapshot(&self) -> SliceCacheStatus {
        let stats = self.cache.stats();
        SliceCacheStatus {
            status: if stats.engine_enabled {
                ServerStateSliceCacheStatus::Healthy
            } else {
                ServerStateSliceCacheStatus::Disabled
            },
            engine_enabled: stats.engine_enabled,
            backend: stats.backend,
            file_cache_dir: stats.file_cache_dir.unwrap_or_default(),
            slice_size: stats.slice_size,
            max_cache_size: stats.max_cache_size,
            segment_ttl_secs: stats.segment_ttl_secs,
            stale_max_age_secs: stats.stale_max_age_secs,
            stale_while_revalidate: stats.stale_while_revalidate,
            eviction_interval_secs: stats.eviction_interval_secs,
            watermark_ratio: stats.watermark_ratio,
            current_size_bytes: stats.current_size_bytes,
            entry_count: stats.entry_count,
            metadata_entries: stats.metadata_entries,
            updating_entries: stats.updating_entries,
            lock_count: stats.lock_count,
            usage_ratio: stats.usage_ratio,
        }
    }
}

fn cluster_node_to_target(node: synctv_cluster::discovery::NodeInfo) -> ServerStateClusterTarget {
    ServerStateClusterTarget {
        node_id: node.node_id,
        api_address: node.api_address,
        last_heartbeat: node.last_heartbeat.timestamp(),
        epoch: node.epoch,
    }
}

fn server_state_uri(address: &str) -> String {
    if address.starts_with("http://") || address.starts_with("https://") {
        address.to_string()
    } else {
        format!("http://{address}")
    }
}

async fn server_state_client(
    config: &synctv_core::Config,
    address: &str,
) -> ServerStateResult<ClusterServerStateClient> {
    let endpoint = Endpoint::from_shared(server_state_uri(address))
        .map_err(|error| ServerStateError::Cluster(format!("invalid node address: {error}")))?
        .connect_timeout(SERVER_STATE_REMOTE_CONNECT_TIMEOUT)
        .timeout(SERVER_STATE_REMOTE_REQUEST_TIMEOUT);
    let channel: Channel = endpoint.connect().await.map_err(|error| {
        ServerStateError::Cluster(format!("failed to connect to {address}: {error}"))
    })?;
    let client = synctv_cluster::grpc::ServerStateServiceClient::new(channel)
        .max_decoding_message_size(config.server.grpc_max_message_size_bytes)
        .max_encoding_message_size(config.server.grpc_max_message_size_bytes);
    let client = if config.server.grpc_compression_enabled {
        client
            .accept_compressed(CompressionEncoding::Gzip)
            .send_compressed(CompressionEncoding::Gzip)
    } else {
        client
    };
    Ok(client)
}

fn attach_cluster_secret<T>(request: &mut Request<T>, secret: &str) -> ServerStateResult<()> {
    if secret.is_empty() {
        return Err(ServerStateError::MissingClusterSecret);
    }
    synctv_cluster::grpc::attach_cluster_secret(request, secret)
        .map_err(|_| ServerStateError::InvalidClusterSecret)?;
    Ok(())
}

#[derive(Clone)]
pub struct ServerStateGrpcService {
    runtime: Arc<ServerStateRuntime>,
    auth: synctv_cluster::grpc::ClusterAuthInterceptor,
}

impl ServerStateGrpcService {
    #[must_use]
    pub fn new(runtime: Arc<ServerStateRuntime>, cluster_secret: String) -> Self {
        Self {
            runtime,
            auth: synctv_cluster::grpc::ClusterAuthInterceptor::new(cluster_secret),
        }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl synctv_cluster::grpc::synctv::cluster::server_state_service_server::ServerStateService
    for ServerStateGrpcService
{
    async fn get_server_state(
        &self,
        request: Request<synctv_cluster::grpc::synctv::cluster::GetServerStateRequest>,
    ) -> Result<Response<synctv_cluster::grpc::synctv::cluster::ServerStateNode>, Status> {
        self.auth.validate_metadata(request.metadata())?;
        let state = self.runtime.collect_local_server_state().await;
        Ok(Response::new(server_state_node_to_cluster_proto(state)))
    }
}

fn server_state_node_to_cluster_proto(
    node: ServerStateNode,
) -> synctv_cluster::grpc::synctv::cluster::ServerStateNode {
    use synctv_cluster::grpc::synctv::cluster as proto;

    synctv_cluster::grpc::synctv::cluster::ServerStateNode {
        node_id: node.node_id,
        status: node_status_to_cluster_proto(node.status),
        updated_at: node.updated_at,
        version: node.version,
        api_address: node.api_address,
        realtime: Some(proto::ServerStateRealtime {
            distributed_enabled: node.realtime.distributed_enabled,
            connection_count: node.realtime.connection_count,
        }),
        database: Some(proto::ServerStateDatabase {
            status: database_status_to_cluster_proto(node.database.status),
            host: node.database.host,
            port: node.database.port,
            database: node.database.database,
            max_connections: node.database.max_connections,
            min_connections: node.database.min_connections,
            connect_timeout_seconds: node.database.connect_timeout_seconds,
            idle_timeout_seconds: node.database.idle_timeout_seconds,
            max_lifetime_seconds: node.database.max_lifetime_seconds,
            primary_pool: Some(database_pool_to_cluster_proto(&node.database.primary_pool)),
            read_pool_enabled: node.database.read_pool_enabled,
            read_host: node.database.read_host,
            read_port: node.database.read_port,
            read_pool: Some(database_pool_to_cluster_proto(&node.database.read_pool)),
            message: node.database.message.unwrap_or_default(),
        }),
        redis: Some(proto::ServerStateRedis {
            status: redis_status_to_cluster_proto(node.redis.status),
            configured: node.redis.configured,
            deployment_mode: node.redis.deployment_mode,
            database: node.redis.database,
            key_prefix: node.redis.key_prefix,
            connect_timeout_seconds: node.redis.connect_timeout_seconds,
            response_timeout_seconds: node.redis.response_timeout_seconds,
            pipeline_buffer_size: node.redis.pipeline_buffer_size,
            sentinel_master_name: node.redis.sentinel_master_name,
            sentinel_node_count: node.redis.sentinel_node_count,
            ping_latency_ms: node.redis.ping_latency_ms,
            message: node.redis.message.unwrap_or_default(),
        }),
        cluster: Some(proto::ServerStateCluster {
            status: cluster_resource_status_to_cluster_proto(node.cluster.status),
            enabled: node.cluster.enabled,
            discovery_mode: node.cluster.discovery_mode,
            distributed_realtime_enabled: node.cluster.distributed_realtime_enabled,
            node_id_empty: node.cluster.node_id_empty,
            routable_node_count: node.cluster.routable_node_count,
            nodes: node
                .cluster
                .nodes
                .into_iter()
                .map(cluster_node_to_cluster_proto)
                .collect(),
            message: node.cluster.message.unwrap_or_default(),
        }),
        ws_ticket: Some(proto::ServerStateWsTicket {
            status: ws_ticket_status_to_cluster_proto(node.ws_ticket.status),
            cross_node_capable: node.ws_ticket.cross_node_capable,
            message: node.ws_ticket.message.unwrap_or_default(),
        }),
        email: Some(proto::ServerStateEmail {
            status: email_status_to_cluster_proto(node.email.status),
            configured: node.email.configured,
        }),
        livestream: Some(proto::ServerStateLivestream {
            status: livestream_status_to_cluster_proto(node.livestream.status),
            configured: node.livestream.configured,
            active_publisher_count: node.livestream.active_publisher_count,
            active_room_count: node.livestream.active_room_count,
            rtmp_port: node.livestream.rtmp_port,
            public_rtmp_host: node.livestream.public_rtmp_host,
            gop_cache_size: node.livestream.gop_cache_size,
            gop_cache_max_memory_mb: node.livestream.gop_cache_max_memory_mb,
            stream_timeout_seconds: node.livestream.stream_timeout_seconds,
            hls_storage_backend: node.livestream.hls_storage_backend,
            hls_storage_path: node.livestream.hls_storage_path,
            hls_memory_max_mb: node.livestream.hls_memory_max_mb,
        }),
        memory: Some(proto::ServerStateMemory {
            status: memory_status_to_cluster_proto(node.memory.status),
            used_bytes: node.memory.used_bytes,
            total_bytes: node.memory.total_bytes,
            available_bytes: node.memory.available_bytes,
            usage_percent: node.memory.usage_percent,
        }),
        webrtc: Some(proto::ServerStateWebRtc {
            status: webrtc_status_to_cluster_proto(node.webrtc.status),
            mode: node.webrtc.mode,
            builtin_stun_configured: node.webrtc.builtin_stun_configured,
            builtin_stun_state: node.webrtc.builtin_stun_state,
            reason: node.webrtc.reason,
            local_addr: node.webrtc.local_addr,
            external_addr: node.webrtc.external_addr,
            message: node.webrtc.message.unwrap_or_default(),
        }),
        cpu: Some(proto::ServerStateCpu {
            status: cpu_status_to_cluster_proto(node.cpu.status),
            available_parallelism: node.cpu.available_parallelism,
            current_load_1m: node.cpu.current_load_1m,
            load_ratio_1m: node.cpu.load_ratio_1m,
            load_average_1m: node.cpu.load_average_1m,
            load_average_5m: node.cpu.load_average_5m,
            load_average_15m: node.cpu.load_average_15m,
        }),
        slice_cache: Some(proto::ServerStateSliceCache {
            status: slice_cache_status_to_cluster_proto(node.slice_cache.status),
            engine_enabled: node.slice_cache.engine_enabled,
            backend: node.slice_cache.backend,
            file_cache_dir: node.slice_cache.file_cache_dir,
            slice_size: node.slice_cache.slice_size,
            max_cache_size: node.slice_cache.max_cache_size,
            segment_ttl_secs: node.slice_cache.segment_ttl_secs,
            stale_max_age_secs: node.slice_cache.stale_max_age_secs,
            stale_while_revalidate: node.slice_cache.stale_while_revalidate,
            eviction_interval_secs: node.slice_cache.eviction_interval_secs,
            watermark_ratio: node.slice_cache.watermark_ratio,
            current_size_bytes: node.slice_cache.current_size_bytes,
            entry_count: node.slice_cache.entry_count,
            metadata_entries: node.slice_cache.metadata_entries,
            updating_entries: node.slice_cache.updating_entries,
            lock_count: node.slice_cache.lock_count,
            usage_ratio: node.slice_cache.usage_ratio,
        }),
    }
}

fn cluster_proto_to_server_state_node(
    node: synctv_cluster::grpc::synctv::cluster::ServerStateNode,
) -> Result<ServerStateNode, String> {
    let database = node
        .database
        .ok_or_else(|| "missing database in server state response".to_string())?;
    let realtime = node
        .realtime
        .ok_or_else(|| "missing realtime in server state response".to_string())?;
    let redis = node
        .redis
        .ok_or_else(|| "missing redis in server state response".to_string())?;
    let cluster = node
        .cluster
        .ok_or_else(|| "missing cluster in server state response".to_string())?;
    let ws_ticket = node
        .ws_ticket
        .ok_or_else(|| "missing ws_ticket in server state response".to_string())?;
    let email = node
        .email
        .ok_or_else(|| "missing email in server state response".to_string())?;
    let livestream = node
        .livestream
        .ok_or_else(|| "missing livestream in server state response".to_string())?;
    let memory = node
        .memory
        .ok_or_else(|| "missing memory in server state response".to_string())?;
    let webrtc = node
        .webrtc
        .ok_or_else(|| "missing webrtc in server state response".to_string())?;
    let cpu = node
        .cpu
        .ok_or_else(|| "missing cpu in server state response".to_string())?;
    let slice_cache = node
        .slice_cache
        .ok_or_else(|| "missing slice_cache in server state response".to_string())?;
    Ok(ServerStateNode {
        node_id: node.node_id,
        status: cluster_node_status_to_core(node.status),
        updated_at: node.updated_at,
        version: node.version,
        api_address: node.api_address,
        realtime: RealtimeStatus {
            distributed_enabled: realtime.distributed_enabled,
            connection_count: realtime.connection_count,
        },
        database: DatabaseStatus {
            status: cluster_database_status_to_core(database.status),
            host: database.host,
            port: database.port,
            database: database.database,
            max_connections: database.max_connections,
            min_connections: database.min_connections,
            connect_timeout_seconds: database.connect_timeout_seconds,
            idle_timeout_seconds: database.idle_timeout_seconds,
            max_lifetime_seconds: database.max_lifetime_seconds,
            primary_pool: cluster_database_pool_to_core(database.primary_pool.unwrap_or_default()),
            read_pool_enabled: database.read_pool_enabled,
            read_host: database.read_host,
            read_port: database.read_port,
            read_pool: cluster_database_pool_to_core(database.read_pool.unwrap_or_default()),
            message: non_empty_string(database.message),
        },
        redis: RedisStatus {
            status: cluster_redis_status_to_core(redis.status),
            configured: redis.configured,
            deployment_mode: redis.deployment_mode,
            database: redis.database,
            key_prefix: redis.key_prefix,
            connect_timeout_seconds: redis.connect_timeout_seconds,
            response_timeout_seconds: redis.response_timeout_seconds,
            pipeline_buffer_size: redis.pipeline_buffer_size,
            sentinel_master_name: redis.sentinel_master_name,
            sentinel_node_count: redis.sentinel_node_count,
            ping_latency_ms: redis.ping_latency_ms,
            message: non_empty_string(redis.message),
        },
        cluster: ClusterStatus {
            status: cluster_resource_status_to_core(cluster.status),
            enabled: cluster.enabled,
            discovery_mode: cluster.discovery_mode,
            distributed_realtime_enabled: cluster.distributed_realtime_enabled,
            node_id_empty: cluster.node_id_empty,
            routable_node_count: cluster.routable_node_count,
            nodes: cluster
                .nodes
                .into_iter()
                .map(cluster_proto_node_to_core)
                .collect(),
            message: non_empty_string(cluster.message),
        },
        ws_ticket: WsTicketStatus {
            status: cluster_ws_ticket_status_to_core(ws_ticket.status),
            cross_node_capable: ws_ticket.cross_node_capable,
            message: non_empty_string(ws_ticket.message),
        },
        email: EmailStatus {
            status: cluster_email_status_to_core(email.status),
            configured: email.configured,
        },
        livestream: LivestreamStatus {
            status: cluster_livestream_status_to_core(livestream.status),
            configured: livestream.configured,
            active_publisher_count: livestream.active_publisher_count,
            active_room_count: livestream.active_room_count,
            rtmp_port: livestream.rtmp_port,
            public_rtmp_host: livestream.public_rtmp_host,
            gop_cache_size: livestream.gop_cache_size,
            gop_cache_max_memory_mb: livestream.gop_cache_max_memory_mb,
            stream_timeout_seconds: livestream.stream_timeout_seconds,
            hls_storage_backend: livestream.hls_storage_backend,
            hls_storage_path: livestream.hls_storage_path,
            hls_memory_max_mb: livestream.hls_memory_max_mb,
        },
        memory: MemoryStatus {
            status: cluster_memory_status_to_core(memory.status),
            used_bytes: memory.used_bytes,
            total_bytes: memory.total_bytes,
            available_bytes: memory.available_bytes,
            usage_percent: memory.usage_percent,
        },
        webrtc: WebRtcStatus {
            status: cluster_webrtc_status_to_core(webrtc.status),
            mode: webrtc.mode,
            builtin_stun_configured: webrtc.builtin_stun_configured,
            builtin_stun_state: webrtc.builtin_stun_state,
            reason: webrtc.reason,
            local_addr: webrtc.local_addr,
            external_addr: webrtc.external_addr,
            message: non_empty_string(webrtc.message),
        },
        cpu: CpuStatus {
            status: cluster_cpu_status_to_core(cpu.status),
            available_parallelism: cpu.available_parallelism,
            current_load_1m: cpu.current_load_1m,
            load_ratio_1m: cpu.load_ratio_1m,
            load_average_1m: cpu.load_average_1m,
            load_average_5m: cpu.load_average_5m,
            load_average_15m: cpu.load_average_15m,
        },
        slice_cache: SliceCacheStatus {
            status: cluster_slice_cache_status_to_core(slice_cache.status),
            engine_enabled: slice_cache.engine_enabled,
            backend: slice_cache.backend,
            file_cache_dir: slice_cache.file_cache_dir,
            slice_size: slice_cache.slice_size,
            max_cache_size: slice_cache.max_cache_size,
            segment_ttl_secs: slice_cache.segment_ttl_secs,
            stale_max_age_secs: slice_cache.stale_max_age_secs,
            stale_while_revalidate: slice_cache.stale_while_revalidate,
            eviction_interval_secs: slice_cache.eviction_interval_secs,
            watermark_ratio: slice_cache.watermark_ratio,
            current_size_bytes: slice_cache.current_size_bytes,
            entry_count: slice_cache.entry_count,
            metadata_entries: slice_cache.metadata_entries,
            updating_entries: slice_cache.updating_entries,
            lock_count: slice_cache.lock_count,
            usage_ratio: slice_cache.usage_ratio,
        },
    })
}

fn database_pool_to_cluster_proto(
    pool: &DatabasePoolStatus,
) -> synctv_cluster::grpc::synctv::cluster::ServerStateDatabasePool {
    synctv_cluster::grpc::synctv::cluster::ServerStateDatabasePool {
        size: pool.size,
        idle_connections: pool.idle_connections,
        active_connections: pool.active_connections,
    }
}

fn cluster_database_pool_to_core(
    pool: synctv_cluster::grpc::synctv::cluster::ServerStateDatabasePool,
) -> DatabasePoolStatus {
    DatabasePoolStatus {
        size: pool.size,
        idle_connections: pool.idle_connections,
        active_connections: pool.active_connections,
    }
}

fn cluster_node_to_cluster_proto(
    node: ClusterNodeStatus,
) -> synctv_cluster::grpc::synctv::cluster::ServerStateClusterNode {
    synctv_cluster::grpc::synctv::cluster::ServerStateClusterNode {
        node_id: node.node_id,
        api_address: node.api_address,
        last_heartbeat: node.last_heartbeat,
        epoch: node.epoch,
    }
}

fn cluster_proto_node_to_core(
    node: synctv_cluster::grpc::synctv::cluster::ServerStateClusterNode,
) -> ClusterNodeStatus {
    ClusterNodeStatus {
        node_id: node.node_id,
        api_address: node.api_address,
        last_heartbeat: node.last_heartbeat,
        epoch: node.epoch,
    }
}

fn node_status_to_cluster_proto(status: ServerStateNodeStatus) -> i32 {
    use synctv_cluster::grpc::synctv::cluster::ServerStateNodeStatus as ProtoStatus;

    match status {
        ServerStateNodeStatus::Healthy => ProtoStatus::Healthy,
        ServerStateNodeStatus::Degraded => ProtoStatus::Degraded,
        ServerStateNodeStatus::Unhealthy => ProtoStatus::Unhealthy,
    }
    .into()
}

fn cluster_node_status_to_core(status: i32) -> ServerStateNodeStatus {
    use synctv_cluster::grpc::synctv::cluster::ServerStateNodeStatus as ProtoStatus;

    match ProtoStatus::try_from(status).unwrap_or(ProtoStatus::Unhealthy) {
        ProtoStatus::Healthy => ServerStateNodeStatus::Healthy,
        ProtoStatus::Degraded => ServerStateNodeStatus::Degraded,
        ProtoStatus::Unhealthy | ProtoStatus::Unspecified => ServerStateNodeStatus::Unhealthy,
    }
}

fn database_status_to_cluster_proto(status: ServerStateDatabaseStatus) -> i32 {
    use synctv_cluster::grpc::synctv::cluster::ServerStateDatabaseStatus as ProtoStatus;

    match status {
        ServerStateDatabaseStatus::Healthy => ProtoStatus::Healthy,
        ServerStateDatabaseStatus::Unhealthy => ProtoStatus::Unhealthy,
    }
    .into()
}

fn cluster_database_status_to_core(status: i32) -> ServerStateDatabaseStatus {
    use synctv_cluster::grpc::synctv::cluster::ServerStateDatabaseStatus as ProtoStatus;

    match ProtoStatus::try_from(status).unwrap_or(ProtoStatus::Unhealthy) {
        ProtoStatus::Healthy => ServerStateDatabaseStatus::Healthy,
        ProtoStatus::Unhealthy | ProtoStatus::Unspecified => ServerStateDatabaseStatus::Unhealthy,
    }
}

fn redis_status_to_cluster_proto(status: ServerStateRedisStatus) -> i32 {
    use synctv_cluster::grpc::synctv::cluster::ServerStateRedisStatus as ProtoStatus;

    match status {
        ServerStateRedisStatus::Healthy => ProtoStatus::Healthy,
        ServerStateRedisStatus::NotConfigured => ProtoStatus::NotConfigured,
        ServerStateRedisStatus::Unhealthy => ProtoStatus::Unhealthy,
    }
    .into()
}

fn cluster_redis_status_to_core(status: i32) -> ServerStateRedisStatus {
    use synctv_cluster::grpc::synctv::cluster::ServerStateRedisStatus as ProtoStatus;

    match ProtoStatus::try_from(status).unwrap_or(ProtoStatus::Unhealthy) {
        ProtoStatus::Healthy => ServerStateRedisStatus::Healthy,
        ProtoStatus::NotConfigured => ServerStateRedisStatus::NotConfigured,
        ProtoStatus::Unhealthy | ProtoStatus::Unspecified => ServerStateRedisStatus::Unhealthy,
    }
}

fn cluster_resource_status_to_cluster_proto(status: ServerStateClusterStatus) -> i32 {
    use synctv_cluster::grpc::synctv::cluster::ServerStateClusterStatus as ProtoStatus;

    match status {
        ServerStateClusterStatus::Healthy => ProtoStatus::Healthy,
        ServerStateClusterStatus::Unhealthy => ProtoStatus::Unhealthy,
        ServerStateClusterStatus::Disabled => ProtoStatus::Disabled,
    }
    .into()
}

fn cluster_resource_status_to_core(status: i32) -> ServerStateClusterStatus {
    use synctv_cluster::grpc::synctv::cluster::ServerStateClusterStatus as ProtoStatus;

    match ProtoStatus::try_from(status).unwrap_or(ProtoStatus::Unhealthy) {
        ProtoStatus::Healthy => ServerStateClusterStatus::Healthy,
        ProtoStatus::Unhealthy | ProtoStatus::Unspecified => ServerStateClusterStatus::Unhealthy,
        ProtoStatus::Disabled => ServerStateClusterStatus::Disabled,
    }
}

fn ws_ticket_status_to_cluster_proto(status: ServerStateWsTicketStatus) -> i32 {
    use synctv_cluster::grpc::synctv::cluster::ServerStateWsTicketStatus as ProtoStatus;

    match status {
        ServerStateWsTicketStatus::Healthy => ProtoStatus::Healthy,
        ServerStateWsTicketStatus::Unhealthy => ProtoStatus::Unhealthy,
    }
    .into()
}

fn cluster_ws_ticket_status_to_core(status: i32) -> ServerStateWsTicketStatus {
    use synctv_cluster::grpc::synctv::cluster::ServerStateWsTicketStatus as ProtoStatus;

    match ProtoStatus::try_from(status).unwrap_or(ProtoStatus::Unhealthy) {
        ProtoStatus::Healthy => ServerStateWsTicketStatus::Healthy,
        ProtoStatus::Unhealthy | ProtoStatus::Unspecified => ServerStateWsTicketStatus::Unhealthy,
    }
}

fn email_status_to_cluster_proto(status: ServerStateEmailStatus) -> i32 {
    use synctv_cluster::grpc::synctv::cluster::ServerStateEmailStatus as ProtoStatus;

    match status {
        ServerStateEmailStatus::Configured => ProtoStatus::Configured,
        ServerStateEmailStatus::NotConfigured => ProtoStatus::NotConfigured,
    }
    .into()
}

fn cluster_email_status_to_core(status: i32) -> ServerStateEmailStatus {
    use synctv_cluster::grpc::synctv::cluster::ServerStateEmailStatus as ProtoStatus;

    match ProtoStatus::try_from(status).unwrap_or(ProtoStatus::NotConfigured) {
        ProtoStatus::Configured => ServerStateEmailStatus::Configured,
        ProtoStatus::NotConfigured | ProtoStatus::Unspecified => {
            ServerStateEmailStatus::NotConfigured
        }
    }
}

fn livestream_status_to_cluster_proto(status: ServerStateLivestreamStatus) -> i32 {
    use synctv_cluster::grpc::synctv::cluster::ServerStateLivestreamStatus as ProtoStatus;

    match status {
        ServerStateLivestreamStatus::Configured => ProtoStatus::Configured,
        ServerStateLivestreamStatus::NotConfigured => ProtoStatus::NotConfigured,
    }
    .into()
}

fn cluster_livestream_status_to_core(status: i32) -> ServerStateLivestreamStatus {
    use synctv_cluster::grpc::synctv::cluster::ServerStateLivestreamStatus as ProtoStatus;

    match ProtoStatus::try_from(status).unwrap_or(ProtoStatus::NotConfigured) {
        ProtoStatus::Configured => ServerStateLivestreamStatus::Configured,
        ProtoStatus::NotConfigured | ProtoStatus::Unspecified => {
            ServerStateLivestreamStatus::NotConfigured
        }
    }
}

fn memory_status_to_cluster_proto(status: ServerStateMemoryStatus) -> i32 {
    use synctv_cluster::grpc::synctv::cluster::ServerStateMemoryStatus as ProtoStatus;

    match status {
        ServerStateMemoryStatus::Healthy => ProtoStatus::Healthy,
        ServerStateMemoryStatus::Unhealthy => ProtoStatus::Unhealthy,
        ServerStateMemoryStatus::Unknown => ProtoStatus::Unknown,
    }
    .into()
}

fn cluster_memory_status_to_core(status: i32) -> ServerStateMemoryStatus {
    use synctv_cluster::grpc::synctv::cluster::ServerStateMemoryStatus as ProtoStatus;

    match ProtoStatus::try_from(status).unwrap_or(ProtoStatus::Unknown) {
        ProtoStatus::Healthy => ServerStateMemoryStatus::Healthy,
        ProtoStatus::Unhealthy => ServerStateMemoryStatus::Unhealthy,
        ProtoStatus::Unknown | ProtoStatus::Unspecified => ServerStateMemoryStatus::Unknown,
    }
}

fn webrtc_status_to_cluster_proto(status: ServerStateWebRtcStatus) -> i32 {
    use synctv_cluster::grpc::synctv::cluster::ServerStateWebRtcStatus as ProtoStatus;

    match status {
        ServerStateWebRtcStatus::Healthy => ProtoStatus::Healthy,
        ServerStateWebRtcStatus::Degraded => ProtoStatus::Degraded,
        ServerStateWebRtcStatus::Disabled => ProtoStatus::Disabled,
    }
    .into()
}

fn cluster_webrtc_status_to_core(status: i32) -> ServerStateWebRtcStatus {
    use synctv_cluster::grpc::synctv::cluster::ServerStateWebRtcStatus as ProtoStatus;

    match ProtoStatus::try_from(status).unwrap_or(ProtoStatus::Degraded) {
        ProtoStatus::Healthy => ServerStateWebRtcStatus::Healthy,
        ProtoStatus::Degraded | ProtoStatus::Unspecified => ServerStateWebRtcStatus::Degraded,
        ProtoStatus::Disabled => ServerStateWebRtcStatus::Disabled,
    }
}

fn cpu_status_to_cluster_proto(status: ServerStateCpuStatus) -> i32 {
    use synctv_cluster::grpc::synctv::cluster::ServerStateCpuStatus as ProtoStatus;

    match status {
        ServerStateCpuStatus::Healthy => ProtoStatus::Healthy,
        ServerStateCpuStatus::Degraded => ProtoStatus::Degraded,
        ServerStateCpuStatus::Unhealthy => ProtoStatus::Unhealthy,
        ServerStateCpuStatus::Unknown => ProtoStatus::Unknown,
    }
    .into()
}

fn cluster_cpu_status_to_core(status: i32) -> ServerStateCpuStatus {
    use synctv_cluster::grpc::synctv::cluster::ServerStateCpuStatus as ProtoStatus;

    match ProtoStatus::try_from(status).unwrap_or(ProtoStatus::Unknown) {
        ProtoStatus::Healthy => ServerStateCpuStatus::Healthy,
        ProtoStatus::Degraded => ServerStateCpuStatus::Degraded,
        ProtoStatus::Unhealthy => ServerStateCpuStatus::Unhealthy,
        ProtoStatus::Unknown | ProtoStatus::Unspecified => ServerStateCpuStatus::Unknown,
    }
}

fn slice_cache_status_to_cluster_proto(status: ServerStateSliceCacheStatus) -> i32 {
    use synctv_cluster::grpc::synctv::cluster::ServerStateSliceCacheStatus as ProtoStatus;

    match status {
        ServerStateSliceCacheStatus::Healthy => ProtoStatus::Healthy,
        ServerStateSliceCacheStatus::Disabled => ProtoStatus::Disabled,
    }
    .into()
}

fn cluster_slice_cache_status_to_core(status: i32) -> ServerStateSliceCacheStatus {
    use synctv_cluster::grpc::synctv::cluster::ServerStateSliceCacheStatus as ProtoStatus;

    match ProtoStatus::try_from(status).unwrap_or(ProtoStatus::Disabled) {
        ProtoStatus::Healthy => ServerStateSliceCacheStatus::Healthy,
        ProtoStatus::Disabled | ProtoStatus::Unspecified => ServerStateSliceCacheStatus::Disabled,
    }
}

fn non_empty_string(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn saturating_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}
