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
        Arc::new(SliceCache::new(SliceCacheConfig::default()).expect("test cache should build")),
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
        Arc::new(SliceCache::new(SliceCacheConfig::default()).expect("test cache should build")),
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
