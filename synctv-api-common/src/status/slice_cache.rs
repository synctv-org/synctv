use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tonic::codec::CompressionEncoding;
use tonic::transport::{Channel, Endpoint};
use tonic::Request;

use crate::http_error::AppError;

use super::{cluster_node_to_target, ApiServerStateClusterRuntime};
use synctv_core::service::{
    ServerStateClusterTarget, SliceCacheConfigInfo, SliceCacheEvictExpiredNodeResult,
    SliceCacheManagementClusterRuntime, SliceCacheManagementError,
    SliceCacheManagementLocalRuntime, SliceCacheManagementRemoteClient, SliceCacheManagementResult,
    SliceCacheManagementService as SliceCacheManagementRuntime,
    SliceCacheManagementServiceDependencies, SliceCachePurgeNodeResult, SliceCachePurgeResult,
    SliceCacheStats as SliceCacheManagementStats, SliceCacheStatsNode,
};

const SLICE_CACHE_REMOTE_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const SLICE_CACHE_REMOTE_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

type ProxySliceCacheClient =
    synctv_proxy::grpc::ProxySliceCacheServiceClient<tonic::transport::Channel>;

impl From<SliceCacheManagementError> for AppError {
    fn from(error: SliceCacheManagementError) -> Self {
        match error {
            SliceCacheManagementError::InvalidSelection => AppError::bad_request(error.to_string()),
            SliceCacheManagementError::ClusterUnavailable(_)
            | SliceCacheManagementError::Cluster(_)
            | SliceCacheManagementError::MissingClusterSecret
            | SliceCacheManagementError::InvalidClusterSecret
            | SliceCacheManagementError::RemoteRequest { .. } => AppError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                error.to_string(),
            ),
        }
    }
}

#[must_use]
pub fn slice_cache_management_runtime_from_router_options(
    config: &crate::app_state::RouterOptions,
) -> SliceCacheManagementRuntime {
    let cluster_runtime = config.cluster_client.as_ref().map(|client| {
        Arc::new(ApiServerStateClusterRuntime {
            client: client.clone(),
        }) as Arc<dyn SliceCacheManagementClusterRuntime>
    });
    let remote_client = config.cluster_client.as_ref().map(|_| {
        Arc::new(ApiSliceCacheManagementRemoteClient {
            runtime_settings: config.runtime_settings.clone(),
        }) as Arc<dyn SliceCacheManagementRemoteClient>
    });

    SliceCacheManagementRuntime::new(SliceCacheManagementServiceDependencies {
        node_id: config.event_service.node_id().to_string(),
        local_runtime: Arc::new(ApiSliceCacheManagementLocalRuntime {
            cache: config.proxy_slice_cache.clone(),
        }),
        cluster_runtime,
        remote_client,
    })
}

#[async_trait]
impl SliceCacheManagementClusterRuntime for ApiServerStateClusterRuntime {
    async fn resolve_routable_node(
        &self,
        target_node_id: &str,
    ) -> SliceCacheManagementResult<ServerStateClusterTarget> {
        let node = self
            .client
            .resolve_routable_node(target_node_id)
            .await
            .map_err(|error| SliceCacheManagementError::Cluster(error.to_string()))?;
        Ok(cluster_node_to_target(node))
    }

    async fn remote_routable_nodes(
        &self,
    ) -> SliceCacheManagementResult<Vec<ServerStateClusterTarget>> {
        let nodes = self
            .client
            .remote_routable_nodes()
            .await
            .map_err(|error| SliceCacheManagementError::Cluster(error.to_string()))?;
        Ok(nodes.into_iter().map(cluster_node_to_target).collect())
    }
}

struct ApiSliceCacheManagementLocalRuntime {
    cache: Arc<synctv_proxy::slice_cache::SliceCache>,
}

#[async_trait]
impl SliceCacheManagementLocalRuntime for ApiSliceCacheManagementLocalRuntime {
    fn stats(&self) -> SliceCacheManagementStats {
        slice_cache_stats_from_proxy(self.cache.stats())
    }

    async fn purge_all(&self) -> SliceCachePurgeResult {
        let result = self.cache.purge_all().await;
        SliceCachePurgeResult {
            removed_entries: result.removed_entries,
            freed_bytes: result.freed_bytes,
        }
    }

    async fn evict_expired_entries(&self) -> u64 {
        self.cache.evict_expired_entries().await
    }
}

struct ApiSliceCacheManagementRemoteClient {
    runtime_settings: Arc<crate::ApiRuntimeSettings>,
}

#[async_trait]
impl SliceCacheManagementRemoteClient for ApiSliceCacheManagementRemoteClient {
    async fn remote_stats(
        &self,
        node: &ServerStateClusterTarget,
    ) -> SliceCacheManagementResult<SliceCacheStatsNode> {
        let mut request = Request::new(synctv_proxy::grpc::GetSliceCacheStatsRequest {});
        attach_slice_cache_cluster_secret(&mut request, &self.runtime_settings.cluster.secret)?;
        let mut client = self.proxy_slice_cache_client(&node.api_address).await?;
        client
            .get_slice_cache_stats(request)
            .await
            .map(|response| proxy_slice_cache_stats_to_api(response.into_inner()))
            .map_err(|error| SliceCacheManagementError::RemoteRequest {
                node_id: node.node_id.clone(),
                error: error.to_string(),
            })
    }

    async fn remote_purge(
        &self,
        node: &ServerStateClusterTarget,
    ) -> SliceCacheManagementResult<SliceCachePurgeNodeResult> {
        let mut request = Request::new(synctv_proxy::grpc::PurgeSliceCacheRequest {});
        attach_slice_cache_cluster_secret(&mut request, &self.runtime_settings.cluster.secret)?;
        let mut client = self.proxy_slice_cache_client(&node.api_address).await?;
        client
            .purge_slice_cache(request)
            .await
            .map(|response| proxy_purge_to_api(response.into_inner()))
            .map_err(|error| SliceCacheManagementError::RemoteRequest {
                node_id: node.node_id.clone(),
                error: error.to_string(),
            })
    }

    async fn remote_evict_expired(
        &self,
        node: &ServerStateClusterTarget,
    ) -> SliceCacheManagementResult<SliceCacheEvictExpiredNodeResult> {
        let mut request = Request::new(synctv_proxy::grpc::EvictExpiredSliceCacheRequest {});
        attach_slice_cache_cluster_secret(&mut request, &self.runtime_settings.cluster.secret)?;
        let mut client = self.proxy_slice_cache_client(&node.api_address).await?;
        client
            .evict_expired_slice_cache(request)
            .await
            .map(|response| proxy_evict_expired_to_api(response.into_inner()))
            .map_err(|error| SliceCacheManagementError::RemoteRequest {
                node_id: node.node_id.clone(),
                error: error.to_string(),
            })
    }
}

impl ApiSliceCacheManagementRemoteClient {
    async fn proxy_slice_cache_client(
        &self,
        address: &str,
    ) -> SliceCacheManagementResult<ProxySliceCacheClient> {
        let endpoint = Endpoint::from_shared(slice_cache_uri(address))
            .map_err(|error| {
                SliceCacheManagementError::Cluster(format!("invalid node address: {error}"))
            })?
            .connect_timeout(SLICE_CACHE_REMOTE_CONNECT_TIMEOUT)
            .timeout(SLICE_CACHE_REMOTE_REQUEST_TIMEOUT);
        let channel: Channel = endpoint.connect().await.map_err(|error| {
            SliceCacheManagementError::Cluster(format!("failed to connect to {address}: {error}"))
        })?;
        let client = synctv_proxy::grpc::ProxySliceCacheServiceClient::new(channel)
            .max_decoding_message_size(self.runtime_settings.server.grpc_max_message_size_bytes)
            .max_encoding_message_size(self.runtime_settings.server.grpc_max_message_size_bytes);
        let client = if self.runtime_settings.server.grpc_compression_enabled {
            client
                .accept_compressed(CompressionEncoding::Gzip)
                .send_compressed(CompressionEncoding::Gzip)
        } else {
            client
        };
        Ok(client)
    }
}

fn proxy_slice_cache_stats_to_api(
    stats: synctv_proxy::grpc::SliceCacheStatsResponse,
) -> SliceCacheStatsNode {
    let config = stats.config.unwrap_or_default();
    SliceCacheStatsNode {
        node_id: stats.node_id,
        config: SliceCacheConfigInfo {
            engine_enabled: config.engine_enabled,
            backend: config.backend,
            file_cache_dir: config.file_cache_dir,
            slice_size: config.slice_size,
            max_cache_size: config.max_cache_size,
            segment_ttl_secs: config.segment_ttl_secs,
            stale_max_age_secs: config.stale_max_age_secs,
            stale_while_revalidate: config.stale_while_revalidate,
            eviction_interval_secs: config.eviction_interval_secs,
            watermark_ratio: config.watermark_ratio,
        },
        current_size_bytes: stats.current_size_bytes,
        entry_count: stats.entry_count,
        metadata_entries: stats.metadata_entries,
        updating_entries: stats.updating_entries,
        lock_count: stats.lock_count,
        usage_ratio: stats.usage_ratio,
    }
}

fn proxy_purge_to_api(
    response: synctv_proxy::grpc::PurgeSliceCacheResponse,
) -> SliceCachePurgeNodeResult {
    SliceCachePurgeNodeResult {
        node_id: response.node_id,
        success: response.success,
        removed_entries: response.removed_entries,
        freed_bytes: response.freed_bytes,
        stats: response.stats.map(proxy_slice_cache_stats_to_api),
    }
}

fn proxy_evict_expired_to_api(
    response: synctv_proxy::grpc::EvictExpiredSliceCacheResponse,
) -> SliceCacheEvictExpiredNodeResult {
    SliceCacheEvictExpiredNodeResult {
        node_id: response.node_id,
        success: response.success,
        removed_expired_entries: response.removed_expired_entries,
        stats: response.stats.map(proxy_slice_cache_stats_to_api),
    }
}

fn slice_cache_stats_from_proxy(
    stats: synctv_proxy::slice_cache::SliceCacheStats,
) -> SliceCacheManagementStats {
    SliceCacheManagementStats {
        config: SliceCacheConfigInfo {
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
        },
        current_size_bytes: stats.current_size_bytes,
        entry_count: stats.entry_count,
        metadata_entries: stats.metadata_entries,
        updating_entries: stats.updating_entries,
        lock_count: stats.lock_count,
        usage_ratio: stats.usage_ratio,
    }
}

fn slice_cache_uri(address: &str) -> String {
    if address.starts_with("http://") || address.starts_with("https://") {
        address.to_string()
    } else {
        format!("http://{address}")
    }
}

fn attach_slice_cache_cluster_secret<T>(
    request: &mut Request<T>,
    secret: &str,
) -> SliceCacheManagementResult<()> {
    if secret.is_empty() {
        return Err(SliceCacheManagementError::MissingClusterSecret);
    }
    synctv_cluster::grpc::attach_cluster_secret(request, secret)
        .map_err(|_| SliceCacheManagementError::InvalidClusterSecret)?;
    Ok(())
}
