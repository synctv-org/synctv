use std::sync::Arc;

use subtle::ConstantTimeEq;
use tonic::{Request, Response, Status};

use super::proto::{
    proxy_slice_cache_service_server, EvictExpiredSliceCacheRequest,
    EvictExpiredSliceCacheResponse, GetSliceCacheStatsRequest, PurgeSliceCacheRequest,
    PurgeSliceCacheResponse, SliceCacheConfigInfo, SliceCacheStatsResponse,
};
use crate::slice_cache::SliceCache;

const AUTH_SECRET_METADATA_KEY: &str = "x-cluster-secret";

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.ct_eq(right).into()
}

#[derive(Clone)]
pub struct ProxySliceCacheServiceImpl {
    cache: Arc<SliceCache>,
    node_id: String,
    cluster_secret: Option<Arc<String>>,
}

impl ProxySliceCacheServiceImpl {
    #[must_use]
    pub fn new(cache: Arc<SliceCache>, node_id: String) -> Self {
        Self {
            cache,
            node_id,
            cluster_secret: None,
        }
    }

    #[must_use]
    pub fn with_cluster_secret(mut self, secret: String) -> Self {
        self.cluster_secret = Some(Arc::new(secret));
        self
    }

    #[allow(clippy::result_large_err)]
    fn authenticate<T>(&self, request: &Request<T>) -> Result<(), Status> {
        let Some(expected) = &self.cluster_secret else {
            return Err(Status::unauthenticated(
                "cluster authentication secret is not configured",
            ));
        };
        if expected.is_empty() {
            return Err(Status::unauthenticated(
                "cluster authentication secret is not configured",
            ));
        }

        let provided = request
            .metadata()
            .get(AUTH_SECRET_METADATA_KEY)
            .ok_or_else(|| Status::unauthenticated("missing cluster authentication secret"))?
            .as_bytes();

        if constant_time_eq(provided, expected.as_bytes()) {
            Ok(())
        } else {
            Err(Status::unauthenticated(
                "invalid cluster authentication secret",
            ))
        }
    }

    fn stats_response(&self) -> SliceCacheStatsResponse {
        let stats = self.cache.stats();
        SliceCacheStatsResponse {
            node_id: self.node_id.clone(),
            config: Some(SliceCacheConfigInfo {
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
            }),
            current_size_bytes: stats.current_size_bytes,
            entry_count: stats.entry_count,
            metadata_entries: stats.metadata_entries,
            updating_entries: stats.updating_entries,
            lock_count: stats.lock_count,
            usage_ratio: stats.usage_ratio,
        }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl proxy_slice_cache_service_server::ProxySliceCacheService for ProxySliceCacheServiceImpl {
    async fn get_slice_cache_stats(
        &self,
        request: Request<GetSliceCacheStatsRequest>,
    ) -> Result<Response<SliceCacheStatsResponse>, Status> {
        self.authenticate(&request)?;
        Ok(Response::new(self.stats_response()))
    }

    async fn purge_slice_cache(
        &self,
        request: Request<PurgeSliceCacheRequest>,
    ) -> Result<Response<PurgeSliceCacheResponse>, Status> {
        self.authenticate(&request)?;
        let result = self.cache.purge_all().await;
        Ok(Response::new(PurgeSliceCacheResponse {
            node_id: self.node_id.clone(),
            success: true,
            removed_entries: result.removed_entries,
            freed_bytes: result.freed_bytes,
            stats: Some(self.stats_response()),
        }))
    }

    async fn evict_expired_slice_cache(
        &self,
        request: Request<EvictExpiredSliceCacheRequest>,
    ) -> Result<Response<EvictExpiredSliceCacheResponse>, Status> {
        self.authenticate(&request)?;
        let removed_expired_entries = self.cache.evict_expired_entries().await;
        Ok(Response::new(EvictExpiredSliceCacheResponse {
            node_id: self.node_id.clone(),
            success: true,
            removed_expired_entries,
            stats: Some(self.stats_response()),
        }))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::grpc::proto::proxy_slice_cache_service_server::ProxySliceCacheService;
    use crate::slice_cache::SliceCacheConfig;

    fn request_with_secret<T>(value: T) -> Request<T> {
        let mut request = Request::new(value);
        request
            .metadata_mut()
            .insert(AUTH_SECRET_METADATA_KEY, "cluster-secret".parse().unwrap());
        request
    }

    #[tokio::test]
    async fn stats_requires_cluster_secret() {
        let service = ProxySliceCacheServiceImpl::new(
            Arc::new(SliceCache::new(SliceCacheConfig::default())),
            "node-a".to_string(),
        )
        .with_cluster_secret("cluster-secret".to_string());

        let error = service
            .get_slice_cache_stats(Request::new(GetSliceCacheStatsRequest {}))
            .await
            .expect_err("missing secret must be rejected");

        assert_eq!(error.code(), tonic::Code::Unauthenticated);
    }

    #[tokio::test]
    async fn stats_returns_node_cache_snapshot() {
        let service = ProxySliceCacheServiceImpl::new(
            Arc::new(SliceCache::new(SliceCacheConfig::default())),
            "node-a".to_string(),
        )
        .with_cluster_secret("cluster-secret".to_string());

        let response = service
            .get_slice_cache_stats(request_with_secret(GetSliceCacheStatsRequest {}))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(response.node_id, "node-a");
        assert!(response.config.is_some());
    }
}
