//! SSRF-safe HTTP client builder.
//!
//! Provides [`SsrfSafeClientBuilder`] with two presets:
//! - [`SsrfSafeClientBuilder::provider()`] — for media-provider API calls
//! - [`SsrfSafeClientBuilder::proxy()`] — for outbound media proxy fetches
//!
//! All clients enforce SSRF protection via [`crate::ssrf::ssrf_dns_resolver()`]
//! and disable automatic redirects.

use std::time::Duration;

use crate::ssrf::ssrf_dns_resolver;

/// Builder for SSRF-safe [`reqwest::Client`] instances.
///
/// Every client built through this builder automatically gets:
/// - SSRF-safe DNS resolver (blocks private/reserved IPs at connect time)
/// - Redirect policy set to `none` (prevents redirect-based SSRF)
pub struct SsrfSafeClientBuilder {
    connect_timeout: Duration,
    request_timeout: Duration,
    read_timeout: Option<Duration>,
    pool_max_idle_per_host: usize,
    pool_idle_timeout: Option<Duration>,
    user_agent: Option<String>,
}

impl SsrfSafeClientBuilder {
    /// Preset for media-provider API calls.
    ///
    /// Defaults: 10 s connect, 30 s request, pool 10, no read timeout.
    #[must_use]
    pub const fn provider() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
            read_timeout: None,
            pool_max_idle_per_host: 10,
            pool_idle_timeout: None,
            user_agent: None,
        }
    }

    /// Preset for outbound media proxy fetches.
    ///
    /// Defaults: 10 s connect, 60 s request, 30 s read, pool 100, 30 s idle.
    #[must_use]
    pub const fn proxy() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_mins(1),
            read_timeout: Some(Duration::from_secs(30)),
            pool_max_idle_per_host: 100,
            pool_idle_timeout: Some(Duration::from_secs(30)),
            user_agent: None,
        }
    }

    /// Override the overall request timeout.
    #[must_use]
    pub const fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
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

    /// Build the [`reqwest::Client`].
    ///
    /// # Panics
    ///
    /// Panics if the underlying `reqwest::ClientBuilder` fails (e.g., TLS
    /// backend unavailable). This is intentional — callers typically store
    /// the result in a `LazyLock` and cannot propagate errors.
    #[must_use]
    pub fn build(self) -> reqwest::Client {
        let mut builder = reqwest::Client::builder()
            .dns_resolver(ssrf_dns_resolver())
            .connect_timeout(self.connect_timeout)
            .timeout(self.request_timeout)
            .pool_max_idle_per_host(self.pool_max_idle_per_host)
            .redirect(reqwest::redirect::Policy::none());

        if let Some(rt) = self.read_timeout {
            builder = builder.read_timeout(rt);
        }
        if let Some(pit) = self.pool_idle_timeout {
            builder = builder.pool_idle_timeout(pit);
        }
        if let Some(ua) = self.user_agent {
            builder = builder.user_agent(ua);
        }

        builder
            .build()
            .expect("Failed to build SSRF-safe HTTP client")
    }
}

/// Build a provider-preset client (convenience wrapper).
///
/// Equivalent to `SsrfSafeClientBuilder::provider().build()`.
#[must_use]
pub fn build_provider_client() -> reqwest::Client {
    SsrfSafeClientBuilder::provider().build()
}

/// Build a proxy-preset client (convenience wrapper).
///
/// Equivalent to `SsrfSafeClientBuilder::proxy().build()`.
#[must_use]
pub fn build_proxy_client() -> reqwest::Client {
    SsrfSafeClientBuilder::proxy().build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_defaults() {
        let b = SsrfSafeClientBuilder::provider();
        assert_eq!(b.connect_timeout, Duration::from_secs(10));
        assert_eq!(b.request_timeout, Duration::from_secs(30));
        assert!(b.read_timeout.is_none());
        assert_eq!(b.pool_max_idle_per_host, 10);
        assert!(b.pool_idle_timeout.is_none());
        assert!(b.user_agent.is_none());
    }

    #[test]
    fn test_proxy_defaults() {
        let b = SsrfSafeClientBuilder::proxy();
        assert_eq!(b.connect_timeout, Duration::from_secs(10));
        assert_eq!(b.request_timeout, Duration::from_mins(1));
        assert_eq!(b.read_timeout, Some(Duration::from_secs(30)));
        assert_eq!(b.pool_max_idle_per_host, 100);
        assert_eq!(b.pool_idle_timeout, Some(Duration::from_secs(30)));
        assert!(b.user_agent.is_none());
    }

    #[test]
    fn test_builder_customization() {
        let b = SsrfSafeClientBuilder::provider()
            .request_timeout(Duration::from_mins(2))
            .read_timeout(Duration::from_mins(1))
            .pool_max_idle_per_host(50)
            .pool_idle_timeout(Duration::from_secs(90))
            .user_agent("test-agent");
        assert_eq!(b.request_timeout, Duration::from_mins(2));
        assert_eq!(b.read_timeout, Some(Duration::from_mins(1)));
        assert_eq!(b.pool_max_idle_per_host, 50);
        assert_eq!(b.pool_idle_timeout, Some(Duration::from_secs(90)));
        assert_eq!(b.user_agent.as_deref(), Some("test-agent"));
    }

    #[test]
    fn test_build_provider_client() {
        let _client = build_provider_client();
    }

    #[test]
    fn test_build_proxy_client() {
        let _client = build_proxy_client();
    }

    #[test]
    fn test_build_with_user_agent() {
        let _client = SsrfSafeClientBuilder::provider()
            .user_agent("MyApp/1.0")
            .build();
    }
}
