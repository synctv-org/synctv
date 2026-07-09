use crate::grpc as upstream_transport;
use tonic::body::Body as TransportBody;
use tonic::client::GrpcService as TransportService;
use tonic::codec::CompressionEncoding as TransportCompression;
use tonic::codegen::{
    Body as TransportResponseBody, Bytes as TransportBytes, StdError as TransportStdError,
};

pub(crate) fn apply_provider_client_compression<T>(
    client: T,
    transport_compression_enabled: bool,
) -> T
where
    T: ProviderTransportClientCompression,
{
    if transport_compression_enabled {
        client
            .accept_provider_compression(TransportCompression::Gzip)
            .send_provider_compression(TransportCompression::Gzip)
    } else {
        client
    }
}

pub(crate) trait ProviderTransportClientCompression: Sized {
    fn accept_provider_compression(self, encoding: TransportCompression) -> Self;
    fn send_provider_compression(self, encoding: TransportCompression) -> Self;
}

impl<T> ProviderTransportClientCompression
    for upstream_transport::alist::alist_client::AlistClient<T>
where
    T: TransportService<TransportBody>,
    T::ResponseBody: TransportResponseBody<Data = TransportBytes> + Send + 'static,
    <T::ResponseBody as TransportResponseBody>::Error: Into<TransportStdError> + Send,
{
    fn accept_provider_compression(self, encoding: TransportCompression) -> Self {
        self.accept_compressed(encoding)
    }

    fn send_provider_compression(self, encoding: TransportCompression) -> Self {
        self.send_compressed(encoding)
    }
}

impl<T> ProviderTransportClientCompression
    for upstream_transport::bilibili::bilibili_client::BilibiliClient<T>
where
    T: TransportService<TransportBody>,
    T::ResponseBody: TransportResponseBody<Data = TransportBytes> + Send + 'static,
    <T::ResponseBody as TransportResponseBody>::Error: Into<TransportStdError> + Send,
{
    fn accept_provider_compression(self, encoding: TransportCompression) -> Self {
        self.accept_compressed(encoding)
    }

    fn send_provider_compression(self, encoding: TransportCompression) -> Self {
        self.send_compressed(encoding)
    }
}

impl<T> ProviderTransportClientCompression for upstream_transport::emby::emby_client::EmbyClient<T>
where
    T: TransportService<TransportBody>,
    T::ResponseBody: TransportResponseBody<Data = TransportBytes> + Send + 'static,
    <T::ResponseBody as TransportResponseBody>::Error: Into<TransportStdError> + Send,
{
    fn accept_provider_compression(self, encoding: TransportCompression) -> Self {
        self.accept_compressed(encoding)
    }

    fn send_provider_compression(self, encoding: TransportCompression) -> Self {
        self.send_compressed(encoding)
    }
}
