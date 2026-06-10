mod proto {
    tonic::include_proto!("synctv.proxy.slice_cache");
}

mod slice_cache_service;

pub use proto::proxy_slice_cache_service_client::ProxySliceCacheServiceClient;
pub use proto::proxy_slice_cache_service_server::{
    ProxySliceCacheService, ProxySliceCacheServiceServer,
};
pub use proto::{
    EvictExpiredSliceCacheRequest, EvictExpiredSliceCacheResponse, GetSliceCacheStatsRequest,
    PurgeSliceCacheRequest, PurgeSliceCacheResponse, SliceCacheConfigInfo, SliceCacheStatsResponse,
};
pub use slice_cache_service::ProxySliceCacheServiceImpl;
