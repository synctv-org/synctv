// gRPC services for stream relay

pub(crate) mod proto {
    tonic::include_proto!("synctv.stream");
}

mod connection_pool;
mod hls_proxy;
pub(crate) mod stream_puller;
mod stream_relay_service;

pub(crate) use connection_pool::GrpcConnectionPool;

pub(crate) use hls_proxy::HlsProxyClient;
pub use proto::stream_relay_service_server::StreamRelayServiceServer;
pub use stream_relay_service::StreamRelayServiceImpl;
