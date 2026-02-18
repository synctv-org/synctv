//! Health check and metrics endpoints
//!
//! Provides health check endpoints for Kubernetes readiness/liveness probes and Prometheus metrics.
//!
//! # Endpoints
//!
//! - `/health/live` - Liveness probe: checks if the application is running (basic check)
//! - `/health/ready` - Readiness probe: checks if dependencies (DB, Redis) are healthy
//! - `/health` - Alias for `/health/live` for backward compatibility
//! - `/metrics` - Prometheus metrics endpoint

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::warn;

use crate::http::AppState;
use crate::observability::metrics;

/// Timeout for individual health check probes (DB, Redis).
/// Prevents a hung dependency from blocking the readiness endpoint indefinitely.
const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Health check router (without metrics endpoint)
///
/// To expose the `/metrics` endpoint, set `server.metrics_enabled = true`
/// in the application config (or `metrics.enabled` in Helm values).
pub fn create_health_router() -> Router<AppState> {
    

    // Metrics are conditionally registered via create_health_router_with_config
    Router::new()
        .route("/health", get(liveness_check))
        .route("/health/live", get(liveness_check))
        .route("/health/ready", get(readiness_check))
}

/// Create health router with `/metrics` Prometheus endpoint
pub fn create_health_router_with_metrics() -> Router<AppState> {
    Router::new()
        .route("/health", get(liveness_check))
        .route("/health/live", get(liveness_check))
        .route("/health/ready", get(readiness_check))
        .route("/metrics", get(prometheus_metrics))
}

/// Health check response structure
#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<HealthDetails>,
}

/// Detailed health check information
#[derive(Debug, Serialize, Deserialize)]
pub struct HealthDetails {
    pub database: String,
    pub redis: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Liveness probe - checks if the application process is running
///
/// This is a basic check that always returns OK if the server is responding.
/// Kubernetes uses this to determine if the pod needs to be restarted.
///
/// Returns:
/// - 200 OK: Application is alive
pub async fn liveness_check() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(HealthResponse {
            status: "ok".to_string(),
            details: None,
        }),
    )
}

/// Readiness probe - checks if the application is ready to serve traffic
///
/// **Production Enhancement (#25)**: Health check validates critical dependencies:
/// - Database connectivity (PostgreSQL) - Executes a test query via user_service
/// - Redis connectivity - Sends PING command, gracefully handles "not configured" case
///
/// Kubernetes uses this to determine if the pod should receive traffic.
/// A failing health check will prevent traffic routing until dependencies recover.
///
/// Returns:
/// - 200 OK: All dependencies are healthy
/// - 503 Service Unavailable: One or more dependencies are unhealthy
pub async fn readiness_check(State(state): State<AppState>) -> impl IntoResponse {
    let mut is_healthy = true;
    let mut error_messages = Vec::new();

    // Check database connectivity
    let db_status = match check_database_health(&state).await {
        Ok(()) => "healthy".to_string(),
        Err(e) => {
            error_messages.push(format!("Database: {e}"));
            is_healthy = false;
            warn!("Database health check failed: {}", e);
            "unhealthy".to_string()
        }
    };

    // Check Redis connectivity
    let redis_status = match check_redis_health(&state).await {
        Ok(()) => "healthy".to_string(),
        Err(e) => {
            error_messages.push(format!("Redis: {e}"));
            is_healthy = false;
            warn!("Redis health check failed: {}", e);
            "unhealthy".to_string()
        }
    };

    // Check cluster health (only when cluster mode is active)
    let cluster_status = match check_cluster_health(&state) {
        Some(Ok(())) => Some("healthy".to_string()),
        Some(Err(e)) => {
            error_messages.push(format!("Cluster: {e}"));
            is_healthy = false;
            warn!("Cluster health check failed: {}", e);
            Some("unhealthy".to_string())
        }
        None => None, // No cluster manager, single-node mode
    };

    let status_code = if is_healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    let response = HealthResponse {
        status: if is_healthy { "healthy".to_string() } else { "unhealthy".to_string() },
        details: Some(HealthDetails {
            database: db_status,
            redis: redis_status,
            cluster: cluster_status,
            message: if error_messages.is_empty() {
                None
            } else {
                Some(error_messages.join("; "))
            },
        }),
    };

    (status_code, Json(response))
}

/// Check database connectivity with timeout
async fn check_database_health(state: &AppState) -> Result<(), String> {
    match tokio::time::timeout(HEALTH_CHECK_TIMEOUT, state.user_service.health_check()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            warn!("Database health check failed: {}", e);
            Err(format!("Database connection failed: {e}"))
        }
        Err(_) => {
            warn!("Database health check timed out after {}s", HEALTH_CHECK_TIMEOUT.as_secs());
            Err(format!("Database health check timed out after {}s", HEALTH_CHECK_TIMEOUT.as_secs()))
        }
    }
}

/// Check cluster health by verifying the ClusterManager is operational.
///
/// Returns `None` if no cluster manager is configured (single-node mode).
/// Returns `Some(Ok(()))` if the cluster manager is healthy.
/// Returns `Some(Err(...))` if the cluster manager reports issues.
fn check_cluster_health(state: &AppState) -> Option<Result<(), String>> {
    let cm = state.cluster_manager.as_ref()?;
    let metrics = cm.metrics();

    // Verify node has a valid ID (non-empty)
    if metrics.node_id.is_empty() {
        return Some(Err("Cluster node ID is empty".to_string()));
    }

    // If Redis pub/sub should be enabled but isn't, the node can't sync with the cluster
    if !metrics.redis_enabled && state.redis_publish_tx.is_some() {
        return Some(Err("Cluster Redis pub/sub is not connected".to_string()));
    }

    Some(Ok(()))
}

/// Check Redis connectivity by sending a PING command with timeout
async fn check_redis_health(state: &AppState) -> Result<(), String> {
    match tokio::time::timeout(HEALTH_CHECK_TIMEOUT, state.rate_limiter.health_check()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) if e.contains("not configured") => {
            // Redis not configured - acceptable in some deployments
            Ok(())
        }
        Ok(Err(e)) => {
            warn!("Redis health check failed: {}", e);
            Err(e)
        }
        Err(_) => {
            warn!("Redis health check timed out after {}s", HEALTH_CHECK_TIMEOUT.as_secs());
            Err(format!("Redis health check timed out after {}s", HEALTH_CHECK_TIMEOUT.as_secs()))
        }
    }
}

/// Prometheus metrics endpoint
pub async fn prometheus_metrics() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        metrics::gather_metrics(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn test_liveness_check_returns_ok() {
        let response = liveness_check().await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_liveness_check_response_body() {
        let response = liveness_check().await.into_response();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let health: HealthResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(health.status, "ok");
        assert!(health.details.is_none());
    }

    #[test]
    fn test_health_response_serialization() {
        let response = HealthResponse {
            status: "healthy".to_string(),
            details: Some(HealthDetails {
                database: "healthy".to_string(),
                redis: "healthy".to_string(),
                cluster: None,
                message: None,
            }),
        };
        let json = serde_json::to_string(&response).unwrap();
        let back: HealthResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status, "healthy");
        let details = back.details.unwrap();
        assert_eq!(details.database, "healthy");
        assert_eq!(details.redis, "healthy");
        assert!(details.cluster.is_none());
        assert!(details.message.is_none());
    }

    #[test]
    fn test_health_response_with_cluster_status() {
        let response = HealthResponse {
            status: "healthy".to_string(),
            details: Some(HealthDetails {
                database: "healthy".to_string(),
                redis: "healthy".to_string(),
                cluster: Some("healthy".to_string()),
                message: None,
            }),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"cluster\":\"healthy\""));
    }

    #[test]
    fn test_health_response_with_error_message() {
        let response = HealthResponse {
            status: "unhealthy".to_string(),
            details: Some(HealthDetails {
                database: "unhealthy".to_string(),
                redis: "healthy".to_string(),
                cluster: None,
                message: Some("Database: connection refused".to_string()),
            }),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("Database: connection refused"));
    }

    #[test]
    fn test_health_response_skips_none_message() {
        let response = HealthResponse {
            status: "healthy".to_string(),
            details: Some(HealthDetails {
                database: "healthy".to_string(),
                redis: "healthy".to_string(),
                cluster: None,
                message: None,
            }),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(!json.contains("message"));
        assert!(!json.contains("cluster"));
    }

    #[test]
    fn test_health_response_skips_none_details() {
        let response = HealthResponse {
            status: "ok".to_string(),
            details: None,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(!json.contains("details"));
    }

    #[test]
    fn test_health_response_deserialization_without_optional_fields() {
        let json = r#"{"status":"ok"}"#;
        let response: HealthResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.status, "ok");
        assert!(response.details.is_none());
    }
}
