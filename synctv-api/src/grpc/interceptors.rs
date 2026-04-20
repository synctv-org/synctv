use sha2::{Digest, Sha256};
use std::fmt::Debug;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tonic::{Request, Status};
use tracing::warn;

/// Constant-time secret comparison to prevent timing attacks.
///
/// Both inputs are hashed to fixed-length SHA-256 digests before comparison,
/// so the execution time is independent of input lengths.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let hash_a = Sha256::digest(a);
    let hash_b = Sha256::digest(b);
    hash_a.ct_eq(&hash_b).into()
}

/// Shared-secret interceptor for cluster gRPC endpoints.
///
/// This is the only remaining transport-level gRPC interceptor used in
/// production. Business auth, blacklist, rate limiting, and timeout handling
/// all run explicitly in impls via `RequestExecutor`.
#[derive(Clone)]
pub struct ClusterAuthInterceptor {
    secret: Arc<String>,
}

impl ClusterAuthInterceptor {
    #[must_use]
    pub fn new(secret: String) -> Self {
        Self {
            secret: Arc::new(secret),
        }
    }

    /// Validate the shared secret from request metadata.
    #[allow(clippy::result_large_err)]
    pub fn validate<T>(&self, request: Request<T>) -> Result<Request<T>, Status> {
        let token = request
            .metadata()
            .get("x-cluster-secret")
            .ok_or_else(|| Status::unauthenticated("Missing x-cluster-secret header"))?
            .to_str()
            .map_err(|_| Status::unauthenticated("Invalid x-cluster-secret header"))?;

        if !constant_time_eq(token.as_bytes(), self.secret.as_bytes()) {
            warn!("Cluster gRPC auth failed: invalid secret");
            return Err(Status::unauthenticated("Invalid cluster secret"));
        }

        Ok(request)
    }
}

impl Debug for ClusterAuthInterceptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterAuthInterceptor").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constant_time_eq_equal() {
        assert!(constant_time_eq(b"secret", b"secret"));
    }

    #[test]
    fn test_constant_time_eq_not_equal() {
        assert!(!constant_time_eq(b"secret", b"Secret"));
    }

    #[test]
    fn test_constant_time_eq_different_lengths() {
        assert!(!constant_time_eq(b"short", b"longer_string"));
    }

    #[test]
    fn test_constant_time_eq_empty_inputs() {
        assert!(constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"", b"notempty"));
        assert!(!constant_time_eq(b"notempty", b""));
    }

    #[test]
    fn test_cluster_auth_interceptor_rejects_invalid_secret() {
        let interceptor = ClusterAuthInterceptor::new("cluster-secret".to_string());
        let mut request = tonic::Request::new(());
        request.metadata_mut().insert(
            "x-cluster-secret",
            "wrong-secret".parse().expect("metadata"),
        );

        let result = interceptor.validate(request);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_cluster_auth_interceptor_rejects_missing_secret() {
        let interceptor = ClusterAuthInterceptor::new("cluster-secret".to_string());
        let request = tonic::Request::new(());

        let result = interceptor.validate(request);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }
}
