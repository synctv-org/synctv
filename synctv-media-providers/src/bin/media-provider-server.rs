//! Provider gRPC Server
//!
//! Standalone gRPC server that exposes provider services (Alist, Bilibili, Emby).
//! Can be deployed as a remote provider instance.
//!
//! Authentication is required via the `PROVIDER_AUTH_SECRET` environment variable.
//! Clients must pass the secret in the `x-provider-secret` gRPC metadata header.

use std::sync::Arc;

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
use tracing::{info, warn, Level};

/// Shared-secret interceptor for provider gRPC endpoints.
///
/// Validates that incoming requests carry the correct shared secret
/// in the `x-provider-secret` metadata header using constant-time comparison.
#[derive(Clone)]
struct ProviderAuthInterceptor {
    secret: Arc<String>,
}

impl ProviderAuthInterceptor {
    fn new(secret: String) -> Self {
        Self {
            secret: Arc::new(secret),
        }
    }

    #[allow(clippy::result_large_err)]
    fn validate<T>(&self, request: Request<T>) -> Result<Request<T>, Status> {
        let token = request
            .metadata()
            .get("x-provider-secret")
            .ok_or_else(|| Status::unauthenticated("Missing x-provider-secret header"))?
            .to_str()
            .map_err(|_| Status::unauthenticated("Invalid x-provider-secret header"))?;

        let token_hash = Sha256::digest(token.as_bytes());
        let secret_hash = Sha256::digest(self.secret.as_bytes());
        if !bool::from(token_hash.ct_eq(&secret_hash)) {
            warn!("Provider gRPC auth failed: invalid secret");
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
        .unwrap_or_else(|_| "[::1]:50051".to_string())
        .parse()?;

    info!("Starting Provider gRPC server on {}", addr);

    // Create service instances
    let alist_service = AlistGrpcService::new();
    let bilibili_service = BilibiliService::new();
    let emby_service = EmbyService::new();

    // Create auth interceptors (one per service, they are Clone + cheap)
    let alist_auth = ProviderAuthInterceptor::new(auth_secret.clone());
    let bilibili_auth = ProviderAuthInterceptor::new(auth_secret.clone());
    let emby_auth = ProviderAuthInterceptor::new(auth_secret);

    // Build and start server with authentication on all services and graceful shutdown
    info!("Starting Provider gRPC server with graceful shutdown support");

    Server::builder()
        .add_service(AlistServer::with_interceptor(alist_service, move |req| {
            alist_auth.validate(req)
        }))
        .add_service(BilibiliServer::with_interceptor(
            bilibili_service,
            move |req| bilibili_auth.validate(req),
        ))
        .add_service(EmbyServer::with_interceptor(emby_service, move |req| {
            emby_auth.validate(req)
        }))
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
