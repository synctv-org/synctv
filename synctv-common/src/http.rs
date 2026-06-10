//! HTTP client builder with injectable SSRF enforcement.
//!
//! Callers pass a [`crate::ssrf::SsrfGuard`] that was built from their startup
//! configuration. Automatic redirects are always disabled.

use std::time::Duration;

use crate::ssrf::SsrfGuard;

/// Builder for [`reqwest::Client`] instances with explicit SSRF enforcement.
///
/// Every client built through this builder automatically gets:
/// - Redirect policy set to `none` (prevents redirect-based SSRF)
///
/// SSRF DNS enforcement is opt-in at this low layer so application code must
/// pass the policy it loaded from configuration.
pub struct SsrfSafeClientBuilder {
    connect_timeout: Duration,
    request_timeout: Option<Duration>,
    read_timeout: Option<Duration>,
    pool_max_idle_per_host: usize,
    pool_idle_timeout: Option<Duration>,
    user_agent: Option<String>,
    resolves: Vec<(String, std::net::SocketAddr)>,
    ssrf_guard: Option<SsrfGuard>,
}

impl Default for SsrfSafeClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SsrfSafeClientBuilder {
    /// Create a generic HTTP client builder.
    ///
    /// Defaults: 10 s connect timeout, no request/read timeout, pool 10.
    #[must_use]
    pub fn new() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            request_timeout: None,
            read_timeout: None,
            pool_max_idle_per_host: 10,
            pool_idle_timeout: None,
            user_agent: None,
            resolves: Vec::new(),
            ssrf_guard: None,
        }
    }

    /// Override the SSRF policy used by the injected DNS resolver.
    #[must_use]
    pub fn ssrf_guard(mut self, guard: SsrfGuard) -> Self {
        self.ssrf_guard = Some(guard);
        self
    }

    /// Disable SSRF DNS enforcement.
    #[must_use]
    pub fn disable_ssrf_guard(mut self) -> Self {
        self.ssrf_guard = None;
        self
    }

    /// Override the overall request timeout.
    #[must_use]
    pub const fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    /// Disable the overall request timeout.
    #[must_use]
    pub const fn disable_request_timeout(mut self) -> Self {
        self.request_timeout = None;
        self
    }

    /// Disable the body-read timeout.
    #[must_use]
    pub const fn disable_read_timeout(mut self) -> Self {
        self.read_timeout = None;
        self
    }

    /// Override the connection-establishment timeout.
    #[must_use]
    pub const fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Override the body-read timeout.
    #[must_use]
    pub const fn read_timeout(mut self, timeout: Duration) -> Self {
        self.read_timeout = Some(timeout);
        self
    }

    /// Override the maximum idle connections per host.
    #[must_use]
    pub const fn pool_max_idle_per_host(mut self, max: usize) -> Self {
        self.pool_max_idle_per_host = max;
        self
    }

    /// Override the pool idle timeout.
    #[must_use]
    pub const fn pool_idle_timeout(mut self, timeout: Duration) -> Self {
        self.pool_idle_timeout = Some(timeout);
        self
    }

    /// Set a custom User-Agent header.
    #[must_use]
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = Some(ua.into());
        self
    }

    /// Pin a hostname to a resolved socket address.
    ///
    /// This is useful when the caller has already performed SSRF-checked DNS
    /// resolution and wants to prevent DNS rebinding between validation and
    /// connect time.
    #[must_use]
    pub fn resolve(mut self, host: impl Into<String>, addr: std::net::SocketAddr) -> Self {
        self.resolves.push((host.into(), addr));
        self
    }

    /// Build the [`reqwest::Client`].
    pub fn build(self) -> Result<reqwest::Client, reqwest::Error> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(self.connect_timeout)
            .pool_max_idle_per_host(self.pool_max_idle_per_host)
            .redirect(reqwest::redirect::Policy::none());

        if let Some(resolver) = self.ssrf_guard.as_ref().and_then(SsrfGuard::dns_resolver) {
            builder = builder.dns_resolver(resolver);
        }

        if let Some(request_timeout) = self.request_timeout {
            builder = builder.timeout(request_timeout);
        }
        if let Some(rt) = self.read_timeout {
            builder = builder.read_timeout(rt);
        }
        if let Some(pit) = self.pool_idle_timeout {
            builder = builder.pool_idle_timeout(pit);
        }
        if let Some(ua) = self.user_agent {
            builder = builder.user_agent(ua);
        }
        for (host, addr) in self.resolves {
            builder = builder.resolve(&host, addr);
        }

        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = Result<T, String>;

    #[test]
    fn test_builder_customization() {
        let b = SsrfSafeClientBuilder::new()
            .connect_timeout(Duration::from_secs(3))
            .request_timeout(Duration::from_mins(2))
            .read_timeout(Duration::from_mins(1))
            .pool_max_idle_per_host(50)
            .pool_idle_timeout(Duration::from_secs(90))
            .user_agent("test-agent");
        assert_eq!(b.connect_timeout, Duration::from_secs(3));
        assert_eq!(b.request_timeout, Some(Duration::from_mins(2)));
        assert_eq!(b.read_timeout, Some(Duration::from_mins(1)));
        assert_eq!(b.pool_max_idle_per_host, 50);
        assert_eq!(b.pool_idle_timeout, Some(Duration::from_secs(90)));
        assert_eq!(b.user_agent.as_deref(), Some("test-agent"));
        assert!(b.resolves.is_empty());
        assert!(b.ssrf_guard.is_none());
    }

    #[test]
    fn test_builder_resolve_override() {
        let addr = std::net::SocketAddr::from(([203, 0, 113, 10], 443));
        let b = SsrfSafeClientBuilder::new().resolve("example.com", addr);

        assert_eq!(b.resolves, vec![("example.com".to_string(), addr)]);
    }

    #[test]
    fn test_build_client() -> TestResult {
        let _client = SsrfSafeClientBuilder::new()
            .build()
            .map_err(|error| format!("HTTP client should build: {error}"))?;
        Ok(())
    }

    #[test]
    fn test_disable_ssrf_guard_is_explicit() {
        let builder = SsrfSafeClientBuilder::new().disable_ssrf_guard();
        assert!(builder.ssrf_guard.is_none());
    }

    #[test]
    fn test_ssrf_guard_is_opt_in() {
        let builder = SsrfSafeClientBuilder::new().ssrf_guard(SsrfGuard::strict_policy());
        assert!(builder.ssrf_guard.is_some());
    }

    #[test]
    fn test_default_disables_request_and_read_timeouts() {
        let builder = SsrfSafeClientBuilder::new();

        assert_eq!(builder.request_timeout, None);
        assert_eq!(builder.read_timeout, None);
    }

    #[test]
    fn test_build_with_user_agent() -> TestResult {
        let _client = SsrfSafeClientBuilder::new()
            .user_agent("MyApp/1.0")
            .build()
            .map_err(|error| format!("custom HTTP client should build: {error}"))?;
        Ok(())
    }

    #[test]
    fn test_disable_request_timeout() {
        let builder = SsrfSafeClientBuilder::new()
            .request_timeout(Duration::from_secs(5))
            .disable_request_timeout();
        assert_eq!(builder.request_timeout, None);
    }

    #[test]
    fn test_disable_read_timeout() {
        let builder = SsrfSafeClientBuilder::new()
            .read_timeout(Duration::from_secs(5))
            .disable_read_timeout();
        assert_eq!(builder.read_timeout, None);
    }
}
