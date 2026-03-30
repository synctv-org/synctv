use std::future::Future;
use std::pin::Pin;
use std::sync::LazyLock;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::{body::Body as AxumBody, http};
use prost::Message;
use prost_types::FileDescriptorSet;
use tower::{Layer, Service};

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type GrpcPathSet = std::collections::HashSet<String>;

static STREAMING_GRPC_PATHS: LazyLock<GrpcPathSet> = LazyLock::new(collect_streaming_grpc_paths);

#[derive(Clone, Copy, Debug)]
pub struct GrpcRequestTimeoutLayer {
    timeout: Duration,
}

impl GrpcRequestTimeoutLayer {
    #[must_use]
    pub const fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl<S> Layer<S> for GrpcRequestTimeoutLayer {
    type Service = GrpcRequestTimeoutService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        GrpcRequestTimeoutService {
            inner,
            timeout: self.timeout,
        }
    }
}

#[derive(Clone)]
pub struct GrpcRequestTimeoutService<S> {
    inner: S,
    timeout: Duration,
}

#[must_use]
pub(crate) fn is_streaming_grpc_path(path: &str) -> bool {
    STREAMING_GRPC_PATHS.contains(path)
}

fn collect_streaming_grpc_paths() -> GrpcPathSet {
    let mut paths = GrpcPathSet::new();
    for descriptor_bytes in [
        synctv_proto::FILE_DESCRIPTOR_SET,
        synctv_livestream::FILE_DESCRIPTOR_SET,
    ] {
        match FileDescriptorSet::decode(descriptor_bytes) {
            Ok(descriptor_set) => append_streaming_paths_from_set(&mut paths, &descriptor_set),
            Err(error) => {
                tracing::error!(%error, "failed to decode gRPC descriptor set for timeout layer");
            }
        }
    }
    paths
}

fn append_streaming_paths_from_set(paths: &mut GrpcPathSet, descriptor_set: &FileDescriptorSet) {
    for file in &descriptor_set.file {
        let package = file.package.as_deref().unwrap_or_default();
        for service in &file.service {
            let Some(service_name) = service.name.as_deref() else {
                continue;
            };

            for method in &service.method {
                if !(method.client_streaming() || method.server_streaming()) {
                    continue;
                }
                let Some(method_name) = method.name.as_deref() else {
                    continue;
                };

                let full_service_name = if package.is_empty() {
                    service_name.to_string()
                } else {
                    format!("{package}.{service_name}")
                };
                paths.insert(format!("/{full_service_name}/{method_name}"));
            }
        }
    }
}

fn timeout_response(path: &str, timeout: Duration) -> http::Response<AxumBody> {
    tracing::warn!(
        path,
        timeout_ms = timeout.as_millis(),
        "gRPC unary request exceeded timeout budget"
    );
    tonic::Status::deadline_exceeded(format!(
        "Request exceeded server timeout of {}s",
        timeout.as_secs()
    ))
    .into_http::<tonic::body::Body>()
    .map(AxumBody::new)
}

impl<S> Service<http::Request<AxumBody>> for GrpcRequestTimeoutService<S>
where
    S: Service<http::Request<AxumBody>, Response = http::Response<AxumBody>>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<AxumBody>) -> Self::Future {
        let path = req.uri().path().to_string();
        if is_streaming_grpc_path(&path) {
            let mut inner = self.inner.clone();
            std::mem::swap(&mut self.inner, &mut inner);
            return Box::pin(async move { inner.call(req).await });
        }

        let timeout = self.timeout;
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        Box::pin(async move {
            match tokio::time::timeout(timeout, inner.call(req)).await {
                Ok(result) => result,
                Err(_) => Ok(timeout_response(&path, timeout)),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{append_streaming_paths_from_set, is_streaming_grpc_path, GrpcRequestTimeoutLayer};
    use axum::body::Body as AxumBody;
    use axum::http;
    use prost_types::{
        FileDescriptorProto, FileDescriptorSet, MethodDescriptorProto, ServiceDescriptorProto,
    };
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tower::{service_fn, Layer, Service, ServiceExt};

    fn request(path: &str) -> http::Request<AxumBody> {
        http::Request::builder()
            .uri(path)
            .body(AxumBody::empty())
            .expect("request should build")
    }

    #[test]
    fn test_is_streaming_grpc_path_matches_known_streaming_methods() {
        assert!(is_streaming_grpc_path(
            "/synctv.client.RoomService/MessageStream"
        ));
        assert!(is_streaming_grpc_path(
            "/synctv.stream.StreamRelayService/PullRtmpStream"
        ));
        assert!(!is_streaming_grpc_path("/synctv.client.AuthService/Login"));
    }

    #[test]
    fn test_descriptor_discovery_marks_streaming_methods_only() {
        let descriptor_set = FileDescriptorSet {
            file: vec![FileDescriptorProto {
                package: Some("example.pkg".to_string()),
                service: vec![ServiceDescriptorProto {
                    name: Some("ExampleService".to_string()),
                    method: vec![
                        MethodDescriptorProto {
                            name: Some("UnaryCall".to_string()),
                            client_streaming: Some(false),
                            server_streaming: Some(false),
                            ..MethodDescriptorProto::default()
                        },
                        MethodDescriptorProto {
                            name: Some("ServerStream".to_string()),
                            client_streaming: Some(false),
                            server_streaming: Some(true),
                            ..MethodDescriptorProto::default()
                        },
                    ],
                    ..ServiceDescriptorProto::default()
                }],
                ..FileDescriptorProto::default()
            }],
        };

        let mut discovered = std::collections::HashSet::new();
        append_streaming_paths_from_set(&mut discovered, &descriptor_set);

        assert!(discovered.contains("/example.pkg.ExampleService/ServerStream"));
        assert!(!discovered.contains("/example.pkg.ExampleService/UnaryCall"));
    }

    #[tokio::test]
    async fn test_grpc_request_timeout_layer_maps_unary_timeout_to_deadline_exceeded() {
        let layer = GrpcRequestTimeoutLayer::new(Duration::from_millis(50));
        let mut svc = layer.layer(service_fn(
            |_req: http::Request<AxumBody>| async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok::<_, Infallible>(
                    http::Response::builder()
                        .status(http::StatusCode::OK)
                        .body(AxumBody::empty())
                        .expect("response should build"),
                )
            },
        ));

        let response = svc
            .ready()
            .await
            .expect("service should be ready")
            .call(request("/synctv.client.AuthService/Login"))
            .await
            .expect("timeout layer should return a grpc response");

        assert_eq!(
            response
                .headers()
                .get("grpc-status")
                .and_then(|value| value.to_str().ok()),
            Some("4")
        );
    }

    #[tokio::test]
    async fn test_grpc_request_timeout_layer_skips_known_streaming_methods() {
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = Arc::clone(&called);
        let layer = GrpcRequestTimeoutLayer::new(Duration::from_millis(20));
        let mut svc = layer.layer(service_fn(move |_req: http::Request<AxumBody>| {
            let called_clone = Arc::clone(&called_clone);
            async move {
                called_clone.store(true, Ordering::SeqCst);
                futures::future::pending::<Result<http::Response<AxumBody>, Infallible>>()
                    .await
            }
        }));

        let result = tokio::time::timeout(
            Duration::from_millis(80),
            svc.ready()
                .await
                .expect("service should be ready")
                .call(request("/synctv.client.RoomService/MessageStream")),
        )
        .await;

        assert!(
            result.is_err(),
            "streaming methods should remain governed by the caller, not by the unary timeout layer"
        );
        assert!(
            called.load(Ordering::SeqCst),
            "streaming requests should still reach the inner service"
        );
    }
}
