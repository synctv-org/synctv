// gRPC services for stream relay

pub mod proto {
    tonic::include_proto!("synctv.stream");
}

mod connection_pool;
mod hls_proxy;
mod stream_puller;
mod stream_relay_service;

pub use connection_pool::GrpcConnectionPool;

pub use hls_proxy::HlsProxyClient;
pub use proto::stream_relay_service_client::StreamRelayServiceClient;
pub use proto::stream_relay_service_server::{StreamRelayService, StreamRelayServiceServer};
pub use stream_puller::GrpcStreamPuller;
pub use stream_relay_service::{RelayActivityCallback, StreamRelayServiceImpl};
// Export proto message types
pub use proto::{FrameType, PullRtmpStreamRequest, RtmpPacket};
