//! Provider gRPC Server
//!
//! Standalone gRPC server that exposes provider services (Alist, Bilibili, Emby).
//! Can be deployed as a remote provider instance.
//!
//! Authentication is required via the `PROVIDER_AUTH_SECRET` environment variable.
//! Clients must pass the secret in the `x-provider-secret` gRPC metadata header.
//!
//! # Circuit Breaker
//! Each provider service is wrapped with a per-service circuit breaker to prevent
//! a failing backend from consuming all server threads. The circuit breaker tracks
//! consecutive failures and opens after `CIRCUIT_BREAKER_THRESHOLD` failures,
//! then transitions to half-open after `CIRCUIT_BREAKER_TIMEOUT_SECS` to allow recovery.
//!
//! The initial gRPC status header determines the outcome of an admitted RPC.
//! Backend failures that are encoded as gRPC error responses still count as
//! failures, while client/auth/validation errors do not poison the breaker.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use synctv_media_providers::circuit_breaker::CircuitBreaker;
use synctv_media_providers::grpc::{
    AlistService as AlistGrpcService, BilibiliService, EmbyService,
    alist::alist_server::AlistServer, bilibili::bilibili_server::BilibiliServer,
    emby::emby_server::EmbyServer,
};
use tonic::codec::CompressionEncoding;
use tonic::codegen::http::{HeaderMap, Response as HttpResponse};
use tonic::metadata::MetadataMap;
use tonic::service::LayerExt as _;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Server;
use tonic::{Code, Request, Status};
use tower::{Layer, Service};
use tracing::{Level, info, warn};

const PROVIDER_GRPC_MESSAGE_SIZE_LIMIT: usize = 4 * 1024 * 1024;
const PROVIDER_GRPC_FRAME_SIZE_LIMIT: u32 = 4 * 1024 * 1024;
const PROVIDER_GRPC_COMPRESSION_ENABLED_ENV: &str = "PROVIDER_GRPC_COMPRESSION_ENABLED";
const PROVIDER_LISTEN_ADDR_ENV: &str = "PROVIDER_LISTEN_ADDR";
const DEFAULT_PROVIDER_LISTEN_ADDR: &str = "[::]:50051";

fn grpc_status_code(headers: &HeaderMap) -> Code {
    headers
        .get(Status::GRPC_STATUS)
        .map_or(Code::Ok, |value| Code::from_bytes(value.as_bytes()))
}

fn should_record_circuit_breaker_success(headers: &HeaderMap) -> bool {
    grpc_status_code(headers) == Code::Ok
}

fn should_record_circuit_breaker_failure(headers: &HeaderMap) -> bool {
    matches!(
        grpc_status_code(headers),
        Code::Unknown
            | Code::DeadlineExceeded
            | Code::Aborted
            | Code::Internal
            | Code::Unavailable
            | Code::DataLoss
    )
}

fn parse_bool_env_value(env_name: &str, raw_value: &str) -> Result<bool, String> {
    let normalized = raw_value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(format!(
            "{env_name} must be a boolean value: true/false, 1/0, yes/no, or on/off"
        )),
    }
}

fn parse_env_bool(env_name: &str, default: bool) -> Result<bool, String> {
    match std::env::var(env_name) {
        Ok(raw_value) => parse_bool_env_value(env_name, &raw_value),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{env_name} must be valid UTF-8")),
    }
}

fn parse_listen_addr(raw_value: Option<&str>) -> Result<SocketAddr, String> {
    let value = raw_value.unwrap_or(DEFAULT_PROVIDER_LISTEN_ADDR).trim();
    if value.is_empty() {
        return Err(format!("{PROVIDER_LISTEN_ADDR_ENV} must not be empty"));
    }
    value
        .parse()
        .map_err(|error| format!("invalid {PROVIDER_LISTEN_ADDR_ENV} `{value}`: {error}"))
}

fn parse_listen_addr_env() -> Result<SocketAddr, String> {
    match std::env::var(PROVIDER_LISTEN_ADDR_ENV) {
        Ok(raw_value) => parse_listen_addr(Some(&raw_value)),
        Err(std::env::VarError::NotPresent) => parse_listen_addr(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(format!("{PROVIDER_LISTEN_ADDR_ENV} must be valid UTF-8"))
        }
    }
}

fn validate_provider_secret(metadata: &MetadataMap, expected_secret: &str) -> Result<(), Status> {
    let token = metadata
        .get("x-provider-secret")
        .ok_or_else(|| Status::unauthenticated("Missing x-provider-secret header"))?
        .to_str()
        .map_err(|_| Status::unauthenticated("Invalid x-provider-secret header"))?;

    if token.is_empty() {
        warn!("Provider gRPC auth failed: empty secret provided");
        return Err(Status::unauthenticated("Missing x-provider-secret header"));
    }

    let token_hash = Sha256::digest(token.as_bytes());
    let secret_hash = Sha256::digest(expected_secret.as_bytes());
    if !bool::from(token_hash.ct_eq(&secret_hash)) {
        warn!("Provider gRPC auth failed: invalid secret");
        return Err(Status::unauthenticated("Invalid provider secret"));
    }

    Ok(())
}

/// Tower [`Layer`] that wraps a gRPC service and signals the circuit breaker
/// after each RPC call completes.
///
/// On a successful gRPC response (`grpc-status = 0`) it calls
/// the permit’s success handler, resetting the failure counter and
/// returning the circuit to the Closed state. Backend failures returned as gRPC
/// status responses are classified from the `grpc-status` header so they still
/// open the circuit, while auth/validation/client errors do not.
#[derive(Clone)]
struct CircuitBreakerLayer {
    circuit_breaker: Arc<CircuitBreaker>,
    service_name: &'static str,
}

impl CircuitBreakerLayer {
    const fn new(circuit_breaker: Arc<CircuitBreaker>, service_name: &'static str) -> Self {
        Self {
            circuit_breaker,
            service_name,
        }
    }
}

impl<S> Layer<S> for CircuitBreakerLayer {
    type Service = CircuitBreakerService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CircuitBreakerService {
            inner,
            circuit_breaker: self.circuit_breaker.clone(),
            service_name: self.service_name,
        }
    }
}

/// Tower [`Service`] produced by [`CircuitBreakerLayer`].
#[derive(Clone)]
struct CircuitBreakerService<S> {
    inner: S,
    circuit_breaker: Arc<CircuitBreaker>,
    service_name: &'static str,
}

impl<S, Req> Service<Req> for CircuitBreakerService<S>
where
    S: Service<Req, Response = HttpResponse<tonic::body::Body>> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, S::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let service_name = self.service_name;
        let Some(permit) = self.circuit_breaker.try_request() else {
            let status = Status::unavailable(format!(
                "Service {service_name} is temporarily unavailable (circuit breaker open)"
            ));
            return Box::pin(async move { Ok(status.into_http()) });
        };
        let fut = self.inner.call(req);
        Box::pin(async move {
            let result = fut.await;
            match &result {
                Ok(response) => {
                    let headers = response.headers();
                    if should_record_circuit_breaker_success(headers) {
                        permit.success();
                    } else if should_record_circuit_breaker_failure(headers) {
                        permit.failure(service_name);
                    }
                }
                Err(_) => permit.failure(service_name),
            }
            result
        })
    }
}

fn provider_auth(secret: Arc<String>) -> impl tonic::service::Interceptor + Clone {
    move |request: Request<()>| {
        validate_provider_secret(request.metadata(), &secret)?;
        Ok(request)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_target(false)
        .init();

    // Read auth secret from environment variable (required)
    let auth_secret = std::env::var("PROVIDER_AUTH_SECRET").map_err(|_| {
        "PROVIDER_AUTH_SECRET environment variable is required for provider server authentication"
    })?;

    if auth_secret.is_empty() {
        return Err("PROVIDER_AUTH_SECRET must not be empty".into());
    }

    let addr = parse_listen_addr_env()?;
    let grpc_compression_enabled = parse_env_bool(PROVIDER_GRPC_COMPRESSION_ENABLED_ENV, true)?;

    info!("Starting Provider gRPC server on {}", addr);
    info!(
        "{}={}",
        PROVIDER_GRPC_COMPRESSION_ENABLED_ENV, grpc_compression_enabled
    );

    // Create service instances
    let alist_service = AlistGrpcService::new()?;
    let bilibili_service = BilibiliService::new()?;
    let emby_service = EmbyService::new()?;

    // Create one circuit breaker per provider service so that a
    // failing Alist backend does not open the circuit for Bilibili/Emby.
    let alist_cb = CircuitBreaker::new();
    let bilibili_cb = CircuitBreaker::new();
    let emby_cb = CircuitBreaker::new();

    let auth_secret = Arc::new(auth_secret);

    // Create circuit-breaker layers so only true gRPC successes reset the
    // breaker, while backend failures encoded in grpc-status still count.
    let alist_cb_layer = CircuitBreakerLayer::new(alist_cb, "alist");
    let bilibili_cb_layer = CircuitBreakerLayer::new(bilibili_cb, "bilibili");
    let emby_cb_layer = CircuitBreakerLayer::new(emby_cb, "emby");

    // Register gRPC Health Check service.
    // Keep status as NOT_SERVING during startup; set to SERVING only after all
    // initialization is complete so load balancers do not route traffic early.
    let (health_reporter, health_service) = tonic_health::server::health_reporter();

    // Build and start server with authentication on all services and graceful shutdown
    info!("Starting Provider gRPC server with graceful shutdown support");

    // All initialization (service construction, circuit breakers, interceptors) is
    // complete — mark services as SERVING now so health probes reflect true readiness.
    health_reporter
        .set_serving::<AlistServer<AlistGrpcService>>()
        .await;
    health_reporter
        .set_serving::<BilibiliServer<BilibiliService>>()
        .await;
    health_reporter
        .set_serving::<EmbyServer<EmbyService>>()
        .await;

    info!("All provider services initialized and marked SERVING");

    let alist_server = AlistServer::new(alist_service)
        .max_decoding_message_size(PROVIDER_GRPC_MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(PROVIDER_GRPC_MESSAGE_SIZE_LIMIT);
    let bilibili_server = BilibiliServer::new(bilibili_service)
        .max_decoding_message_size(PROVIDER_GRPC_MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(PROVIDER_GRPC_MESSAGE_SIZE_LIMIT);
    let emby_server = EmbyServer::new(emby_service)
        .max_decoding_message_size(PROVIDER_GRPC_MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(PROVIDER_GRPC_MESSAGE_SIZE_LIMIT);

    let alist_server = if grpc_compression_enabled {
        alist_server
            .accept_compressed(CompressionEncoding::Gzip)
            .send_compressed(CompressionEncoding::Gzip)
    } else {
        alist_server
    };
    let bilibili_server = if grpc_compression_enabled {
        bilibili_server
            .accept_compressed(CompressionEncoding::Gzip)
            .send_compressed(CompressionEncoding::Gzip)
    } else {
        bilibili_server
    };
    let emby_server = if grpc_compression_enabled {
        emby_server
            .accept_compressed(CompressionEncoding::Gzip)
            .send_compressed(CompressionEncoding::Gzip)
    } else {
        emby_server
    };
    let health_service = if grpc_compression_enabled {
        health_service
            .accept_compressed(CompressionEncoding::Gzip)
            .send_compressed(CompressionEncoding::Gzip)
    } else {
        health_service
    };

    Server::builder()
        .max_frame_size(Some(PROVIDER_GRPC_FRAME_SIZE_LIMIT))
        .concurrency_limit_per_connection(100)
        .add_service(InterceptedService::new(
            health_service,
            provider_auth(auth_secret.clone()),
        ))
        .add_service(InterceptedService::new(
            alist_cb_layer.named_layer(alist_server),
            provider_auth(auth_secret.clone()),
        ))
        .add_service(InterceptedService::new(
            bilibili_cb_layer.named_layer(bilibili_server),
            provider_auth(auth_secret.clone()),
        ))
        .add_service(InterceptedService::new(
            emby_cb_layer.named_layer(emby_server),
            provider_auth(auth_secret),
        ))
        .serve_with_shutdown(addr, shutdown_signal())
        .await?;

    info!("Provider gRPC server shut down gracefully");
    Ok(())
}

/// Wait for a shutdown signal (Ctrl+C or SIGTERM) for graceful connection draining.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            warn!(%error, "Failed to install Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                warn!(%error, "Failed to install SIGTERM handler");
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => { info!("Received Ctrl+C, shutting down..."); }
        () = terminate => { info!("Received SIGTERM, shutting down..."); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn provider_auth_and_open_breaker_gate_backend_calls() -> anyhow::Result<()> {
        use std::convert::Infallible;
        use std::sync::atomic::{AtomicU32, Ordering};
        use synctv_media_providers::circuit_breaker::CIRCUIT_BREAKER_THRESHOLD;
        use tonic::codegen::http::Request as HttpRequest;
        use tower::ServiceExt;

        let calls = Arc::new(AtomicU32::new(0));
        let backend_calls = Arc::clone(&calls);
        let backend = tower::service_fn(move |_: HttpRequest<tonic::body::Body>| {
            backend_calls.fetch_add(1, Ordering::Relaxed);
            std::future::ready(Ok::<_, Infallible>(
                Status::unavailable("backend").into_http(),
            ))
        });
        let breaker = CircuitBreakerLayer::new(CircuitBreaker::new(), "test");
        let service = InterceptedService::new(
            breaker.layer(backend),
            provider_auth(Arc::new("test-secret".to_string())),
        );
        for _ in 0..=CIRCUIT_BREAKER_THRESHOLD {
            let request = HttpRequest::builder()
                .header("x-provider-secret", "test-secret")
                .body(tonic::body::Body::empty())?;
            let response = service.clone().oneshot(request).await?;
            assert_eq!(grpc_status_code(response.headers()), Code::Unavailable);
        }
        let response = service
            .oneshot(HttpRequest::new(tonic::body::Body::empty()))
            .await?;
        assert_eq!(grpc_status_code(response.headers()), Code::Unauthenticated);
        assert_eq!(calls.load(Ordering::Relaxed), CIRCUIT_BREAKER_THRESHOLD);
        Ok(())
    }

    fn grpc_headers(code: Option<&str>) -> Result<HeaderMap, Box<dyn std::error::Error>> {
        let mut headers = HeaderMap::new();
        if let Some(code) = code {
            headers.insert(Status::GRPC_STATUS, code.parse()?);
        }
        Ok(headers)
    }

    #[test]
    fn circuit_breaker_treats_ok_response_as_success() -> Result<(), Box<dyn std::error::Error>> {
        let headers = grpc_headers(Some("0"))?;
        assert!(should_record_circuit_breaker_success(&headers));
        assert!(!should_record_circuit_breaker_failure(&headers));
        Ok(())
    }

    #[test]
    fn circuit_breaker_treats_backend_failure_codes_as_failures()
    -> Result<(), Box<dyn std::error::Error>> {
        for code in ["2", "4", "10", "13", "14", "15"] {
            let headers = grpc_headers(Some(code))?;
            assert!(
                should_record_circuit_breaker_failure(&headers),
                "grpc-status {code} should count as a backend failure"
            );
        }
        Ok(())
    }

    #[test]
    fn circuit_breaker_ignores_client_and_auth_errors() -> Result<(), Box<dyn std::error::Error>> {
        for code in ["3", "5", "7", "8", "9", "11", "12", "16"] {
            let headers = grpc_headers(Some(code))?;
            assert!(
                !should_record_circuit_breaker_failure(&headers),
                "grpc-status {code} should not poison the service breaker"
            );
        }
        Ok(())
    }

    #[test]
    fn missing_grpc_status_defaults_to_success_path() -> Result<(), Box<dyn std::error::Error>> {
        let headers = grpc_headers(None)?;
        assert!(should_record_circuit_breaker_success(&headers));
        assert!(!should_record_circuit_breaker_failure(&headers));
        Ok(())
    }

    #[test]
    fn parse_bool_env_value_accepts_common_values() -> Result<(), Box<dyn std::error::Error>> {
        for value in ["true", "1", "yes", "on", " TRUE "] {
            assert!(parse_bool_env_value(
                PROVIDER_GRPC_COMPRESSION_ENABLED_ENV,
                value
            )?);
        }

        for value in ["false", "0", "no", "off", " FALSE "] {
            assert!(!parse_bool_env_value(
                PROVIDER_GRPC_COMPRESSION_ENABLED_ENV,
                value
            )?);
        }
        Ok(())
    }

    #[test]
    fn parse_bool_env_value_rejects_invalid_values() {
        assert!(parse_bool_env_value(PROVIDER_GRPC_COMPRESSION_ENABLED_ENV, "maybe").is_err());
    }

    #[test]
    fn parse_listen_addr_uses_default_when_absent() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            parse_listen_addr(None)?,
            DEFAULT_PROVIDER_LISTEN_ADDR.parse::<SocketAddr>()?
        );
        Ok(())
    }

    #[test]
    fn parse_listen_addr_accepts_explicit_socket_addr() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            parse_listen_addr(Some("127.0.0.1:50052"))?,
            "127.0.0.1:50052".parse::<SocketAddr>()?
        );
        Ok(())
    }

    #[test]
    fn parse_listen_addr_rejects_empty_value() -> Result<(), Box<dyn std::error::Error>> {
        let error = parse_listen_addr(Some("   "))
            .err()
            .ok_or("empty listen addr should fail")?;
        assert!(error.contains("must not be empty"));
        Ok(())
    }

    #[test]
    fn parse_listen_addr_rejects_invalid_value() -> Result<(), Box<dyn std::error::Error>> {
        let Err(error) = parse_listen_addr(Some("not-a-socket")) else {
            return Err("invalid listen addr should fail".into());
        };
        assert!(error.contains("invalid PROVIDER_LISTEN_ADDR"));
        Ok(())
    }
}
