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
    http::{HeaderMap, StatusCode},
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
    pub ws_ticket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub livestream: Option<String>,
    /// Memory usage percentage (0-100). None if unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryHealth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Memory health information
#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryHealth {
    /// Memory usage percentage (0-100)
    pub usage_percent: f64,
    /// Human-readable status: "healthy" or "unhealthy"
    pub status: String,
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
/// - Database connectivity (`PostgreSQL`) - Executes a test query via `user_service`
/// - Redis connectivity - Sends PING command, gracefully handles "not configured" case
/// - Memory pressure - Checks if system memory usage exceeds 90%
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

    // Check WebSocket ticket service (if configured)
    // In cluster mode, memory-backed ticket storage causes cross-replica auth failures.
    let ws_ticket_status = state.ws_ticket_service.as_ref().map(|svc| {
        let is_cluster_mode = state.cluster_manager.is_some();
        let health = check_ws_ticket_health(svc);
        if is_cluster_mode && svc.backend_name() != "redis" {
            error_messages.push(
                "WsTicketService: memory mode is not safe in cluster mode (tickets created on one node cannot be validated on another)".to_string()
            );
            is_healthy = false;
            warn!("WsTicketService is using memory storage in cluster mode — cross-replica ticket validation will fail");
            "unhealthy (memory, cluster mode)".to_string()
        } else {
            health
        }
    });

    // Check email service (if configured) - validates SMTP config is present
    let email_status = state.email_service.as_ref().map(|svc| {
        check_email_health(svc)
    });

    // Check livestream infrastructure (if configured)
    let livestream_status = state.live_streaming_infrastructure.as_ref().map(|_| {
        "configured".to_string()
    });

    // Check memory pressure - high memory usage should mark node as unhealthy
    let memory_health = check_memory_health();
    if let Some(ref mem) = memory_health {
        if mem.status == "unhealthy" {
            error_messages.push(format!("Memory: usage at {:.1}% (threshold: {:.0}%)",
                mem.usage_percent, MEMORY_UNHEALTHY_THRESHOLD_PERCENT));
            is_healthy = false;
            warn!("Memory pressure detected: {:.1}% usage", mem.usage_percent);
        }
    }

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
            ws_ticket: ws_ticket_status,
            email: email_status,
            livestream: livestream_status,
            memory: memory_health,
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

/// Check cluster health by verifying the `ClusterManager` is operational.
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

/// Check Redis connectivity by sending a PING command with timeout.
///
/// Uses the shared raw Redis connection (`state.redis_conn`) rather than the
/// rate limiter's internal connection. This ensures the health check reports
/// the status of the application's Redis, not whether the rate limiter's
/// connection pool happens to be healthy. When the rate limiter is degraded
/// (e.g. using in-memory fallback due to a transient Redis error), the health
/// check will still correctly detect that Redis is unavailable.
///
/// Issue #41: In cluster mode, Redis is required. If Redis is not configured
/// and the node is in cluster mode, this returns an error (503).
/// In single-node mode, Redis is optional and "not configured" is OK.
async fn check_redis_health(state: &AppState) -> Result<(), String> {
    let is_cluster_mode = state.cluster_manager.is_some();

    // Resolve a fresh ConnectionManager clone from the shared RwLock.
    // Returns None when Redis is not configured.
    let redis_conn = state.resolve_redis_conn().await;

    let Some(mut conn) = redis_conn else {
        // Redis not configured.
        if is_cluster_mode {
            warn!("Redis not configured but cluster mode is active — node is not ready");
            return Err("Redis is required for cluster mode but is not configured".to_string());
        }
        // Single-node mode: Redis is optional
        return Ok(());
    };

    // Send a direct PING to verify the raw connection is responsive.
    match tokio::time::timeout(
        HEALTH_CHECK_TIMEOUT,
        redis::cmd("PING").query_async::<String>(&mut conn),
    )
    .await
    {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => {
            warn!("Redis health check failed: {}", e);
            Err(format!("Redis ping failed: {e}"))
        }
        Err(_) => {
            warn!("Redis health check timed out after {}s", HEALTH_CHECK_TIMEOUT.as_secs());
            Err(format!("Redis health check timed out after {}s", HEALTH_CHECK_TIMEOUT.as_secs()))
        }
    }
}

/// Check WebSocket ticket service health
///
/// Reports whether the service is using Redis-backed (multi-replica safe) or
/// memory-backed (single-replica only) storage.
fn check_ws_ticket_health(svc: &synctv_core::service::WsTicketService) -> String {
    format!("healthy ({})", svc.backend_name())
}

/// Check email service health
///
/// Validates that the email service has SMTP configuration. The service is
/// considered healthy if it is configured with valid SMTP settings; if no
/// config is provided the service is present but unconfigured (informational).
fn check_email_health(svc: &synctv_core::service::EmailService) -> String {
    if svc.is_configured() {
        "configured".to_string()
    } else {
        "not configured".to_string()
    }
}

/// Memory usage threshold percentage for marking the node as unhealthy.
/// When memory usage exceeds this threshold, the node should not receive
/// additional traffic to prevent OOM or performance degradation.
const MEMORY_UNHEALTHY_THRESHOLD_PERCENT: f64 = 90.0;

/// Check system memory health.
///
/// Returns memory usage information and health status.
/// When memory usage exceeds 90%, the status is "unhealthy".
/// Returns None if memory information cannot be obtained.
fn check_memory_health() -> Option<MemoryHealth> {
    // Use the `sysinfo` crate to get memory info
    // Note: We use a minimal approach here to avoid heavy dependencies
    #[cfg(target_os = "linux")]
    {
        check_memory_health_linux()
    }
    #[cfg(target_os = "macos")]
    {
        check_memory_health_macos()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn check_memory_health_linux() -> Option<MemoryHealth> {
    use std::fs;

    // Read /proc/meminfo for memory stats
    let meminfo = fs::read_to_string("/proc/meminfo").ok()?;
    let mut total_kb: u64 = 0;
    let mut available_kb: u64 = 0;

    for line in meminfo.lines() {
        if line.starts_with("MemTotal:") {
            total_kb = line
                .split(':')
                .nth(1)?
                .trim()
                .split_whitespace()
                .next()?
                .parse()
                .ok()?;
        } else if line.starts_with("MemAvailable:") {
            available_kb = line
                .split(':')
                .nth(1)?
                .trim()
                .split_whitespace()
                .next()?
                .parse()
                .ok()?;
        }
        if total_kb > 0 && available_kb > 0 {
            break;
        }
    }

    if total_kb == 0 {
        return None;
    }

    let used_kb = total_kb.saturating_sub(available_kb);
    let usage_percent = (used_kb as f64 / total_kb as f64) * 100.0;
    let status = if usage_percent > MEMORY_UNHEALTHY_THRESHOLD_PERCENT {
        "unhealthy"
    } else {
        "healthy"
    };

    Some(MemoryHealth {
        usage_percent: (usage_percent * 100.0).round() / 100.0, // Round to 2 decimal places
        status: status.to_string(),
    })
}

#[cfg(target_os = "macos")]
fn check_memory_health_macos() -> Option<MemoryHealth> {
    // On macOS, use vm_stat to get memory info
    // For simplicity, we use sysctl which is more portable
    use std::process::Command;

    // Get total memory in bytes
    let total_output = Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    let total_bytes: u64 = String::from_utf8_lossy(&total_output.stdout)
        .trim()
        .parse()
        .ok()?;

    // Get page size and free pages from vm_stat
    let vm_stat_output = Command::new("vm_stat").output().ok()?;
    let vm_stat = String::from_utf8_lossy(&vm_stat_output.stdout);

    let mut page_size: u64 = 4096; // Default page size
    let mut free_pages: u64 = 0;
    let mut inactive_pages: u64 = 0;

    for line in vm_stat.lines() {
        if line.contains("page size of") {
            // Extract page size from line like: "Mach Virtual Memory Statistics: (page size of 16384 bytes)"
            if let Some(start) = line.find("page size of ") {
                let rest = &line[start + 13..];
                if let Some(end) = rest.find(" bytes") {
                    if let Ok(ps) = rest[..end].parse::<u64>() {
                        page_size = ps;
                    }
                }
            }
        } else if line.starts_with("Pages free:") {
            // Extract number from "Pages free:      12345."
            let num_str: String = line
                .split(':')
                .nth(1)?
                .trim()
                .trim_end_matches('.')
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect();
            free_pages = num_str.parse().ok()?;
        } else if line.starts_with("Pages inactive:") {
            let num_str: String = line
                .split(':')
                .nth(1)?
                .trim()
                .trim_end_matches('.')
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect();
            inactive_pages = num_str.parse().ok()?;
        }
    }

    // On macOS, "free + inactive" is roughly equivalent to available memory
    let available_bytes = (free_pages + inactive_pages) * page_size;
    let used_bytes = total_bytes.saturating_sub(available_bytes);
    let usage_percent = (used_bytes as f64 / total_bytes as f64) * 100.0;

    let status = if usage_percent > MEMORY_UNHEALTHY_THRESHOLD_PERCENT {
        "unhealthy"
    } else {
        "healthy"
    };

    Some(MemoryHealth {
        usage_percent: (usage_percent * 100.0).round() / 100.0,
        status: status.to_string(),
    })
}

/// Prometheus metrics endpoint
///
/// When `server.metrics_bearer_token` is configured (non-empty), this endpoint
/// requires an `Authorization: Bearer <token>` header matching the configured
/// value. Requests without the correct token receive HTTP 401 Unauthorized.
///
/// When the token is empty (default), the endpoint is unauthenticated and
/// operators must ensure it is network-restricted (e.g. not exposed externally).
pub async fn prometheus_metrics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let expected_token = &state.config.server.metrics_bearer_token;

    if !expected_token.is_empty() {
        // Check Authorization: Bearer <token>
        let provided = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| {
                synctv_core::service::auth::JwtValidator::extract_bearer_token(s).ok()
            });

        match provided {
            Some(ref token) if token == expected_token => {
                // Token matches — proceed to return metrics
            }
            _ => {
                return (
                    StatusCode::UNAUTHORIZED,
                    [(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    "Unauthorized".to_string(),
                )
                    .into_response();
            }
        }
    }

    (
        [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        metrics::gather_metrics(),
    )
        .into_response()
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
                ws_ticket: None,
                email: None,
                livestream: None,
                memory: None,
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
        assert!(details.ws_ticket.is_none());
        assert!(details.email.is_none());
        assert!(details.livestream.is_none());
        assert!(details.memory.is_none());
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
                ws_ticket: None,
                email: None,
                livestream: None,
                memory: None,
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
                ws_ticket: None,
                email: None,
                livestream: None,
                memory: None,
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
                ws_ticket: None,
                email: None,
                livestream: None,
                memory: None,
                message: None,
            }),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(!json.contains("message"));
        assert!(!json.contains("cluster"));
        assert!(!json.contains("ws_ticket"));
        assert!(!json.contains("email"));
        assert!(!json.contains("livestream"));
        assert!(!json.contains("memory"));
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

    #[test]
    fn test_health_response_with_memory_health() {
        let response = HealthResponse {
            status: "healthy".to_string(),
            details: Some(HealthDetails {
                database: "healthy".to_string(),
                redis: "healthy".to_string(),
                cluster: None,
                ws_ticket: None,
                email: None,
                livestream: None,
                memory: Some(MemoryHealth {
                    usage_percent: 45.5,
                    status: "healthy".to_string(),
                }),
                message: None,
            }),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"memory\":{"));
        assert!(json.contains("\"usage_percent\":45.5"));
        assert!(json.contains("\"status\":\"healthy\""));
    }

    #[test]
    fn test_health_response_with_unhealthy_memory() {
        let response = HealthResponse {
            status: "unhealthy".to_string(),
            details: Some(HealthDetails {
                database: "healthy".to_string(),
                redis: "healthy".to_string(),
                cluster: None,
                ws_ticket: None,
                email: None,
                livestream: None,
                memory: Some(MemoryHealth {
                    usage_percent: 95.2,
                    status: "unhealthy".to_string(),
                }),
                message: Some("Memory: usage at 95.2% (threshold: 90%)".to_string()),
            }),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"usage_percent\":95.2"));
        assert!(json.contains("\"status\":\"unhealthy\""));
    }

    #[test]
    fn test_memory_health_status_below_threshold() {
        // Memory usage below 90% should be healthy
        let mem = MemoryHealth {
            usage_percent: 50.0,
            status: "healthy".to_string(),
        };
        assert_eq!(mem.status, "healthy");
        assert!(mem.usage_percent < MEMORY_UNHEALTHY_THRESHOLD_PERCENT);
    }

    #[test]
    fn test_memory_health_status_above_threshold() {
        // Memory usage above 90% should be unhealthy
        let mem = MemoryHealth {
            usage_percent: 95.0,
            status: "unhealthy".to_string(),
        };
        assert_eq!(mem.status, "unhealthy");
        assert!(mem.usage_percent > MEMORY_UNHEALTHY_THRESHOLD_PERCENT);
    }

    #[test]
    fn test_check_memory_health_returns_some() {
        // This test verifies that check_memory_health() works on the current platform
        let result = check_memory_health();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            // On Linux and macOS, we should get memory info
            assert!(result.is_some());
            let mem = result.unwrap();
            assert!(mem.usage_percent >= 0.0);
            assert!(mem.usage_percent <= 100.0);
            assert!(mem.status == "healthy" || mem.status == "unhealthy");
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            // On other platforms, memory check is not supported
            assert!(result.is_none());
        }
    }

    #[test]
    fn test_memory_threshold_constant() {
        // Verify the threshold is set at 90%
        assert_eq!(MEMORY_UNHEALTHY_THRESHOLD_PERCENT, 90.0);
    }
}
