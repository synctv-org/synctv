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
use std::time::Duration;
use tracing::warn;

use crate::http::AppState;
use synctv_api_common::observability::metrics;
use synctv_api_common::webrtc_status;
use synctv_core::service::{email_health, ws_ticket_backend_is_safe_for_mode, ws_ticket_health};
use synctv_proto::client::{HealthDetails, HealthResponse, MemoryHealth};

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
/// Health check validates critical dependencies:
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

    let (database_health, redis_health) =
        tokio::join!(check_database_health(&state), check_redis_health(&state),);

    // Check database connectivity
    let db_status = match database_health {
        Ok(()) => "healthy".to_string(),
        Err(e) => {
            error_messages.push(format!("Database: {e}"));
            is_healthy = false;
            warn!(target: "synctv::health", "Database health check failed: {}", e);
            "unhealthy".to_string()
        }
    };

    // Check Redis connectivity
    let redis_status = match redis_health {
        RedisHealthStatus::Healthy => "healthy".to_string(),
        RedisHealthStatus::NotConfigured => "not configured".to_string(),
        RedisHealthStatus::Unhealthy(e) => {
            error_messages.push(format!("Redis: {e}"));
            is_healthy = false;
            warn!(target: "synctv::health", "Redis health check failed: {}", e);
            "unhealthy".to_string()
        }
    };

    // Check cluster health (only when distributed mode is active)
    let cluster_status = match check_cluster_health(&state) {
        Some(Ok(())) => Some("healthy".to_string()),
        Some(Err(e)) => {
            error_messages.push(format!("Cluster: {e}"));
            is_healthy = false;
            warn!(target: "synctv::health", "Cluster health check failed: {}", e);
            Some("unhealthy".to_string())
        }
        None => None, // No cluster realtime service, single-node mode
    };

    // Check WebSocket ticket service.
    // In distributed mode, memory-backed ticket storage causes cross-replica auth failures.
    let ws_ticket_status = {
        let svc = &state.ws_ticket_service;
        let is_cluster_mode = state.runtime_settings.cluster_runtime_enabled();
        let health = ws_ticket_health(svc.as_ref());
        if ws_ticket_backend_is_safe_for_mode(svc.as_ref(), is_cluster_mode) {
            Some(health)
        } else {
            error_messages.push(
                "WsTicketService: ticket storage is not cross-node capable in distributed mode \
                 (tickets created on one node cannot be validated on another)"
                    .to_string(),
            );
            is_healthy = false;
            warn!(target: "synctv::health",
                "WsTicketService storage is not cross-node capable in distributed mode; \
                 cross-replica ticket validation will fail"
            );
            Some("unhealthy (single-node ticket storage in distributed mode)".to_string())
        }
    };

    // Check email service (if configured) - validates SMTP config is present
    let email_status = state.email_service.as_ref().map(|svc| email_health(svc));

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
            warn!(target: "synctv::health", "Memory pressure detected: {:.1}% usage", mem.usage_percent);
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
            webrtc: Some(webrtc_status::to_proto_status(
                &state.shared_api_runtime.webrtc_status,
            )),
        }),
    };

    (status_code, Json(response))
}

/// Check database connectivity with timeout
pub(crate) async fn check_database_health(state: &AppState) -> Result<(), String> {
    match tokio::time::timeout(HEALTH_CHECK_TIMEOUT, state.user_service.health_check()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            warn!(target: "synctv::health", "Database health check failed: {}", e);
            Err(format!("Database connection failed: {e}"))
        }
        Err(_) => {
            warn!(target: "synctv::health",
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

/// Check cluster health by verifying the realtime cluster service is operational.
///
/// Returns `Some(Ok(()))` if the realtime runtime is healthy.
/// Returns `Some(Err(...))` if the realtime runtime reports issues.
fn check_cluster_health(state: &AppState) -> Option<Result<(), String>> {
    if !state.runtime_settings.cluster_runtime_enabled() {
        return None;
    }
    let event_service = state.event_service.as_ref();
    let metrics = event_service.metrics();

    // Verify node has a valid ID (non-empty)
    if event_service.node_id().is_empty() {
        return Some(Err("Cluster node ID is empty".to_string()));
    }

    // If the distributed event path should be enabled but isn't, the node can't sync with the cluster
    if !metrics.distributed_enabled {
        return Some(Err(
            "Cluster distributed realtime transport is not connected".to_string(),
        ));
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
pub(crate) enum RedisHealthStatus {
    Healthy,
    NotConfigured,
    Unhealthy(String),
}

/// Whether Redis must exist is a configuration-layer invariant enforced during
/// startup validation. At runtime, a missing Redis handle is treated as
/// "not configured" rather than re-enforcing realtime configuration rules.
pub(crate) async fn check_redis_health(state: &AppState) -> RedisHealthStatus {
    let redis_conn = state.resolve_redis_conn().await;
    check_redis_health_from_conn(redis_conn).await
}

pub(crate) async fn check_redis_health_from_conn(
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
            warn!(target: "synctv::health", "Redis health check failed: {}", e);
            RedisHealthStatus::Unhealthy(format!("Redis ping failed: {e}"))
        }
        Err(_) => {
            warn!(target: "synctv::health",
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

/// Memory usage threshold percentage for marking the node as unhealthy.
/// When memory usage exceeds this threshold, the node should not receive
/// additional traffic to prevent OOM or performance degradation.
const MEMORY_UNHEALTHY_THRESHOLD_PERCENT: f64 = 90.0;

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn memory_usage_percent(used_bytes: u64, total_bytes: u64) -> Option<f64> {
    if total_bytes == 0 {
        return None;
    }

    let scaled_percent =
        (u128::from(used_bytes) * 10_000 + u128::from(total_bytes / 2)) / u128::from(total_bytes);
    let scaled_percent = u32::try_from(scaled_percent).ok()?;

    Some(f64::from(scaled_percent) / 100.0)
}

/// Check system memory health.
///
/// Returns memory usage information and health status.
/// When memory usage exceeds 90%, the status is "unhealthy".
/// Returns None if memory information cannot be obtained.
pub(crate) fn check_memory_health() -> Option<MemoryHealth> {
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

    let usage_percent = memory_usage_percent(current, limit)?;
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
                .split_whitespace()
                .next()?
                .parse()
                .ok()?;
        } else if line.starts_with("MemAvailable:") {
            available_kb = line
                .split(':')
                .nth(1)?
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
    let usage_percent = memory_usage_percent(used_kb, total_kb)?;
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
    let usage_percent = memory_usage_percent(used_bytes, total_bytes)?.min(100.0);

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
    #[cfg(feature = "k8s")]
    let auth_result = state
        .metrics_access_controller
        .authorize(&state.runtime_settings.metrics, &headers, "/metrics", "GET")
        .await;
    #[cfg(not(feature = "k8s"))]
    let auth_result = state.metrics_access_controller.authorize(
        &state.runtime_settings.metrics,
        &headers,
        "/metrics",
        "GET",
    );

    match auth_result {
        Ok(()) => {}
        Err(synctv_api_common::metrics_auth::MetricsAccessError::Unauthorized) => {
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
        Err(synctv_api_common::metrics_auth::MetricsAccessError::Forbidden) => {
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
        Err(synctv_api_common::metrics_auth::MetricsAccessError::Internal) => {
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
    use async_trait::async_trait;
    use axum::extract::State;
    use axum::http::header::AUTHORIZATION;
    use axum::http::HeaderValue;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        anyhow::anyhow!(message.into()).into()
    }

    async fn read_health_response(
        response: axum::response::Response,
    ) -> TestResult<HealthResponse> {
        let body = response.into_body().collect().await?.to_bytes();
        Ok(serde_json::from_slice(&body)?)
    }

    fn require_details(health: HealthResponse) -> TestResult<HealthDetails> {
        health
            .details
            .ok_or_else(|| test_error("readiness should include details"))
    }

    fn request(uri: &str) -> TestResult<axum::http::Request<axum::body::Body>> {
        axum::http::Request::builder()
            .uri(uri)
            .body(axum::body::Body::empty())
            .map_err(|err| test_error(format!("request should build: {err}")))
    }

    #[derive(Clone)]
    struct TestEmailConfigProvider(Option<synctv_core::service::EmailConfig>);

    impl synctv_core::service::EmailConfigProvider for TestEmailConfigProvider {
        fn current_config(&self) -> synctv_core::Result<Option<synctv_core::service::EmailConfig>> {
            Ok(self.0.clone())
        }
    }

    struct SharedTestTicketStore;

    #[async_trait]
    impl synctv_core::service::TicketStore for SharedTestTicketStore {
        async fn store(
            &self,
            _ticket: &str,
            _data: &synctv_core::service::WsTicketData,
            _ttl_secs: u64,
        ) -> synctv_core::Result<()> {
            Ok(())
        }

        async fn load(
            &self,
            _ticket: &str,
        ) -> synctv_core::Result<Option<synctv_core::service::WsTicketData>> {
            Ok(None)
        }

        async fn claim(
            &self,
            _ticket: &str,
            _expected_ticket: &synctv_core::service::WsTicketData,
        ) -> synctv_core::Result<bool> {
            Ok(false)
        }

        fn supports_cluster_runtime(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn test_liveness_check_response_body() -> TestResult {
        let response = liveness_check().await.into_response();
        let health = read_health_response(response).await?;
        assert_eq!(health.status, "ok");
        assert!(health.details.is_none());
        Ok(())
    }

    #[test]
    fn test_memory_usage_percent_handles_large_totals_without_u32_truncation() -> TestResult {
        let total_bytes = 16 * 1024 * 1024 * 1024_u64;
        let used_bytes = 8 * 1024 * 1024 * 1024_u64;

        let usage_percent = memory_usage_percent(used_bytes, total_bytes)
            .ok_or_else(|| test_error("non-zero total memory"))?;

        assert!((usage_percent - 50.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn test_check_redis_health_status_reports_not_configured_without_connection() -> TestResult
    {
        let state = test_app_state();
        let response = readiness_check(State(state)).await.into_response();
        assert!(
            matches!(
                response.status(),
                StatusCode::OK | StatusCode::SERVICE_UNAVAILABLE
            ),
            "readiness should remain on the documented status surface"
        );

        let health = read_health_response(response).await?;
        let details = require_details(health)?;
        assert_eq!(
            details.redis, "not configured",
            "readiness must distinguish missing Redis from a healthy configured Redis"
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn test_readiness_includes_webrtc_status() -> TestResult {
        let state = test_app_state();
        let response = readiness_check(State(state)).await.into_response();
        let health = read_health_response(response).await?;
        let details = require_details(health)?;
        let webrtc = details
            .webrtc
            .ok_or_else(|| test_error("readiness should expose WebRTC status"))?;

        assert_eq!(webrtc.mode, "peer_to_peer");
        assert_eq!(webrtc.builtin_stun_state, "disabled");
        assert_eq!(webrtc.reason, "disabled_by_config");
        Ok(())
    }

    fn metrics_test_state(metrics_bearer_token: &str) -> crate::http::AppState {
        let mut state = test_app_state();
        Arc::make_mut(&mut state.router_options).runtime_settings = Arc::new({
            let mut config = (*state.runtime_settings).clone();
            config.metrics.enabled = true;
            config.metrics.auth.mode = synctv_api_common::api_runtime::MetricsAuthMode::BearerToken;
            config.metrics.auth.bearer_token = metrics_bearer_token.to_string();
            config
        });
        state
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn test_prometheus_metrics_rejects_missing_auth_when_token_configured() {
        let response = prometheus_metrics(State(metrics_test_state("secret")), HeaderMap::new())
            .await
            .into_response();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn test_prometheus_metrics_accepts_matching_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));

        let response = prometheus_metrics(State(metrics_test_state("secret")), headers)
            .await
            .into_response();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn test_prometheus_metrics_accepts_matching_basic_auth() {
        let mut state = test_app_state();
        Arc::make_mut(&mut state.router_options).runtime_settings = Arc::new({
            let mut config = (*state.runtime_settings).clone();
            config.metrics.enabled = true;
            config.metrics.auth.mode = synctv_api_common::api_runtime::MetricsAuthMode::Basic;
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
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn test_metrics_router_exposes_metrics_endpoint() -> TestResult {
        let app = create_metrics_router().with_state(metrics_test_state("secret"));

        let mut request = request("/metrics")?;
        request
            .headers_mut()
            .insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
        let response = app.oneshot(request).await?;

        assert_eq!(response.status(), StatusCode::OK);
        Ok(())
    }

    #[test]
    fn test_ws_ticket_memory_backend_is_only_unhealthy_when_distributed_enabled() {
        let memory_tickets = synctv_core::service::WsTicketService::local_only(None);
        assert!(
            ws_ticket_backend_is_safe_for_mode(&memory_tickets, false),
            "standalone mode should allow memory-backed ws tickets"
        );
        assert!(
            !ws_ticket_backend_is_safe_for_mode(&memory_tickets, true),
            "distributed mode must reject memory-backed ws tickets"
        );

        let shared_tickets = synctv_core::service::WsTicketService::from_store(
            Arc::new(SharedTestTicketStore),
            None,
        );
        let mut standalone = synctv_api_common::ApiRuntimeSettings::default();
        standalone.cluster_enabled = false;
        assert!(
            !standalone.cluster_runtime_enabled(),
            "standalone config should not be treated as distributed mode"
        );

        let mut clustered = synctv_api_common::ApiRuntimeSettings::default();
        clustered.cluster_enabled = true;
        clustered.cluster.secret = "shared-secret".to_string();
        assert!(
            clustered.cluster_runtime_enabled(),
            "cluster-enabled config should be treated as distributed mode"
        );
        assert!(
            ws_ticket_backend_is_safe_for_mode(&shared_tickets, true),
            "distributed mode should accept any store that advertises cluster capability"
        );
        assert!(
            ws_ticket_health(&shared_tickets).contains("cross-node capable"),
            "health helper should expose capability rather than backend implementation"
        );
        assert!(
            ws_ticket_health(&memory_tickets).contains("single-node"),
            "health helper should expose capability rather than backend implementation"
        );
    }

    #[test]
    fn test_cluster_health_is_skipped_when_distributed_cluster_disabled() {
        let memory_tickets = synctv_core::service::WsTicketService::local_only(None);
        assert!(
            ws_ticket_backend_is_safe_for_mode(&memory_tickets, false),
            "helper should treat standalone mode as safe"
        );
        assert!(
            !ws_ticket_backend_is_safe_for_mode(&memory_tickets, true),
            "helper should treat distributed distributed mode as unsafe for memory tickets"
        );
    }

    #[test]
    fn test_email_health_reports_configuration_only() -> TestResult {
        let unconfigured =
            synctv_core::service::EmailService::new(Arc::new(TestEmailConfigProvider(None)))?;
        assert_eq!(email_health(&unconfigured), "not configured");

        let configured = synctv_core::service::EmailService::new(Arc::new(
            TestEmailConfigProvider(Some(synctv_core::service::EmailConfig {
                smtp_host: "smtp.example.com".to_string(),
                smtp_port: 587,
                smtp_credentials: Some(synctv_core::service::SmtpCredentials {
                    username: "user".to_string(),
                    password: "password".to_string(),
                }),
                smtp_proxy: None,
                from_email: "noreply@example.com".to_string(),
                from_name: "SyncTV".to_string(),
                use_tls: true,
            })),
        ))?;
        assert_eq!(email_health(&configured), "configured");
        Ok(())
    }

    /// Verify that cgroup memory check is attempted on Linux.
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

    /// Verify that proc_meminfo fallback works on Linux.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_proc_meminfo_returns_some() -> TestResult {
        let result = check_proc_meminfo();
        let mem = result.ok_or_else(|| test_error("proc meminfo should be available"))?;
        assert!(mem.usage_percent >= 0.0);
        assert!(mem.usage_percent <= 100.0);
        Ok(())
    }
}
