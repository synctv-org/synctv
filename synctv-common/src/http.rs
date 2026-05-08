//! HTTP client builder with optional SSRF enforcement.
//!
//! Provides [`SsrfSafeClientBuilder`] with two presets:
//! - [`SsrfSafeClientBuilder::provider()`] — for media-provider API calls
//! - [`SsrfSafeClientBuilder::proxy()`] — for outbound media proxy fetches
//!
//! Clients can enforce SSRF protection through an explicit [`crate::ssrf::SsrfGuard`]
//! stored on the builder and always disable automatic redirects.

use std::time::Duration;

use crate::ssrf::SsrfGuard;

/// Builder for [`reqwest::Client`] instances with optional SSRF enforcement.
///
/// Every client built through this builder automatically gets:
/// - Redirect policy set to `none` (prevents redirect-based SSRF)
///
/// SSRF DNS enforcement is opt-in because many SyncTV deployments intentionally
/// use private-network media sources.
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

impl SsrfSafeClientBuilder {
    /// Preset for media-provider API calls.
    ///
    /// Defaults: 10 s connect, 30 s request, pool 10, no read timeout.
    #[must_use]
    pub fn provider() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            request_timeout: Some(Duration::from_secs(30)),
            read_timeout: None,
            pool_max_idle_per_host: 10,
            pool_idle_timeout: None,
            user_agent: None,
            resolves: Vec::new(),
            ssrf_guard: None,
        }
    }

    /// Preset for outbound media proxy fetches.
    ///
    /// Defaults: 10 s connect, no request/read timeout, pool 100, 30 s idle.
    ///
    /// Proxy clients intentionally avoid client-wide request/read timeouts so
    /// long-lived media bodies are not cut off mid-stream. If a caller needs a
    /// timeout, it should apply an explicit per-request budget only around
    /// "send request + receive response headers".
    #[must_use]
    pub fn proxy() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            request_timeout: None,
            read_timeout: None,
            pool_max_idle_per_host: 100,
            pool_idle_timeout: Some(Duration::from_secs(30)),
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

/// Build a provider-preset client (convenience wrapper).
///
/// Equivalent to `SsrfSafeClientBuilder::provider().build()`.
pub fn build_provider_client() -> Result<reqwest::Client, reqwest::Error> {
    SsrfSafeClientBuilder::provider().build()
}

/// Build a proxy-preset client (convenience wrapper).
///
/// Equivalent to `SsrfSafeClientBuilder::proxy().build()`.
pub fn build_proxy_client() -> Result<reqwest::Client, reqwest::Error> {
    SsrfSafeClientBuilder::proxy().build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_customization() {
        let b = SsrfSafeClientBuilder::provider()
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
        let b = SsrfSafeClientBuilder::proxy().resolve("example.com", addr);

        assert_eq!(b.resolves, vec![("example.com".to_string(), addr)]);
    }

    #[test]
    fn test_build_provider_client() {
        let _client = build_provider_client().expect("provider client should build");
    }

    #[test]
    fn test_build_proxy_client() {
        let _client = build_proxy_client().expect("proxy client should build");
    }

    #[test]
    fn test_disable_ssrf_guard_is_explicit() {
        let builder = SsrfSafeClientBuilder::provider().disable_ssrf_guard();
        assert!(builder.ssrf_guard.is_none());
    }

    #[test]
    fn test_ssrf_guard_is_opt_in() {
        let builder = SsrfSafeClientBuilder::provider().ssrf_guard(SsrfGuard::strict_policy());
        assert!(builder.ssrf_guard.is_some());
    }

    #[test]
    fn test_proxy_preset_disables_request_and_read_timeouts() {
        let builder = SsrfSafeClientBuilder::proxy();

        assert_eq!(builder.request_timeout, None);
        assert_eq!(builder.read_timeout, None);
    }

    #[test]
    fn test_build_with_user_agent() {
        let _client = SsrfSafeClientBuilder::provider()
            .user_agent("MyApp/1.0")
            .build()
            .expect("custom provider client should build");
    }

    #[test]
    fn test_disable_request_timeout() {
        let builder = SsrfSafeClientBuilder::provider().disable_request_timeout();
        assert_eq!(builder.request_timeout, None);
    }

    #[test]
    fn test_disable_read_timeout() {
        let builder = SsrfSafeClientBuilder::proxy().disable_read_timeout();
        assert_eq!(builder.read_timeout, None);
    }
}
