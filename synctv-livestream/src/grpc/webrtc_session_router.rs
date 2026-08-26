use std::time::Duration;

use tonic::{codec::CompressionEncoding, metadata::MetadataValue, Request};

use super::{
    connection_pool::GrpcConnectionPool,
    proto::{
        stream_relay_service_client::StreamRelayServiceClient, DeleteWebRtcSessionRequest,
        WebRtcSessionKind as ProtoWebRtcSessionKind,
    },
};
use crate::{
    error::{StreamError, StreamResult},
    relay::{WebRtcSessionKind, WebRtcSessionOwner},
    util::{
        validate_publisher_cluster_address, validate_stream_generation_id, validate_stream_ids,
    },
};

#[derive(Clone)]
pub(crate) struct WebRtcSessionRouter {
    cluster_secret: Option<String>,
    grpc_max_message_size_bytes: usize,
    grpc_compression_enabled: bool,
    connection_pool: GrpcConnectionPool,
}

impl WebRtcSessionRouter {
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

    #[must_use]
    pub(crate) fn with_defaults(cluster_secret: Option<String>) -> Self {
        Self {
            cluster_secret,
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            grpc_compression_enabled: true,
            connection_pool: GrpcConnectionPool::with_defaults(),
        }
    }

    #[must_use]
    pub(crate) const fn with_grpc_max_message_size(mut self, bytes: usize) -> Self {
        self.grpc_max_message_size_bytes = bytes;
        self
    }

    #[must_use]
    pub(crate) const fn with_grpc_compression(mut self, enabled: bool) -> Self {
        self.grpc_compression_enabled = enabled;
        self
    }

    #[must_use]
    pub(crate) fn with_connection_pool(mut self, pool: GrpcConnectionPool) -> Self {
        self.connection_pool = pool;
        self
    }

    fn cluster_secret_metadata(&self) -> StreamResult<MetadataValue<tonic::metadata::Ascii>> {
        let secret = self
            .cluster_secret
            .as_deref()
            .filter(|secret| !secret.is_empty())
            .ok_or_else(|| {
                StreamError::InvalidState(
                    "cluster authentication secret is not configured".to_string(),
                )
            })?;
        secret.parse().map_err(|error| {
            StreamError::InvalidState(format!("invalid cluster authentication secret: {error}"))
        })
    }

    fn map_remote_status(status: &tonic::Status) -> StreamError {
        let message = status.message().to_string();
        match status.code() {
            tonic::Code::InvalidArgument | tonic::Code::OutOfRange => {
                StreamError::InvalidInput(message)
            }
            tonic::Code::PermissionDenied => StreamError::PermissionDenied(message),
            tonic::Code::FailedPrecondition | tonic::Code::AlreadyExists | tonic::Code::Aborted => {
                StreamError::InvalidState(message)
            }
            tonic::Code::ResourceExhausted => StreamError::ResourceExhausted(message),
            tonic::Code::NotFound => StreamError::StreamNotFound(message),
            tonic::Code::Internal | tonic::Code::DataLoss | tonic::Code::Unknown => {
                StreamError::Internal(message)
            }
            tonic::Code::Cancelled
            | tonic::Code::DeadlineExceeded
            | tonic::Code::Unauthenticated
            | tonic::Code::Unimplemented
            | tonic::Code::Unavailable => StreamError::ConnectionFailed(message),
            tonic::Code::Ok => StreamError::Internal(
                "remote WebRTC deletion returned an invalid status".to_string(),
            ),
        }
    }

    pub(crate) async fn delete_session(
        &self,
        session_id: &str,
        owner: &WebRtcSessionOwner,
        publish_token: Option<&str>,
    ) -> StreamResult<bool> {
        validate_stream_generation_id(session_id)
            .map_err(|error| StreamError::InvalidInput(error.to_string()))?;
        validate_stream_ids(&owner.room_id, &owner.media_id)
            .map_err(|error| StreamError::InvalidInput(error.to_string()))?;
        validate_publisher_cluster_address(
            &owner.cluster_address,
            &owner.node_id,
            &owner.room_id,
            &owner.media_id,
        )
        .map_err(|error| StreamError::InvalidAddress(error.to_string()))?;
        let cluster_secret = self.cluster_secret_metadata()?;
        let channel = self
            .connection_pool
            .get_channel(&owner.cluster_address)
            .await
            .map_err(|error| {
                StreamError::ConnectionFailed(format!(
                    "failed to connect to WebRTC session owner: {error}"
                ))
            })?;
        let client = StreamRelayServiceClient::new(channel)
            .max_decoding_message_size(self.grpc_max_message_size_bytes)
            .max_encoding_message_size(self.grpc_max_message_size_bytes);
        let mut client = if self.grpc_compression_enabled {
            client
                .accept_compressed(CompressionEncoding::Gzip)
                .send_compressed(CompressionEncoding::Gzip)
        } else {
            client
        };
        let kind = match owner.kind {
            WebRtcSessionKind::Whip => ProtoWebRtcSessionKind::Whip,
            WebRtcSessionKind::Whep => ProtoWebRtcSessionKind::Whep,
        };
        let mut request = Request::new(DeleteWebRtcSessionRequest {
            session_id: session_id.to_string(),
            room_id: owner.room_id.clone(),
            media_id: owner.media_id.clone(),
            kind: kind as i32,
            publish_token: publish_token.unwrap_or_default().to_string(),
        });
        request
            .metadata_mut()
            .insert("x-cluster-secret", cluster_secret);

        let result = tokio::time::timeout(
            Self::REQUEST_TIMEOUT,
            client.delete_web_rtc_session(request),
        )
        .await
        .map_err(|_| {
            StreamError::ConnectionFailed("timed out deleting remote WebRTC session".to_string())
        })?;
        match result {
            Ok(response) => Ok(response.into_inner().deleted),
            Err(status) => {
                self.connection_pool
                    .invalidate(&owner.cluster_address)
                    .await;
                Err(Self::map_remote_status(&status))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{pin::Pin, sync::Arc};

    use futures::Stream;
    use tokio::net::TcpListener;
    use tokio_stream::wrappers::TcpListenerStream;
    use tokio_util::sync::CancellationToken;
    use tonic::{codec::CompressionEncoding, transport::Server, Response, Status};

    use super::*;
    use crate::grpc::proto::{
        stream_relay_service_server::{StreamRelayService, StreamRelayServiceServer},
        DeleteWebRtcSessionResponse, GetHlsPlaylistRequest, GetHlsPlaylistResponse,
        GetHlsSegmentRequest, GetHlsSegmentResponse, PullRtmpStreamRequest, RtmpPacket, RtpPacket,
    };

    type RtmpStream =
        Pin<Box<dyn Stream<Item = Result<RtmpPacket, Status>> + Send + Sync + 'static>>;
    type RtpStream = Pin<Box<dyn Stream<Item = Result<RtpPacket, Status>> + Send + Sync + 'static>>;

    #[derive(Clone)]
    struct RecordingRelayService {
        requests: tokio::sync::mpsc::UnboundedSender<DeleteWebRtcSessionRequest>,
        expected_secret: Arc<str>,
    }

    #[tonic::async_trait]
    impl StreamRelayService for RecordingRelayService {
        type PullRtmpStreamStream = RtmpStream;

        async fn pull_rtmp_stream(
            &self,
            _request: Request<PullRtmpStreamRequest>,
        ) -> Result<Response<Self::PullRtmpStreamStream>, Status> {
            Err(Status::unimplemented("outside this test"))
        }

        type PullRtpStreamStream = RtpStream;

        async fn pull_rtp_stream(
            &self,
            _request: Request<PullRtmpStreamRequest>,
        ) -> Result<Response<Self::PullRtpStreamStream>, Status> {
            Err(Status::unimplemented("outside this test"))
        }

        async fn get_hls_playlist(
            &self,
            _request: Request<GetHlsPlaylistRequest>,
        ) -> Result<Response<GetHlsPlaylistResponse>, Status> {
            Err(Status::unimplemented("outside this test"))
        }

        async fn get_hls_segment(
            &self,
            _request: Request<GetHlsSegmentRequest>,
        ) -> Result<Response<GetHlsSegmentResponse>, Status> {
            Err(Status::unimplemented("outside this test"))
        }

        async fn delete_web_rtc_session(
            &self,
            request: Request<DeleteWebRtcSessionRequest>,
        ) -> Result<Response<DeleteWebRtcSessionResponse>, Status> {
            let supplied_secret = request
                .metadata()
                .get("x-cluster-secret")
                .and_then(|value| value.to_str().ok());
            if supplied_secret != Some(self.expected_secret.as_ref()) {
                return Err(Status::unauthenticated("invalid cluster secret"));
            }
            let request = request.into_inner();
            if request.publish_token == "denied" {
                return Err(Status::permission_denied(
                    "WHIP session credentials do not match",
                ));
            }
            self.requests
                .send(request)
                .map_err(|_| Status::internal("request observer closed"))?;
            Ok(Response::new(DeleteWebRtcSessionResponse { deleted: true }))
        }
    }

    fn owner(address: String, kind: WebRtcSessionKind) -> WebRtcSessionOwner {
        WebRtcSessionOwner {
            node_id: "node-owner".to_string(),
            cluster_address: address,
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            kind,
        }
    }

    #[tokio::test]
    async fn router_forwards_whip_and_whep_deletes_with_cluster_auth() -> anyhow::Result<()> {
        const WHIP_SESSION_ID: &str = "00000000-0000-4000-8000-000000000001";
        const WHEP_SESSION_ID: &str = "00000000-0000-4000-8000-000000000002";
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?.to_string();
        let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
        let service = StreamRelayServiceServer::new(RecordingRelayService {
            requests: request_tx,
            expected_secret: Arc::from("cluster-secret"),
        })
        .accept_compressed(CompressionEncoding::Gzip)
        .send_compressed(CompressionEncoding::Gzip);
        let cancel = CancellationToken::new();
        let server_cancel = cancel.clone();
        let server = tokio::spawn(async move {
            Server::builder()
                .add_service(service)
                .serve_with_incoming_shutdown(
                    TcpListenerStream::new(listener),
                    server_cancel.cancelled_owned(),
                )
                .await
        });
        let router = WebRtcSessionRouter::with_defaults(Some("cluster-secret".to_string()));

        assert!(
            router
                .delete_session(
                    WHIP_SESSION_ID,
                    &owner(address.clone(), WebRtcSessionKind::Whip),
                    Some("publish-token"),
                )
                .await?
        );
        let whip = tokio::time::timeout(Duration::from_secs(1), request_rx.recv())
            .await?
            .ok_or_else(|| anyhow::anyhow!("WHIP request was not observed"))?;
        assert_eq!(whip.session_id, WHIP_SESSION_ID);
        assert_eq!(whip.room_id, "room1");
        assert_eq!(whip.media_id, "media1");
        assert_eq!(whip.kind, ProtoWebRtcSessionKind::Whip as i32);
        assert_eq!(whip.publish_token, "publish-token");

        assert!(
            router
                .delete_session(
                    WHEP_SESSION_ID,
                    &owner(address.clone(), WebRtcSessionKind::Whep),
                    None,
                )
                .await?
        );
        let whep = tokio::time::timeout(Duration::from_secs(1), request_rx.recv())
            .await?
            .ok_or_else(|| anyhow::anyhow!("WHEP request was not observed"))?;
        assert_eq!(whep.kind, ProtoWebRtcSessionKind::Whep as i32);
        assert!(whep.publish_token.is_empty());

        let denied = router
            .delete_session(
                WHIP_SESSION_ID,
                &owner(address, WebRtcSessionKind::Whip),
                Some("denied"),
            )
            .await;
        assert!(matches!(denied, Err(StreamError::PermissionDenied(_))));

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), server).await???;
        Ok(())
    }

    #[tokio::test]
    async fn router_requires_a_cluster_secret_before_connecting() {
        let router = WebRtcSessionRouter::with_defaults(None);
        let error = router
            .delete_session(
                "00000000-0000-4000-8000-000000000001",
                &owner("127.0.0.1:1".to_string(), WebRtcSessionKind::Whep),
                None,
            )
            .await
            .expect_err("remote deletion without cluster auth must fail");
        assert!(error.to_string().contains("secret is not configured"));
    }
}
