//! SSRF (Server-Side Request Forgery) protection using `http-acl`.
//!
//! Provides [`SsrfGuard`], a configurable SSRF protection guard that wraps
//! `HttpAcl` and provides DNS resolver + IP/host checking.
//!
//! # Quick Start
//!
//! Use the free functions for production defaults:
//! ```
//! let resolver = synctv_common::ssrf::ssrf_dns_resolver();
//! let blocked = synctv_common::ssrf::is_ip_blocked(&"127.0.0.1".parse().unwrap());
//! ```
//!
//! Or use [`SsrfGuard`] directly for custom policies:
//! ```
//! use synctv_common::ssrf::SsrfGuard;
//!
//! let guard = SsrfGuard::builder()
//!     .extra_allowed_host("internal.example.com".to_string())
//!     .build();
//! let resolver = guard.dns_resolver();
//! ```

use http_acl::HttpAcl;
use http_acl_reqwest::HttpAclMiddleware;
use ipnet::IpNet;
use std::net::IpAddr;
use std::sync::Arc;

/// Default extra denied IP ranges (beyond what `http-acl` blocks via `is_global`).
const DEFAULT_EXTRA_DENIED_RANGES: &[&str] = &[
    "224.0.0.0/4", // IPv4 multicast
    "ff00::/8",    // IPv6 multicast
    "2002::/16",   // 6to4 (deprecated, SSRF vector)
];

/// Default denied hostnames.
const DEFAULT_DENIED_HOSTS: &[&str] = &[
    "localhost",
    "metadata.google.internal",
    "instance-data",
    "metadata.azure",
];

/// Configurable SSRF protection guard.
///
/// Wraps [`HttpAcl`] and [`HttpAclMiddleware`] to provide DNS resolver
/// integration and IP/host checking. Use [`SsrfGuard::default_policy()`]
/// for production defaults or [`SsrfGuard::builder()`] for custom policies.
#[derive(Clone)]
pub struct SsrfGuard {
    acl: HttpAcl,
    middleware: HttpAclMiddleware,
}

impl SsrfGuard {
    /// Create with sensible production defaults.
    ///
    /// Blocks private/reserved/multicast/metadata IPs and known internal hostnames.
    #[must_use]
    pub fn default_policy() -> Self {
        Self::builder().build()
    }

    /// Create from builder for custom policies.
    #[must_use]
    pub const fn builder() -> SsrfGuardBuilder {
        SsrfGuardBuilder::new()
    }

    /// Get a reqwest DNS resolver that enforces this guard's policy.
    #[must_use]
    pub fn dns_resolver(&self) -> Arc<dyn reqwest::dns::Resolve> {
        self.middleware.dns_resolver()
    }

    /// Check if an IP is blocked by this guard's policy.
    ///
    /// Useful for non-HTTP protocols (e.g., RTMP) where the DNS resolver
    /// cannot be injected.
    #[must_use]
    pub fn is_ip_blocked(&self, ip: &IpAddr) -> bool {
        self.acl.is_ip_allowed(ip).is_denied()
    }

    /// Check if a hostname is blocked by this guard's policy.
    #[must_use]
    pub fn is_host_blocked(&self, host: &str) -> bool {
        self.acl.is_host_allowed(host).is_denied()
    }

    /// Access the underlying ACL for advanced use.
    #[must_use]
    pub const fn acl(&self) -> &HttpAcl {
        &self.acl
    }
}

/// Builder for [`SsrfGuard`] with custom policies.
///
/// Starts with the same defaults as [`SsrfGuard::default_policy()`] and allows
/// adding extra denied/allowed ranges and hosts.
pub struct SsrfGuardBuilder {
    extra_denied_ip_ranges: Vec<IpNet>,
    extra_denied_hosts: Vec<String>,
    extra_allowed_ip_ranges: Vec<IpNet>,
    extra_allowed_hosts: Vec<String>,
    allow_http: bool,
    allow_https: bool,
}

impl SsrfGuardBuilder {
    const fn new() -> Self {
        Self {
            extra_denied_ip_ranges: Vec::new(),
            extra_denied_hosts: Vec::new(),
            extra_allowed_ip_ranges: Vec::new(),
            extra_allowed_hosts: Vec::new(),
            allow_http: true,
            allow_https: true,
        }
    }

    /// Add an extra IP range to deny (beyond defaults).
    #[must_use]
    pub fn extra_denied_ip_range(mut self, range: IpNet) -> Self {
        self.extra_denied_ip_ranges.push(range);
        self
    }

    /// Add an extra hostname to deny (beyond defaults).
    #[must_use]
    pub fn extra_denied_host(mut self, host: String) -> Self {
        self.extra_denied_hosts.push(host);
        self
    }

    /// Add an IP range to explicitly allow (overrides deny rules).
    #[must_use]
    pub fn extra_allowed_ip_range(mut self, range: IpNet) -> Self {
        self.extra_allowed_ip_ranges.push(range);
        self
    }

    /// Add a hostname to explicitly allow (overrides deny rules).
    #[must_use]
    pub fn extra_allowed_host(mut self, host: String) -> Self {
        self.extra_allowed_hosts.push(host);
        self
    }

    /// Set whether HTTP is allowed (default: true).
    #[must_use]
    pub const fn allow_http(mut self, allow: bool) -> Self {
        self.allow_http = allow;
        self
    }

    /// Set whether HTTPS is allowed (default: true).
    #[must_use]
    pub const fn allow_https(mut self, allow: bool) -> Self {
        self.allow_https = allow;
        self
    }

    /// Build the [`SsrfGuard`] with configured policy.
    #[must_use]
    #[allow(clippy::unwrap_used)] // Parsing constant CIDR strings cannot fail
    pub fn build(self) -> SsrfGuard {
        let mut builder = HttpAcl::builder();

        // Apply default denied ranges
        for range_str in DEFAULT_EXTRA_DENIED_RANGES {
            let range: IpNet = range_str.parse().unwrap();
            builder = builder.add_denied_ip_range(range).expect("valid IP range");
        }

        // Apply default denied hosts
        for host in DEFAULT_DENIED_HOSTS {
            builder = builder
                .add_denied_host((*host).to_string())
                .expect("valid hostname");
        }

        // Apply extra denied ranges
        for range in self.extra_denied_ip_ranges {
            builder = builder.add_denied_ip_range(range).expect("valid IP range");
        }

        // Apply extra denied hosts
        for host in self.extra_denied_hosts {
            builder = builder.add_denied_host(host).expect("valid hostname");
        }

        // Apply extra allowed ranges
        for range in self.extra_allowed_ip_ranges {
            builder = builder.add_allowed_ip_range(range).expect("valid IP range");
        }

        // Apply extra allowed hosts
        for host in self.extra_allowed_hosts {
            builder = builder.add_allowed_host(host).expect("valid hostname");
        }

        let acl = builder
            .ip_acl_default(true)
            .host_acl_default(true)
            .http(self.allow_http)
            .https(self.allow_https)
            .try_build()
            .expect("SSRF ACL configuration is valid");

        let middleware = HttpAclMiddleware::new(acl.clone());

        SsrfGuard { acl, middleware }
    }
}

// ---------------------------------------------------------------------------
// Backward-compatible free functions
// ---------------------------------------------------------------------------

/// Create the default [`SsrfGuard`] with production defaults.
#[must_use]
pub fn default_ssrf_guard() -> SsrfGuard {
    SsrfGuard::default_policy()
}

/// Create the standard SSRF-safe ACL used across all HTTP clients.
///
/// Equivalent to `SsrfGuard::default_policy().acl().clone()`.
#[must_use]
pub fn ssrf_acl() -> HttpAcl {
    SsrfGuard::default_policy().acl().clone()
}

/// Create a SSRF-safe DNS resolver for use with `reqwest::Client::builder().dns_resolver()`.
///
/// Equivalent to `SsrfGuard::default_policy().dns_resolver()`.
#[must_use]
pub fn ssrf_dns_resolver() -> Arc<dyn reqwest::dns::Resolve> {
    SsrfGuard::default_policy().dns_resolver()
}

/// Check if an IP address is blocked by the default SSRF policy.
///
/// Equivalent to `SsrfGuard::default_policy().is_ip_blocked(ip)`.
#[must_use]
pub fn is_ip_blocked(ip: &IpAddr) -> bool {
    SsrfGuard::default_policy().is_ip_blocked(ip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    // =======================================================================
    // Default policy tests (migrated from synctv-ssrf)
    // =======================================================================

    #[test]
    fn test_acl_blocks_private_ipv4() {
        let guard = SsrfGuard::default_policy();
        let acl = guard.acl();
        // Loopback
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::LOCALHOST))
            .is_denied());
        // Private ranges
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)))
            .is_denied());
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)))
            .is_denied());
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255)))
            .is_denied());
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)))
            .is_denied());
        // Link-local / cloud metadata
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1)))
            .is_denied());
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)))
            .is_denied());
        // CGNAT
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)))
            .is_denied());
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(100, 127, 255, 254)))
            .is_denied());
        // Current network
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::UNSPECIFIED))
            .is_denied());
        // Multicast
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)))
            .is_denied());
        // Reserved
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(240, 0, 0, 1)))
            .is_denied());
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::BROADCAST))
            .is_denied());
    }

    #[test]
    fn test_acl_allows_public_ipv4() {
        let guard = SsrfGuard::default_policy();
        let acl = guard.acl();
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)))
            .is_allowed());
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)))
            .is_allowed());
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)))
            .is_allowed());
    }

    #[test]
    fn test_acl_blocks_ipv6() {
        let guard = SsrfGuard::default_policy();
        let acl = guard.acl();
        assert!(acl
            .is_ip_allowed(&IpAddr::V6(Ipv6Addr::LOCALHOST))
            .is_denied());
        assert!(acl
            .is_ip_allowed(&IpAddr::V6(Ipv6Addr::UNSPECIFIED))
            .is_denied());
        // Unique local
        assert!(acl
            .is_ip_allowed(&IpAddr::V6(Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 1)))
            .is_denied());
        assert!(acl
            .is_ip_allowed(&IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)))
            .is_denied());
        // Link-local
        assert!(acl
            .is_ip_allowed(&IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)))
            .is_denied());
        // Multicast
        assert!(acl
            .is_ip_allowed(&IpAddr::V6(Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 1)))
            .is_denied());
        // Teredo
        assert!(acl
            .is_ip_allowed(&IpAddr::V6(Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 1)))
            .is_denied());
        // 6to4
        assert!(acl
            .is_ip_allowed(&IpAddr::V6(Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 1)))
            .is_denied());
    }

    #[test]
    fn test_acl_allows_public_ipv6() {
        let guard = SsrfGuard::default_policy();
        let acl = guard.acl();
        let google = IpAddr::V6(Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888));
        assert!(acl.is_ip_allowed(&google).is_allowed());
        let cloudflare = IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111));
        assert!(acl.is_ip_allowed(&cloudflare).is_allowed());
    }

    #[test]
    fn test_acl_blocks_hostnames() {
        let guard = SsrfGuard::default_policy();
        assert!(guard.is_host_blocked("localhost"));
        assert!(guard.is_host_blocked("metadata.google.internal"));
        assert!(guard.is_host_blocked("instance-data"));
        assert!(guard.is_host_blocked("metadata.azure"));
    }

    #[test]
    fn test_acl_allows_public_hostnames() {
        let guard = SsrfGuard::default_policy();
        let acl = guard.acl();
        assert!(acl.is_host_allowed("example.com").is_allowed());
        assert!(acl.is_host_allowed("api.bilibili.com").is_allowed());
        assert!(acl.is_host_allowed("github.com").is_allowed());
    }

    #[test]
    fn test_is_ip_blocked() {
        assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(!is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
    }

    #[test]
    fn test_ssrf_dns_resolver_creation() {
        let _resolver = ssrf_dns_resolver();
    }

    #[test]
    fn test_ipv4_172_range_boundary() {
        let guard = SsrfGuard::default_policy();
        let acl = guard.acl();
        // 172.15.x.x is NOT private (outside 172.16.0.0/12)
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(172, 15, 255, 255)))
            .is_allowed());
        // 172.16.0.0 through 172.31.255.255 IS private
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(172, 16, 0, 0)))
            .is_denied());
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255)))
            .is_denied());
        // 172.32.x.x is NOT private
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(172, 32, 0, 0)))
            .is_allowed());
    }

    #[test]
    fn test_ipv4_cgnat_boundary() {
        let guard = SsrfGuard::default_policy();
        let acl = guard.acl();
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(100, 63, 255, 255)))
            .is_allowed());
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(100, 64, 0, 0)))
            .is_denied());
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(100, 127, 255, 255)))
            .is_denied());
        assert!(acl
            .is_ip_allowed(&IpAddr::V4(Ipv4Addr::new(100, 128, 0, 0)))
            .is_allowed());
    }

    // =======================================================================
    // SsrfGuard struct tests
    // =======================================================================

    #[test]
    fn test_guard_is_ip_blocked() {
        let guard = SsrfGuard::default_policy();
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    #[test]
    fn test_guard_is_host_blocked() {
        let guard = SsrfGuard::default_policy();
        assert!(guard.is_host_blocked("localhost"));
        assert!(guard.is_host_blocked("metadata.google.internal"));
        assert!(!guard.is_host_blocked("example.com"));
    }

    #[test]
    fn test_guard_dns_resolver() {
        let guard = SsrfGuard::default_policy();
        let _resolver = guard.dns_resolver();
    }

    #[test]
    fn test_guard_clone() {
        let guard = SsrfGuard::default_policy();
        let cloned = guard;
        assert!(cloned.is_ip_blocked(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(!cloned.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    // =======================================================================
    // Builder tests
    // =======================================================================

    #[test]
    fn test_builder_extra_denied_ip_range() {
        // Use a global IP range (Cloudflare's 104.16.0.0/12) to test custom deny rules.
        // Non-global ranges like 203.0.113.0/24 are already blocked by default.
        let guard = SsrfGuard::builder()
            .extra_denied_ip_range("104.16.0.0/12".parse().unwrap())
            .build();
        // Custom denied range is blocked
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(104, 16, 0, 1))));
        // Default blocks still apply
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
        // Public IPs outside the denied range still allowed
        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    #[test]
    fn test_builder_extra_denied_host() {
        let guard = SsrfGuard::builder()
            .extra_denied_host("evil.internal".to_string())
            .build();
        assert!(guard.is_host_blocked("evil.internal"));
        // Default blocks still apply
        assert!(guard.is_host_blocked("localhost"));
        // Public hosts still allowed
        assert!(!guard.is_host_blocked("example.com"));
    }

    #[test]
    fn test_builder_extra_allowed_host() {
        let guard = SsrfGuard::builder()
            .extra_allowed_host("trusted.internal".to_string())
            .build();
        // Default blocks still apply
        assert!(guard.is_host_blocked("localhost"));
        // Public hosts still allowed
        assert!(!guard.is_host_blocked("example.com"));
    }

    #[test]
    fn test_builder_disallow_http() {
        let guard = SsrfGuard::builder().allow_http(false).build();
        let acl = guard.acl();
        // HTTP should be disallowed
        assert!(acl.is_scheme_allowed("http").is_denied());
        // HTTPS should still be allowed
        assert!(acl.is_scheme_allowed("https").is_allowed());
    }

    #[test]
    fn test_builder_disallow_https() {
        let guard = SsrfGuard::builder().allow_https(false).build();
        let acl = guard.acl();
        // HTTPS should be disallowed
        assert!(acl.is_scheme_allowed("https").is_denied());
        // HTTP should still be allowed
        assert!(acl.is_scheme_allowed("http").is_allowed());
    }

    #[test]
    fn test_default_ssrf_guard_matches_default_policy() {
        let guard1 = default_ssrf_guard();
        let guard2 = SsrfGuard::default_policy();
        // Both should block/allow the same IPs
        let test_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert_eq!(
            guard1.is_ip_blocked(&test_ip),
            guard2.is_ip_blocked(&test_ip)
        );
        let public_ip = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
        assert_eq!(
            guard1.is_ip_blocked(&public_ip),
            guard2.is_ip_blocked(&public_ip)
        );
    }
}
