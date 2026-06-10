use super::*;
use crate::grpc::ProxySliceCacheService;
use crate::slice_cache::SliceCacheConfig;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn request_with_secret<T>(
    value: T,
) -> Result<Request<T>, tonic::metadata::errors::InvalidMetadataValue> {
    let mut request = Request::new(value);
    request
        .metadata_mut()
        .insert(AUTH_SECRET_METADATA_KEY, "cluster-secret".parse()?);
    Ok(request)
}

fn service() -> anyhow::Result<ProxySliceCacheServiceImpl> {
    Ok(ProxySliceCacheServiceImpl::new(
        Arc::new(SliceCache::new(SliceCacheConfig::default())?),
        "node-a".to_string(),
    )
    .with_cluster_secret("cluster-secret".to_string()))
}

#[tokio::test]
async fn stats_requires_cluster_secret() -> TestResult {
    let service = service()?;

    let error = service
        .get_slice_cache_stats(Request::new(GetSliceCacheStatsRequest {}))
        .await
        .expect_err("missing secret must be rejected");

    assert_eq!(error.code(), tonic::Code::Unauthenticated);
    Ok(())
}

#[tokio::test]
async fn stats_returns_node_cache_snapshot() -> TestResult {
    let service = service()?;

    let response = service
        .get_slice_cache_stats(request_with_secret(GetSliceCacheStatsRequest {})?)
        .await?
        .into_inner();

    assert_eq!(response.node_id, "node-a");
    assert!(response.config.is_some());
    Ok(())
}
