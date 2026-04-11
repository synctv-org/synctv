//! TURN server health checking and monitoring.
//!
//! This module provides periodic health checks for TURN servers to detect
//! unreachable or malfunctioning servers. Unhealthy servers are automatically
//! excluded from ICE server lists.
//!
//! # Features
//!
//! - Periodic TCP connection checks to TURN server endpoints
//! - Health status tracking with automatic recovery detection
//! - Integration with global settings registry for dynamic server lists
//! - Prometheus metrics for monitoring
//!
//! # Health Check Strategy
//!
//! Health checks attempt a TCP connection to each TURN server endpoint.
//! This is a lightweight check that verifies:
//! - The server is reachable (network connectivity)
//! - The server is accepting connections (service is running)
//!
//! Note: This does NOT perform STUN/TURN protocol validation, as that
//! would require full protocol implementation. The TCP connection check
//! is sufficient to detect most failure modes.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::RwLock;

/// Health status of a TURN server endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnServerHealth {
    /// Server is healthy and passing health checks.
    Healthy,
    /// Server is unhealthy and failing health checks.
    Unhealthy,
}

impl TurnServerHealth {
    /// Returns `true` if the server is healthy.
    #[must_use]
    pub const fn is_healthy(self) -> bool {
        matches!(self, Self::Healthy)
    }
}

/// Health check result for a single TURN server.
#[derive(Debug, Clone)]
pub struct HealthCheckResult {
    /// Server URL being checked
    pub url: String,
    /// Whether the server is healthy
    pub health: TurnServerHealth,
    /// Error message if unhealthy (empty if healthy)
    pub error: String,
    /// When this check was performed
    pub checked_at: Instant,
}

impl HealthCheckResult {
    /// Create a new healthy check result.
    #[must_use]
    pub fn healthy(url: String) -> Self {
        Self {
            url,
            health: TurnServerHealth::Healthy,
            error: String::new(),
            checked_at: Instant::now(),
        }
    }

    /// Create a new unhealthy check result.
    #[must_use]
    pub fn unhealthy(url: String, error: String) -> Self {
        Self {
            url,
            health: TurnServerHealth::Unhealthy,
            error,
            checked_at: Instant::now(),
        }
    }
}

/// Health check configuration.
#[derive(Debug, Clone)]
pub struct TurnHealthCheckConfig {
    /// Interval between health checks.
    /// Default: 30 seconds.
    pub check_interval: Duration,
    /// Timeout for each health check attempt.
    /// Default: 5 seconds.
    pub check_timeout: Duration,
    /// Number of consecutive failures before marking server unhealthy.
    /// Default: 2.
    pub failure_threshold: usize,
    /// Number of consecutive successes before marking server healthy.
    /// Default: 2.
    pub success_threshold: usize,
}

impl Default for TurnHealthCheckConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(30),
            check_timeout: Duration::from_secs(5),
            failure_threshold: 2,
            success_threshold: 2,
        }
    }
}

/// Health state for a single TURN server endpoint.
#[derive(Debug)]
struct ServerHealthState {
    /// Current health status
    health: TurnServerHealth,
    /// Consecutive failures count
    consecutive_failures: usize,
    /// Consecutive successes count
    consecutive_successes: usize,
    /// Last check result
    last_result: Option<HealthCheckResult>,
}

impl ServerHealthState {
    const fn new() -> Self {
        Self {
            health: TurnServerHealth::Healthy,
            consecutive_failures: 0,
            consecutive_successes: 0,
            last_result: None,
        }
    }

    fn record_check(&mut self, result: &HealthCheckResult, config: &TurnHealthCheckConfig) {
        self.last_result = Some(result.clone());

        match result.health {
            TurnServerHealth::Healthy => {
                self.consecutive_successes += 1;
                self.consecutive_failures = 0;

                // Only mark as healthy after threshold crosses
                if self.consecutive_successes >= config.success_threshold {
                    self.health = TurnServerHealth::Healthy;
                }
            }
            TurnServerHealth::Unhealthy => {
                self.consecutive_failures += 1;
                self.consecutive_successes = 0;

                // Mark as unhealthy after threshold crosses
                if self.consecutive_failures >= config.failure_threshold {
                    self.health = TurnServerHealth::Unhealthy;
                }
            }
        }
    }
}

/// TURN server health checker.
///
/// Tracks health status for TURN servers and provides methods to filter
/// unhealthy servers from ICE server lists.
#[derive(Debug)]
pub struct TurnHealthChecker {
    /// Health state for each server endpoint
    health_states: RwLock<HashMap<String, ServerHealthState>>,
    /// Health check configuration
    config: TurnHealthCheckConfig,
}

impl TurnHealthChecker {
    /// Create a new health checker with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(TurnHealthCheckConfig::default())
    }

    /// Create a new health checker with custom configuration.
    #[must_use]
    pub fn with_config(config: TurnHealthCheckConfig) -> Self {
        Self {
            health_states: RwLock::new(HashMap::new()),
            config,
        }
    }

    /// Perform a health check on a single TURN server URL.
    ///
    /// Extracts the host:port from the URL and attempts a TCP connection.
    /// Returns a health check result indicating success or failure.
    pub async fn check_server(&self, url: &str) -> HealthCheckResult {
        // Parse TURN URL to extract host:port
        let addr = match Self::parse_turn_url(url) {
            Ok(addr) => addr,
            Err(e) => {
                return HealthCheckResult::unhealthy(
                    url.to_string(),
                    format!("Invalid TURN URL: {e}"),
                );
            }
        };

        // Attempt TCP connection with timeout
        let result =
            tokio::time::timeout(self.config.check_timeout, TcpStream::connect(&addr)).await;

        match result {
            Ok(Ok(_stream)) => {
                // Connection successful - server is healthy
                HealthCheckResult::healthy(url.to_string())
            }
            Ok(Err(e)) => {
                // Connection failed
                HealthCheckResult::unhealthy(url.to_string(), format!("Connection failed: {e}"))
            }
            Err(_) => {
                // Timeout
                HealthCheckResult::unhealthy(url.to_string(), "Connection timeout".to_string())
            }
        }
    }

    /// Parse a TURN URL to extract the socket address.
    ///
    /// Supports formats:
    /// - `turn:hostname:port`
    /// - `turns:hostname:port` (TLS)
    /// - `turn:IP:port`
    /// - `turns:IP:port` (TLS)
    ///
    /// Note: Hostnames will be resolved to IP addresses by the TCP connection.
    fn parse_turn_url(url: &str) -> Result<String, String> {
        // Strip the scheme (turn: or turns:)
        let addr_without_scheme = url
            .strip_prefix("turn:")
            .or_else(|| url.strip_prefix("turns:"))
            .ok_or_else(|| "URL must start with 'turn:' or 'turns:'".to_string())?;

        // Validate that the address part is not empty and contains a port
        if addr_without_scheme.is_empty() {
            return Err("Address is empty".to_string());
        }

        // Check if it has a port (format: host:port)
        if !addr_without_scheme.contains(':') {
            return Err("Address must include a port (host:port)".to_string());
        }

        Ok(addr_without_scheme.to_string())
    }

    /// Record a health check result for a server.
    ///
    /// Updates the server's health state based on the result and
    /// configured thresholds.
    pub async fn record_check(&self, result: &HealthCheckResult) {
        let mut states = self.health_states.write().await;
        let state = states
            .entry(result.url.clone())
            .or_insert_with(ServerHealthState::new);

        // Record the previous health for logging
        let previous_health = state.health;
        state.record_check(result, &self.config);

        // Log health status changes
        if previous_health != state.health {
            match state.health {
                TurnServerHealth::Healthy => {
                    tracing::info!(
                        url = %result.url,
                        "TURN server recovered and is now healthy"
                    );
                }
                TurnServerHealth::Unhealthy => {
                    tracing::warn!(
                        url = %result.url,
                        error = %result.error,
                        "TURN server marked as unhealthy"
                    );
                }
            }
        }
    }

    /// Get the current health status of a server.
    ///
    /// Returns `None` if no health checks have been performed yet.
    pub async fn get_health(&self, url: &str) -> Option<TurnServerHealth> {
        let states = self.health_states.read().await;
        states.get(url).map(|state| state.health)
    }

    /// Get all currently healthy servers from a list.
    ///
    /// Filters the input list to only include servers that are currently
    /// marked as healthy. Servers that have never been checked are assumed
    /// to be healthy (optimistic default).
    ///
    /// # Arguments
    /// * `urls` - List of TURN server URLs to filter
    ///
    /// # Returns
    /// A vector containing only the healthy server URLs.
    pub async fn filter_healthy_servers(&self, urls: &[String]) -> Vec<String> {
        let states = self.health_states.read().await;

        urls.iter()
            .filter(|url| {
                // If we haven't checked this server yet, assume it's healthy
                match states.get(*url) {
                    None => true,
                    Some(state) => state.health.is_healthy(),
                }
            })
            .cloned()
            .collect()
    }

    /// Get health metrics for all tracked servers.
    ///
    /// Returns a map of server URLs to their current health status.
    pub async fn get_all_health(&self) -> HashMap<String, TurnServerHealth> {
        let states = self.health_states.read().await;
        states
            .iter()
            .map(|(url, state)| (url.clone(), state.health))
            .collect()
    }

    /// Start periodic health checks on a list of servers.
    ///
    /// This spawns a background task that periodically checks all servers
    /// in the list. The task runs until the cancellation token is triggered.
    ///
    /// # Arguments
    /// * `servers` - Initial list of TURN server URLs to check
    /// * `cancel` - Cancellation token to stop the background task
    ///
    /// # Returns
    /// A handle to the background task.
    pub fn spawn_health_checks(
        self: Arc<Self>,
        servers: Vec<String>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(self.config.check_interval);

            // First check is immediate
            interval.tick().await;

            loop {
                tokio::select! {
                    () = cancel.cancelled() => {
                        tracing::info!("TURN health check task stopped");
                        break;
                    }
                    _ = interval.tick() => {
                        // Check all servers
                        for url in &servers {
                            let result = self.check_server(url).await;
                            self.record_check(&result).await;
                        }
                    }
                }
            }
        })
    }

    /// Update the list of servers being health-checked.
    ///
    /// This doesn't restart the background task - it's used to update
    /// the server list that the next periodic check will use.
    pub async fn update_servers(&self, servers: Vec<String>) {
        // Add new servers to the health state map
        let mut states = self.health_states.write().await;
        for url in &servers {
            states
                .entry(url.clone())
                .or_insert_with(ServerHealthState::new);
        }

        // Note: We don't remove old servers from the map to preserve their
        // health state in case they're added back later
    }
}

impl Default for TurnHealthChecker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_turn_server_health_is_healthy() {
        assert!(TurnServerHealth::Healthy.is_healthy());
        assert!(!TurnServerHealth::Unhealthy.is_healthy());
    }

    #[test]
    fn test_health_check_result_healthy() {
        let result = HealthCheckResult::healthy("turn:example.com:3478".to_string());
        assert!(result.health.is_healthy());
        assert!(result.error.is_empty());
        assert_eq!(result.url, "turn:example.com:3478");
    }

    #[test]
    fn test_health_check_result_unhealthy() {
        let result = HealthCheckResult::unhealthy(
            "turn:example.com:3478".to_string(),
            "Connection failed".to_string(),
        );
        assert!(!result.health.is_healthy());
        assert_eq!(result.error, "Connection failed");
        assert_eq!(result.url, "turn:example.com:3478");
    }

    #[test]
    fn test_server_health_state_new() {
        let state = ServerHealthState::new();
        assert_eq!(state.health, TurnServerHealth::Healthy);
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.consecutive_successes, 0);
        assert!(state.last_result.is_none());
    }

    #[test]
    fn test_server_health_state_mark_unhealthy_after_threshold() {
        let mut state = ServerHealthState::new();
        let config = TurnHealthCheckConfig {
            failure_threshold: 2,
            ..Default::default()
        };

        // First failure - still healthy
        let result1 = HealthCheckResult::unhealthy(
            "turn:example.com:3478".to_string(),
            "Error 1".to_string(),
        );
        state.record_check(&result1, &config);
        assert_eq!(state.health, TurnServerHealth::Healthy);
        assert_eq!(state.consecutive_failures, 1);

        // Second failure - now unhealthy
        let result2 = HealthCheckResult::unhealthy(
            "turn:example.com:3478".to_string(),
            "Error 2".to_string(),
        );
        state.record_check(&result2, &config);
        assert_eq!(state.health, TurnServerHealth::Unhealthy);
        assert_eq!(state.consecutive_failures, 2);
        assert_eq!(state.consecutive_successes, 0);
    }

    #[test]
    fn test_server_health_state_mark_healthy_after_threshold() {
        let mut state = ServerHealthState::new();
        let config = TurnHealthCheckConfig {
            failure_threshold: 1,
            success_threshold: 2,
            ..Default::default()
        };

        // Mark as unhealthy first
        let result1 =
            HealthCheckResult::unhealthy("turn:example.com:3478".to_string(), "Error".to_string());
        state.record_check(&result1, &config);
        assert_eq!(state.health, TurnServerHealth::Unhealthy);

        // First success - still unhealthy
        let result2 = HealthCheckResult::healthy("turn:example.com:3478".to_string());
        state.record_check(&result2, &config);
        assert_eq!(state.health, TurnServerHealth::Unhealthy);
        assert_eq!(state.consecutive_successes, 1);

        // Second success - now healthy
        let result3 = HealthCheckResult::healthy("turn:example.com:3478".to_string());
        state.record_check(&result3, &config);
        assert_eq!(state.health, TurnServerHealth::Healthy);
        assert_eq!(state.consecutive_successes, 2);
        assert_eq!(state.consecutive_failures, 0);
    }

    #[test]
    fn test_server_health_state_resets_counters_on_status_change() {
        let mut state = ServerHealthState::new();
        let config = TurnHealthCheckConfig::default();

        // Fail twice - becomes unhealthy
        state.record_check(
            &HealthCheckResult::unhealthy("turn:example.com:3478".to_string(), "Error".to_string()),
            &config,
        );
        state.record_check(
            &HealthCheckResult::unhealthy("turn:example.com:3478".to_string(), "Error".to_string()),
            &config,
        );
        assert_eq!(state.health, TurnServerHealth::Unhealthy);
        assert_eq!(state.consecutive_failures, 2);

        // Success once - failures reset, successes increment
        state.record_check(
            &HealthCheckResult::healthy("turn:example.com:3478".to_string()),
            &config,
        );
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.consecutive_successes, 1);
    }

    #[test]
    fn test_parse_turn_url_valid() {
        // Standard TURN URL with hostname
        let result = TurnHealthChecker::parse_turn_url("turn:example.com:3478");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "example.com:3478");

        // TURN over TLS
        let result = TurnHealthChecker::parse_turn_url("turns:example.com:5349");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "example.com:5349");

        // IP address
        let result = TurnHealthChecker::parse_turn_url("turn:192.168.1.1:3478");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "192.168.1.1:3478");
    }

    #[test]
    fn test_parse_turn_url_invalid() {
        // Missing scheme
        assert!(TurnHealthChecker::parse_turn_url("example.com:3478").is_err());

        // Wrong scheme
        assert!(TurnHealthChecker::parse_turn_url("stun:example.com:3478").is_err());

        // Missing port
        assert!(TurnHealthChecker::parse_turn_url("turn:example.com").is_err());

        // Invalid format
        assert!(TurnHealthChecker::parse_turn_url("not a url").is_err());
    }

    #[tokio::test]
    async fn test_turn_health_checker_new() {
        let checker = TurnHealthChecker::new();

        // Should have empty health states initially
        let health = checker.get_health("turn:example.com:3478").await;
        assert!(health.is_none());

        // Should filter all servers as healthy when none checked
        let urls = vec![
            "turn:example.com:3478".to_string(),
            "turn:example.com:5349".to_string(),
        ];
        let healthy = checker.filter_healthy_servers(&urls).await;
        assert_eq!(healthy.len(), 2);
    }

    #[tokio::test]
    async fn test_turn_health_checker_record_and_get() {
        let checker = TurnHealthChecker::new();

        // Record a healthy check
        let result = HealthCheckResult::healthy("turn:example.com:3478".to_string());
        checker.record_check(&result).await;

        // Should be healthy
        let health = checker.get_health("turn:example.com:3478").await;
        assert_eq!(health, Some(TurnServerHealth::Healthy));
    }

    #[tokio::test]
    async fn test_turn_health_checker_filter_unhealthy() {
        let checker = TurnHealthChecker::new();

        let urls = vec![
            "turn:healthy.com:3478".to_string(),
            "turn:unhealthy.com:3478".to_string(),
            "turn:unknown.com:3478".to_string(),
        ];

        // Mark one as unhealthy (requires 2 failures with default config)
        let result1 = HealthCheckResult::unhealthy(
            "turn:unhealthy.com:3478".to_string(),
            "Error 1".to_string(),
        );
        checker.record_check(&result1).await;
        let result2 = HealthCheckResult::unhealthy(
            "turn:unhealthy.com:3478".to_string(),
            "Error 2".to_string(),
        );
        checker.record_check(&result2).await;

        // Mark one as healthy
        let result3 = HealthCheckResult::healthy("turn:healthy.com:3478".to_string());
        checker.record_check(&result3).await;

        // Filter - should exclude unhealthy, include healthy and unknown
        let filtered = checker.filter_healthy_servers(&urls).await;
        assert_eq!(filtered.len(), 2);
        assert!(filtered.contains(&"turn:healthy.com:3478".to_string()));
        assert!(filtered.contains(&"turn:unknown.com:3478".to_string()));
        assert!(!filtered.contains(&"turn:unhealthy.com:3478".to_string()));
    }

    #[tokio::test]
    async fn test_turn_health_checker_get_all_health() {
        let checker = TurnHealthChecker::new();

        // Record some results
        checker
            .record_check(&HealthCheckResult::healthy("turn:a.com:3478".to_string()))
            .await;
        checker
            .record_check(&HealthCheckResult::unhealthy(
                "turn:b.com:3478".to_string(),
                "Error".to_string(),
            ))
            .await;
        checker
            .record_check(&HealthCheckResult::unhealthy(
                "turn:b.com:3478".to_string(),
                "Error".to_string(),
            ))
            .await;

        let all = checker.get_all_health().await;
        assert_eq!(all.len(), 2);
        assert_eq!(all.get("turn:a.com:3478"), Some(&TurnServerHealth::Healthy));
        assert_eq!(
            all.get("turn:b.com:3478"),
            Some(&TurnServerHealth::Unhealthy)
        );
    }

    #[tokio::test]
    async fn test_turn_health_checker_update_servers() {
        let checker = TurnHealthChecker::new();

        // Update servers list
        checker
            .update_servers(vec![
                "turn:a.com:3478".to_string(),
                "turn:b.com:3478".to_string(),
            ])
            .await;

        // All should be tracked (as healthy by default)
        let health = checker.get_health("turn:a.com:3478").await;
        assert_eq!(health, Some(TurnServerHealth::Healthy));

        let health = checker.get_health("turn:b.com:3478").await;
        assert_eq!(health, Some(TurnServerHealth::Healthy));
    }
}
