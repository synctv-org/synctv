//! Provider gRPC Server
//!
//! Standalone gRPC server that exposes provider services (Alist, Bilibili, Emby).
//! Can be deployed as a remote provider instance.
//!
//! Authentication is required via the `PROVIDER_AUTH_SECRET` environment variable.
//! Clients must pass the secret in the `x-provider-secret` gRPC metadata header.
//!
//! # Circuit Breaker (Issue #34)
//!
//! Each provider service is wrapped with a per-service circuit breaker to prevent
//! a failing backend from consuming all server threads. The circuit breaker tracks
//! consecutive failures and opens after `CIRCUIT_BREAKER_THRESHOLD` failures,
//! then transitions to half-open after `CIRCUIT_BREAKER_TIMEOUT` to allow recovery.
//!
//! `record_success` is called via `CircuitBreakerLayer` (a tower middleware layer)
//! after each RPC call returns a non-error response, allowing the circuit to
//! recover to the Closed state after transient failures resolve.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicI64, Ordering};
use std::task::{Context, Poll};

use sha2::{Sha256, Digest};
use subtle::ConstantTimeEq;
use synctv_media_providers::grpc::{
    alist::alist_server::AlistServer,
    alist_server::AlistService as AlistGrpcService,
    bilibili::bilibili_server::BilibiliServer,
    bilibili_server::BilibiliService,
    emby::emby_server::EmbyServer,
    emby_server::EmbyService,
};
use tonic::{Request, Status};
use tonic::transport::Server;
use tonic::service::LayerExt as _;
use tower::{Layer, Service};
use tracing::{error, info, warn, Level};

/// Number of consecutive failures before the circuit opens.
const CIRCUIT_BREAKER_THRESHOLD: u32 = 5;

/// Seconds the circuit stays open before transitioning to half-open.
const CIRCUIT_BREAKER_TIMEOUT_SECS: i64 = 30;

/// Circuit breaker state per provider service.
///
/// States:
///   - `consecutive_failures < THRESHOLD` → **Closed** (requests pass through)
///   - `consecutive_failures >= THRESHOLD` and within timeout → **Open** (requests rejected)
///   - After timeout → **Half-open** (next request allowed as a probe; resets or re-opens)
#[derive(Default)]
struct CircuitBreaker {
    /// Number of consecutive failures (reset to 0 on success)
    consecutive_failures: AtomicU32,
    /// Unix timestamp (seconds) when the circuit was opened. -1 = never opened.
    opened_at: AtomicI64,
}

impl CircuitBreaker {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            consecutive_failures: AtomicU32::new(0),
            opened_at: AtomicI64::new(-1),
        })
    }

    /// Check whether a request should be allowed through.
    fn allow_request(&self) -> bool {
        let failures = self.consecutive_failures.load(Ordering::Relaxed);
        if failures < CIRCUIT_BREAKER_THRESHOLD {
            return true; // Closed
        }
        // Circuit is open — check if the half-open timeout has elapsed
        let opened_at = self.opened_at.load(Ordering::Relaxed);
        if opened_at < 0 {
            return true;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        now.saturating_sub(opened_at) >= CIRCUIT_BREAKER_TIMEOUT_SECS
    }

    /// Record a successful request: reset failure counter.
    fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
        self.opened_at.store(-1, Ordering::Relaxed);
    }

    /// Record a failure: increment counter and open circuit if threshold reached.
    fn record_failure(&self, service: &str) {
        let prev = self.consecutive_failures.fetch_add(1, Ordering::Relaxed);
        if prev + 1 >= CIRCUIT_BREAKER_THRESHOLD {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64);
            // Only update opened_at when transitioning to Open
            if prev + 1 == CIRCUIT_BREAKER_THRESHOLD {
                self.opened_at.store(now, Ordering::Relaxed);
                error!(
                    service = %service,
                    threshold = CIRCUIT_BREAKER_THRESHOLD,
                    "Circuit breaker opened after {} consecutive failures",
                    CIRCUIT_BREAKER_THRESHOLD
                );
            }
        }
    }
}

/// Tower [`Layer`] that wraps a gRPC service and signals the circuit breaker
/// after each RPC call completes.
///
/// On a successful response (`Ok`) it calls [`CircuitBreaker::record_success`],
/// resetting the failure counter and returning the circuit to the Closed state.
/// On an error response (`Err`) it calls [`CircuitBreaker::record_failure`] so
/// that repeated backend errors eventually open the circuit.
#[derive(Clone)]
struct CircuitBreakerLayer {
    circuit_breaker: Arc<CircuitBreaker>,
    service_name: &'static str,
}

impl CircuitBreakerLayer {
    fn new(circuit_breaker: Arc<CircuitBreaker>, service_name: &'static str) -> Self {
        Self { circuit_breaker, service_name }
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
    S: Service<Req> + Clone + Send + 'static,
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
        let cb = self.circuit_breaker.clone();
        let service_name = self.service_name;
        let fut = self.inner.call(req);
        Box::pin(async move {
            let result = fut.await;
            match &result {
                Ok(_) => cb.record_success(),
                Err(_) => cb.record_failure(service_name),
            }
            result
        })
    }
}

/// Shared-secret interceptor for provider gRPC endpoints.
///
/// Validates that incoming requests carry the correct shared secret
/// in the `x-provider-secret` metadata header using constant-time comparison.
///
/// Issue #34: Also enforces the per-service circuit breaker so that a
/// continuously failing backend cannot consume all server threads.
#[derive(Clone)]
struct ProviderAuthInterceptor {
    secret: Arc<String>,
    circuit_breaker: Arc<CircuitBreaker>,
    service_name: &'static str,
}

impl ProviderAuthInterceptor {
    fn new(secret: String, circuit_breaker: Arc<CircuitBreaker>, service_name: &'static str) -> Self {
        Self {
            secret: Arc::new(secret),
            circuit_breaker,
            service_name,
        }
    }

    #[allow(clippy::result_large_err)]
    fn validate<T>(&self, request: Request<T>) -> Result<Request<T>, Status> {
        // Issue #34: Check circuit breaker before auth to short-circuit quickly
        if !self.circuit_breaker.allow_request() {
            warn!(
                service = %self.service_name,
                "Circuit breaker open — rejecting request"
            );
            return Err(Status::unavailable(format!(
                "Service {} is temporarily unavailable (circuit breaker open)",
                self.service_name
            )));
        }

        let token = request
            .metadata()
            .get("x-provider-secret")
            .ok_or_else(|| Status::unauthenticated("Missing x-provider-secret header"))?
            .to_str()
            .map_err(|_| Status::unauthenticated("Invalid x-provider-secret header"))?;

        // Issue #34: Validate that the secret is non-empty at the call site too
        if token.is_empty() {
            warn!("Provider gRPC auth failed: empty secret provided");
            self.circuit_breaker.record_failure(self.service_name);
            return Err(Status::unauthenticated("Missing x-provider-secret header"));
        }

        let token_hash = Sha256::digest(token.as_bytes());
        let secret_hash = Sha256::digest(self.secret.as_bytes());
        if !bool::from(token_hash.ct_eq(&secret_hash)) {
            warn!("Provider gRPC auth failed: invalid secret");
            // Auth failures don't count as circuit-breaker events (they are
            // expected from mis-configured clients, not from a failing backend)
            return Err(Status::unauthenticated("Invalid provider secret"));
        }

        Ok(request)
    }
}

#[tokio::main]
#[allow(clippy::result_large_err)]
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

    let addr = std::env::var("PROVIDER_LISTEN_ADDR")
        .unwrap_or_else(|_| "[::]:50051".to_string())
        .parse()?;

    info!("Starting Provider gRPC server on {}", addr);

    // Create service instances
    let alist_service = AlistGrpcService::new();
    let bilibili_service = BilibiliService::new();
    let emby_service = EmbyService::new();

    // Issue #34: Create one circuit breaker per provider service so that a
    // failing Alist backend does not open the circuit for Bilibili/Emby.
    let alist_cb = CircuitBreaker::new();
    let bilibili_cb = CircuitBreaker::new();
    let emby_cb = CircuitBreaker::new();

    // Create auth interceptors (one per service, they are Clone + cheap)
    let alist_auth = ProviderAuthInterceptor::new(auth_secret.clone(), alist_cb.clone(), "alist");
    let bilibili_auth = ProviderAuthInterceptor::new(auth_secret.clone(), bilibili_cb.clone(), "bilibili");
    let emby_auth = ProviderAuthInterceptor::new(auth_secret, emby_cb.clone(), "emby");

    // Create circuit-breaker layers so record_success is called after every
    // successful RPC response, allowing the circuit to recover to Closed state.
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

    Server::builder()
        .max_frame_size(Some(4 * 1024 * 1024))
        .concurrency_limit_per_connection(100)
        .add_service(health_service)
        .add_service(alist_cb_layer.named_layer(
            AlistServer::with_interceptor(alist_service, move |req| alist_auth.validate(req)),
        ))
        .add_service(bilibili_cb_layer.named_layer(
            BilibiliServer::with_interceptor(bilibili_service, move |req| {
                bilibili_auth.validate(req)
            }),
        ))
        .add_service(emby_cb_layer.named_layer(
            EmbyServer::with_interceptor(emby_service, move |req| emby_auth.validate(req)),
        ))
        .serve_with_shutdown(addr, shutdown_signal())
        .await?;

    info!("Provider gRPC server shut down gracefully");
    Ok(())
}

/// Wait for a shutdown signal (Ctrl+C or SIGTERM) for graceful connection draining.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => { info!("Received Ctrl+C, shutting down..."); }
        () = terminate => { info!("Received SIGTERM, shutting down..."); }
    }
}
