// gRPC services for stream relay

pub mod proto {
    tonic::include_proto!("synctv.stream");
}

mod connection_pool;
mod stream_relay_service;
mod stream_puller;
mod hls_proxy;

pub use connection_pool::GrpcConnectionPool;

pub use stream_relay_service::StreamRelayServiceImpl;
pub use stream_puller::GrpcStreamPuller;
pub use hls_proxy::HlsProxyClient;
pub use proto::stream_relay_service_server::{StreamRelayService, StreamRelayServiceServer};
pub use proto::stream_relay_service_client::StreamRelayServiceClient;
// Export proto message types
pub use proto::{RtmpPacket, PullRtmpStreamRequest, FrameType};
