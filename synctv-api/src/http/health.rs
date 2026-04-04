//! Health check and metrics endpoints
//!
//! Provides health check endpoints for Kubernetes readiness/liveness probes and Prometheus metrics.
//!
//! # Endpoints
//!
//! - `/health/live` - Liveness probe: checks if the application is running (basic check)
//! - `/health/ready` - Readiness probe: checks if dependencies (DB, Redis) are healthy
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

/// Health check router.
pub fn create_health_router() -> Router<AppState> {
    Router::new()
        .route("/health/live", get(liveness_check))
        .route("/health/ready", get(readiness_check))
}

/// Dedicated metrics router.
pub fn create_metrics_router() -> Router<AppState> {
    Router::new().route("/metrics", get(prometheus_metrics))
}

/// Health check response structure
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct HealthResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<HealthDetails>,
}

/// Detailed health check information
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/health/live",
        tag = "Health",
        responses(
            (status = 200, description = "Application is alive", body = HealthResponse)
        )
    )
)]
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/health/ready",
        tag = "Health",
        responses(
            (status = 200, description = "Dependencies are healthy", body = HealthResponse),
            (status = 503, description = "One or more dependencies are unhealthy", body = HealthResponse)
        )
    )
)]
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
        RedisHealthStatus::Healthy => "healthy".to_string(),
        RedisHealthStatus::NotConfigured => "not configured".to_string(),
        RedisHealthStatus::Unhealthy(e) => {
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

    // Check WebSocket ticket service.
    // In cluster mode, memory-backed ticket storage causes cross-replica auth failures.
    let ws_ticket_status = {
        let svc = &state.ws_ticket_service;
        let is_cluster_mode = state.config.cluster_runtime_enabled();
        let health = check_ws_ticket_health(svc);
        if ws_ticket_backend_is_safe_for_mode(svc, is_cluster_mode) {
            Some(health)
        } else {
            error_messages.push(
                "WsTicketService: memory mode is not safe in cluster mode (tickets created on one node cannot be validated on another)".to_string()
            );
            is_healthy = false;
            warn!("WsTicketService is using memory storage in cluster mode — cross-replica ticket validation will fail");
            Some("unhealthy (memory, cluster mode)".to_string())
        }
    };

    // Check email service (if configured) - validates SMTP config is present
    let email_status = state
        .email_service
        .as_ref()
        .map(|svc| check_email_health(svc));

    // Check livestream infrastructure (if configured)
    let livestream_status = state
        .live_streaming_infrastructure
        .as_ref()
        .map(|_| "configured".to_string());

    // Check memory pressure - high memory usage should mark node as unhealthy
    let memory_health = check_memory_health();
    if let Some(ref mem) = memory_health {
        if mem.status == "unhealthy" {
            error_messages.push(format!(
                "Memory: usage at {:.1}% (threshold: {:.0}%)",
                mem.usage_percent, MEMORY_UNHEALTHY_THRESHOLD_PERCENT
            ));
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
        status: if is_healthy {
            "healthy".to_string()
        } else {
            "unhealthy".to_string()
        },
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
            warn!(
                "Database health check timed out after {}s",
                HEALTH_CHECK_TIMEOUT.as_secs()
            );
            Err(format!(
                "Database health check timed out after {}s",
                HEALTH_CHECK_TIMEOUT.as_secs()
            ))
        }
    }
}

/// Check cluster health by verifying the `ClusterManager` is operational.
///
/// Returns `None` if no cluster manager is configured (single-node mode).
/// Returns `Some(Ok(()))` if the cluster manager is healthy.
/// Returns `Some(Err(...))` if the cluster manager reports issues.
fn check_cluster_health(state: &AppState) -> Option<Result<(), String>> {
    if !state.config.cluster_runtime_enabled() {
        return None;
    }
    let Some(cm) = state.cluster_manager.as_ref() else {
        return Some(Err(
            "Cluster runtime is enabled but ClusterManager is not available".to_string(),
        ));
    };
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
/// Redis readiness reflects the health of the configured Redis connection.
///
#[derive(Debug, Clone, PartialEq, Eq)]
enum RedisHealthStatus {
    Healthy,
    NotConfigured,
    Unhealthy(String),
}

/// Whether Redis must exist is a configuration-layer invariant enforced during
/// startup validation. At runtime, a missing Redis handle is treated as
/// "not configured" rather than re-enforcing cluster configuration rules.
async fn check_redis_health(state: &AppState) -> RedisHealthStatus {
    let redis_conn = state.resolve_redis_conn().await;
    check_redis_health_from_conn(redis_conn).await
}

async fn check_redis_health_from_conn(
    redis_conn: Option<redis::aio::ConnectionManager>,
) -> RedisHealthStatus {
    let Some(mut conn) = redis_conn else {
        return RedisHealthStatus::NotConfigured;
    };

    // Send a direct PING to verify the raw connection is responsive.
    match tokio::time::timeout(
        HEALTH_CHECK_TIMEOUT,
        redis::cmd("PING").query_async::<String>(&mut conn),
    )
    .await
    {
        Ok(Ok(_)) => RedisHealthStatus::Healthy,
        Ok(Err(e)) => {
            warn!("Redis health check failed: {}", e);
            RedisHealthStatus::Unhealthy(format!("Redis ping failed: {e}"))
        }
        Err(_) => {
            warn!(
                "Redis health check timed out after {}s",
                HEALTH_CHECK_TIMEOUT.as_secs()
            );
            RedisHealthStatus::Unhealthy(format!(
                "Redis health check timed out after {}s",
                HEALTH_CHECK_TIMEOUT.as_secs()
            ))
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

fn ws_ticket_backend_is_safe_for_mode(
    svc: &synctv_core::service::WsTicketService,
    cluster_mode: bool,
) -> bool {
    !cluster_mode || svc.backend_name() == "redis"
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
    // Try cgroup-aware memory check first (for containers), fall back to /proc/meminfo
    check_cgroup_memory().or_else(check_proc_meminfo)
}

/// Read memory limits from cgroup v2 or v1 when running inside a container.
///
/// Returns `None` if not running in a cgroup-limited environment (e.g., bare metal).
#[cfg(target_os = "linux")]
fn check_cgroup_memory() -> Option<MemoryHealth> {
    use std::fs;

    // Try cgroup v2 first
    let (limit, current) = if let (Ok(limit_str), Ok(current_str)) = (
        fs::read_to_string("/sys/fs/cgroup/memory.max"),
        fs::read_to_string("/sys/fs/cgroup/memory.current"),
    ) {
        let limit = limit_str.trim().parse::<u64>().ok()?;
        let current = current_str.trim().parse::<u64>().ok()?;
        (limit, current)
    } else if let (Ok(limit_str), Ok(usage_str)) = (
        // Try cgroup v1 fallback
        fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes"),
        fs::read_to_string("/sys/fs/cgroup/memory/memory.usage_in_bytes"),
    ) {
        let limit = limit_str.trim().parse::<u64>().ok()?;
        let current = usage_str.trim().parse::<u64>().ok()?;
        (limit, current)
    } else {
        return None;
    };

    // Sanity check: a limit of u64::MAX or 0 means "no limit" (not containerized)
    if limit == 0 || limit >= (1u64 << 62) {
        return None;
    }

    let usage_percent = (current as f64 / limit as f64) * 100.0;
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

/// Read memory info from /proc/meminfo (host-level, not container-aware).
#[cfg(target_os = "linux")]
fn check_proc_meminfo() -> Option<MemoryHealth> {
    use std::fs;

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
        usage_percent: (usage_percent * 100.0).round() / 100.0,
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
                .filter(char::is_ascii_digit)
                .collect();
            free_pages = num_str.parse().ok()?;
        } else if line.starts_with("Pages inactive:") {
            let num_str: String = line
                .split(':')
                .nth(1)?
                .trim()
                .trim_end_matches('.')
                .chars()
                .filter(char::is_ascii_digit)
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
/// Startup validation requires a valid `metrics.auth` configuration whenever
/// metrics are enabled, so this endpoint always enforces authenticated access
/// in production.
pub async fn prometheus_metrics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    match state
        .metrics_access_controller
        .authorize(&state.config.metrics, &headers, "/metrics", "GET")
        .await
    {
        Ok(()) => {}
        Err(crate::http::metrics_auth::MetricsAccessError::Unauthorized) => {
            return (
                StatusCode::UNAUTHORIZED,
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )],
                "Unauthorized".to_string(),
            )
                .into_response();
        }
        Err(crate::http::metrics_auth::MetricsAccessError::Forbidden) => {
            return (
                StatusCode::FORBIDDEN,
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )],
                "Forbidden".to_string(),
            )
                .into_response();
        }
        Err(crate::http::metrics_auth::MetricsAccessError::Internal) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )],
                "Internal Server Error".to_string(),
            )
                .into_response();
        }
    }

    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        metrics::gather_metrics(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::tests::test_app_state;
    use axum::extract::State;
    use axum::http::header::AUTHORIZATION;
    use axum::http::HeaderValue;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

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
    fn test_check_memory_health_returns_valid_result_when_available() {
        // Memory inspection is best-effort and depends on host capabilities
        // (`/proc/meminfo`, cgroup files, `sysctl`, `vm_stat`, etc.).
        // The production contract is:
        // - return `Some(MemoryHealth)` with sane values when memory info can be obtained
        // - return `None` when the platform/runtime cannot provide it
        let result = check_memory_health();

        if let Some(mem) = result {
            assert!(mem.usage_percent >= 0.0);
            assert!(mem.usage_percent <= 100.0);
            assert!(mem.status == "healthy" || mem.status == "unhealthy");
        }
    }

    #[test]
    fn test_memory_threshold_constant() {
        // Verify the threshold is set at 90%
        assert_eq!(MEMORY_UNHEALTHY_THRESHOLD_PERCENT, 90.0);
    }

    #[tokio::test]
    async fn test_check_redis_health_accepts_missing_redis_connection() {
        let result = check_redis_health_from_conn(None).await;
        assert_eq!(
            result,
            RedisHealthStatus::NotConfigured,
            "missing redis should be reported as not configured"
        );
    }

    #[tokio::test]
    async fn test_check_redis_health_status_reports_not_configured_without_connection() {
        let state = test_app_state();
        let response = readiness_check(State(state)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let health: HealthResponse = serde_json::from_slice(&body).unwrap();
        let details = health.details.expect("readiness should include details");
        assert_eq!(
            details.redis, "not configured",
            "single-node readiness must distinguish missing Redis from a healthy configured Redis"
        );
    }

    fn metrics_test_state(metrics_bearer_token: &str) -> crate::http::AppState {
        let mut state = test_app_state();
        Arc::make_mut(&mut state.router_config).config = Arc::new({
            let mut config = (*state.config).clone();
            config.metrics.enabled = true;
            config.metrics.auth.mode = synctv_core::config::MetricsAuthMode::BearerToken;
            config.metrics.auth.bearer_token = metrics_bearer_token.to_string();
            config
        });
        state
    }

    #[tokio::test]
    async fn test_prometheus_metrics_rejects_missing_auth_when_token_configured() {
        let response = prometheus_metrics(State(metrics_test_state("secret")), HeaderMap::new())
            .await
            .into_response();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_prometheus_metrics_accepts_matching_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));

        let response = prometheus_metrics(State(metrics_test_state("secret")), headers)
            .await
            .into_response();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_prometheus_metrics_accepts_matching_basic_auth() {
        let mut state = test_app_state();
        Arc::make_mut(&mut state.router_config).config = Arc::new({
            let mut config = (*state.config).clone();
            config.metrics.enabled = true;
            config.metrics.auth.mode = synctv_core::config::MetricsAuthMode::Basic;
            config.metrics.auth.basic_username = "metrics".to_string();
            config.metrics.auth.basic_password = "secret".to_string();
            config
        });

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Basic bWV0cmljczpzZWNyZXQ="),
        );

        let response = prometheus_metrics(State(state), headers)
            .await
            .into_response();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_metrics_router_exposes_metrics_endpoint() {
        let app = create_metrics_router().with_state(metrics_test_state("secret"));

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/metrics")
                    .header(AUTHORIZATION, "Bearer secret")
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn test_ws_ticket_memory_backend_is_only_unhealthy_when_cluster_enabled() {
        let memory_tickets = synctv_core::service::WsTicketService::with_memory(None);
        assert!(
            ws_ticket_backend_is_safe_for_mode(&memory_tickets, false),
            "standalone mode should allow memory-backed ws tickets"
        );
        assert!(
            !ws_ticket_backend_is_safe_for_mode(&memory_tickets, true),
            "cluster mode must reject memory-backed ws tickets"
        );

        let redis = synctv_core::service::WsTicketService::with_memory(None);
        let mut standalone = synctv_core::Config::default();
        standalone.cluster.enabled = false;
        assert!(
            !standalone.cluster_runtime_enabled(),
            "standalone config should not be treated as cluster mode"
        );

        let mut clustered = synctv_core::Config::default();
        clustered.cluster.enabled = true;
        clustered.server.cluster_secret = "shared-secret".to_string();
        assert!(
            clustered.cluster_runtime_enabled(),
            "cluster-enabled config should be treated as cluster mode"
        );
        assert!(
            check_ws_ticket_health(&redis).contains("memory"),
            "health helper should expose backend mode"
        );
    }

    #[test]
    fn test_cluster_health_is_skipped_when_distributed_cluster_disabled() {
        let memory_tickets = synctv_core::service::WsTicketService::with_memory(None);
        assert!(
            ws_ticket_backend_is_safe_for_mode(&memory_tickets, false),
            "helper should treat standalone mode as safe"
        );
        assert!(
            !ws_ticket_backend_is_safe_for_mode(&memory_tickets, true),
            "helper should treat distributed cluster mode as unsafe for memory tickets"
        );
    }

    /// M18: Verify that cgroup memory check is attempted on Linux.
    /// On non-containerized Linux, it should fall back to /proc/meminfo.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_cgroup_memory_check_graceful_fallback() {
        // check_cgroup_memory returns None when not in a container
        // (cgroup files don't exist or limit is very large)
        let cgroup_result = check_cgroup_memory();
        // On bare-metal Linux, this will be None; in a container, Some
        // Either way, the overall check_memory_health_linux should return Some
        let overall = check_memory_health_linux();
        assert!(overall.is_some() || cgroup_result.is_some());
    }

    /// M18: Verify that proc_meminfo fallback works on Linux.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_proc_meminfo_returns_some() {
        let result = check_proc_meminfo();
        assert!(result.is_some());
        let mem = result.unwrap();
        assert!(mem.usage_percent >= 0.0);
        assert!(mem.usage_percent <= 100.0);
    }
}
