use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tonic::codec::CompressionEncoding;
use tonic::transport::{Channel, Endpoint};
use tonic::Request;

use crate::http::AppError;
use synctv_realtime::fanout::RealtimeEventService;
mod server_state_cluster;
mod slice_cache;
use server_state_cluster::cluster_proto_to_server_state_node;
pub use server_state_cluster::ServerStateGrpcService;
pub use slice_cache::slice_cache_management_runtime_from_router_options;

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
    SliceCacheConfigInfo, SliceCacheEvictExpiredNodeResult, SliceCacheEvictExpiredResponse,
    SliceCacheManagementClusterRuntime, SliceCacheManagementError,
    SliceCacheManagementLocalRuntime, SliceCacheManagementRemoteClient, SliceCacheManagementResult,
    SliceCacheManagementService as SliceCacheManagementRuntime,
    SliceCacheManagementServiceDependencies, SliceCacheNodeFailure, SliceCachePurgeNodeResult,
    SliceCachePurgeResponse, SliceCachePurgeResult, SliceCacheSelection,
    SliceCacheStats as SliceCacheManagementStats, SliceCacheStatsNode, SliceCacheStatsResponse,
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
pub fn server_state_runtime_from_router_options(
    config: &crate::http::RouterOptions,
) -> ServerStateRuntime {
    let cluster_runtime = config.cluster_client.as_ref().map(|client| {
        Arc::new(ApiServerStateClusterRuntime {
            client: client.clone(),
        }) as Arc<dyn ServerStateClusterRuntime>
    });
    let remote_client = config.cluster_client.as_ref().map(|_| {
        Arc::new(ApiServerStateRemoteClient {
            runtime_settings: config.runtime_settings.clone(),
        }) as Arc<dyn ServerStateRemoteClient>
    });

    ServerStateRuntime::new(ServerStateServiceDependencies {
        runtime_params: Arc::new(config.runtime_settings.server_state.clone()),
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
    runtime_settings: Arc<crate::ApiRuntimeSettings>,
}

#[async_trait]
impl ServerStateRemoteClient for ApiServerStateRemoteClient {
    async fn remote_node_server_state(
        &self,
        node: &ServerStateClusterTarget,
    ) -> ServerStateResult<ServerStateNode> {
        let mut request =
            Request::new(synctv_cluster::grpc::synctv::cluster::GetServerStateRequest {});
        attach_cluster_secret(&mut request, &self.runtime_settings.cluster.secret)?;
        let mut client = server_state_client(&self.runtime_settings, &node.api_address).await?;
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
    runtime_settings: &crate::ApiRuntimeSettings,
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
        .max_decoding_message_size(runtime_settings.server.grpc_max_message_size_bytes)
        .max_encoding_message_size(runtime_settings.server.grpc_max_message_size_bytes);
    let client = if runtime_settings.server.grpc_compression_enabled {
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

fn saturating_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}
