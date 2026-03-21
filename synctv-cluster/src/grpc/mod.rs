//! Cluster gRPC communication

pub mod circuit_breaker;
pub mod client;
pub mod server;

// Include generated protobuf code
pub mod synctv {
    pub mod cluster {
        include!("proto/synctv.cluster.rs");
    }
}

pub use client::{ClusterClient, ClusterClientConfig, FanOutResult};
pub use server::ClusterServer;
pub use synctv::cluster::cluster_service_server::ClusterServiceServer;

use subtle::ConstantTimeEq;
use tonic::{Request, Status};

/// Constant-time byte comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

/// Shared-secret interceptor for cluster gRPC endpoints.
///
/// Validates that incoming inter-node requests carry the correct shared secret
/// in the `x-cluster-secret` metadata header using constant-time comparison.
#[derive(Clone)]
pub struct ClusterAuthInterceptor {
    secret: std::sync::Arc<String>,
}

impl ClusterAuthInterceptor {
    #[must_use]
    pub fn new(secret: String) -> Self {
        Self {
            secret: std::sync::Arc::new(secret),
        }
    }

    /// Validate the shared secret from request metadata
    #[allow(clippy::result_large_err)]
    pub fn validate<T>(&self, request: Request<T>) -> Result<Request<T>, Status> {
        self.validate_metadata(request.metadata())?;
        Ok(request)
    }

    /// Validate the shared secret directly from request metadata.
    #[allow(clippy::result_large_err)]
    pub fn validate_metadata(
        &self,
        metadata: &tonic::metadata::MetadataMap,
    ) -> Result<(), Status> {
        if self.secret.is_empty() {
            tracing::error!(
                "Cluster gRPC auth interceptor is misconfigured: shared secret is empty"
            );
            return Err(Status::unauthenticated(
                "Cluster authentication secret is not configured",
            ));
        }

        let token = metadata
            .get("x-cluster-secret")
            .ok_or_else(|| Status::unauthenticated("Missing x-cluster-secret header"))?
            .to_str()
            .map_err(|_| Status::unauthenticated("Invalid x-cluster-secret header"))?;

        if !constant_time_eq(token.as_bytes(), self.secret.as_bytes()) {
            tracing::warn!("Cluster gRPC auth failed: invalid secret");
            return Err(Status::unauthenticated("Invalid cluster secret"));
        }

        Ok(())
    }
}

impl std::fmt::Debug for ClusterAuthInterceptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterAuthInterceptor").finish()
    }
}
