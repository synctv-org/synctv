//! SSRF (Server-Side Request Forgery) protection using `http-acl`.
//!
//! Provides [`SsrfGuard`], a configurable SSRF protection guard that wraps
//! `HttpAcl` and provides DNS resolver + IP/host checking.
//!
//! # Quick Start
//!
//! Build a guard explicitly from application configuration and pass it to
//! outbound clients or validators:
//! ```
//! use synctv_common::ssrf::SsrfGuard;
//!
//! let guard = SsrfGuard::strict_policy();
//! let resolver = guard.dns_resolver();
//! ```
//!
//! ```
//! use synctv_common::ssrf::SsrfGuard;
//!
//! let guard = SsrfGuard::builder()
//!     .extra_allowed_host("internal.example.com".to_string())
//!     .build();
//! let resolver = guard.dns_resolver();
//! ```

use http_acl::HttpAcl;
use ipnet::IpNet;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::collections::HashSet;
use std::error::Error;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

type BoxError = Box<dyn Error + Send + Sync>;

/// Default extra denied IP ranges (beyond what `http-acl` blocks via `is_global`).
const DEFAULT_EXTRA_DENIED_RANGES: &[&str] = &[
    "224.0.0.0/4", // IPv4 multicast
    "ff00::/8",    // IPv6 multicast
    "2002::/16",   // 6to4 (deprecated, SSRF vector)
];

/// Non-global ranges that remain denied when selected private CIDRs are allowlisted.
///
/// `http-acl` checks the broad non-global gate before user allow rules. To allow
/// a specific private CIDR without allowing every non-global address, we enable
/// that broad gate and then re-add the non-global ranges as explicit deny rules.
const DEFAULT_NON_GLOBAL_DENIED_RANGES: &[&str] = &[
    "0.0.0.0/8",       // current network
    "10.0.0.0/8",      // private
    "100.64.0.0/10",   // carrier-grade NAT
    "127.0.0.0/8",     // loopback
    "169.254.0.0/16",  // link-local / cloud metadata
    "172.16.0.0/12",   // private
    "192.0.0.0/24",    // IETF protocol assignments
    "192.0.2.0/24",    // documentation
    "192.88.99.0/24",  // 6to4 relay anycast
    "192.168.0.0/16",  // private
    "198.18.0.0/15",   // benchmarking
    "198.51.100.0/24", // documentation
    "203.0.113.0/24",  // documentation
    "240.0.0.0/4",     // reserved
    "255.255.255.255/32",
    "::/128",        // unspecified
    "::1/128",       // loopback
    "::ffff:0:0/96", // IPv4-mapped
    "64:ff9b::/96",  // IPv4/IPv6 translation
    "100::/64",      // discard-only
    "2001::/32",     // Teredo
    "2001:db8::/32", // documentation
    "fc00::/7",      // unique local
    "fe80::/10",     // link-local
];

/// Non-global ranges that a hostname allowlist entry still must not unlock.
///
/// A hostname allowlist is intended for controlled private services such as
/// `alist.internal -> 10.0.8.10`. It should not turn DNS names into a shortcut
/// to loopback, link-local, metadata, documentation, multicast, or reserved
/// networks. Users can still opt into those explicitly with `allowed_ip_ranges`
/// or `allow_private_network_targets`.
const HOST_ALLOWLIST_DENIED_RANGES: &[&str] = &[
    "0.0.0.0/8",       // current network
    "100.64.0.0/10",   // carrier-grade NAT
    "127.0.0.0/8",     // loopback
    "169.254.0.0/16",  // link-local / cloud metadata
    "192.0.0.0/24",    // IETF protocol assignments
    "192.0.2.0/24",    // documentation
    "192.88.99.0/24",  // 6to4 relay anycast
    "198.18.0.0/15",   // benchmarking
    "198.51.100.0/24", // documentation
    "203.0.113.0/24",  // documentation
    "224.0.0.0/4",     // IPv4 multicast
    "240.0.0.0/4",     // reserved
    "255.255.255.255/32",
    "::/128",        // unspecified
    "::1/128",       // loopback
    "::ffff:0:0/96", // IPv4-mapped
    "64:ff9b::/96",  // IPv4/IPv6 translation
    "100::/64",      // discard-only
    "2001::/32",     // Teredo
    "2001:db8::/32", // documentation
    "2002::/16",     // 6to4
    "fe80::/10",     // link-local
    "ff00::/8",      // IPv6 multicast
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
/// integration and IP/host checking. Application code should build one guard
/// from startup configuration and inject it into outbound clients or validators.
#[derive(Clone)]
pub struct SsrfGuard {
    inner: Option<Arc<SsrfGuardInner>>,
}

struct SsrfGuardInner {
    acl: HttpAcl,
    resolver: Arc<dyn Resolve>,
    policy: Arc<SsrfPolicy>,
}

#[derive(Clone)]
struct SsrfPolicy {
    allow_private_network_targets: bool,
    default_denied_ip_ranges: Vec<IpNet>,
    host_allowlist_denied_ip_ranges: Vec<IpNet>,
    extra_denied_ip_ranges: Vec<IpNet>,
    extra_allowed_ip_ranges: Vec<IpNet>,
    denied_hosts: HashSet<String>,
    allowed_hosts: HashSet<String>,
}

struct SsrfDnsResolver {
    acl: Arc<HttpAcl>,
    inner: Arc<dyn Resolve>,
    policy: Arc<SsrfPolicy>,
}

struct SystemDnsResolver;

impl Resolve for SystemDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addresses = tokio::net::lookup_host((host, 0))
                .await
                .map_err(|error| Box::new(error) as BoxError)?;
            Ok(Box::new(addresses) as Addrs)
        })
    }
}

fn normalize_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

fn parse_ranges(ranges: &[&str]) -> Vec<IpNet> {
    ranges
        .iter()
        .map(|range| range.parse().expect("default CIDR should parse"))
        .collect()
}

fn contains_ip(ranges: &[IpNet], ip: &IpAddr) -> bool {
    ranges.iter().any(|range| range.contains(ip))
}

impl SsrfPolicy {
    fn is_ip_allowed(&self, ip: &IpAddr) -> bool {
        if contains_ip(&self.extra_allowed_ip_ranges, ip) {
            return true;
        }

        if contains_ip(&self.extra_denied_ip_ranges, ip) {
            return false;
        }

        if !self.allow_private_network_targets && contains_ip(&self.default_denied_ip_ranges, ip) {
            return false;
        }

        true
    }

    fn is_ip_allowed_for_host(&self, host: &str, ip: &IpAddr) -> bool {
        if self.is_ip_allowed(ip) {
            return true;
        }

        if contains_ip(&self.extra_denied_ip_ranges, ip) {
            return false;
        }

        self.allowed_hosts.contains(&normalize_host(host))
            && !contains_ip(&self.host_allowlist_denied_ip_ranges, ip)
    }

    fn is_host_blocked(&self, host: &str) -> bool {
        let host = normalize_host(host);
        !self.allowed_hosts.contains(&host) && self.denied_hosts.contains(&host)
    }
}

impl Resolve for SsrfDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        if self.acl.is_host_allowed(&host).is_denied() {
            let err: BoxError = Box::new(std::io::Error::other("Host denied by ACL"));
            return Box::pin(std::future::ready(Err(err)));
        }

        let acl = self.acl.clone();
        let resolver = self.inner.clone();
        let policy = self.policy.clone();

        Box::pin(async move {
            let resolved = resolver.resolve(name).await?;
            let filtered = resolved
                .filter(|addr| {
                    policy.is_ip_allowed_for_host(&host, &addr.ip())
                        && acl.is_port_allowed(addr.port()).is_allowed()
                })
                .collect::<Vec<SocketAddr>>();

            Ok(Box::new(filtered.into_iter()) as Addrs)
        })
    }
}

impl SsrfGuard {
    /// Create an explicit strict SSRF policy.
    ///
    /// Blocks private/reserved/multicast/metadata IPs and known internal
    /// hostnames.
    #[must_use]
    pub fn strict_policy() -> Self {
        Self::builder().build()
    }

    /// Create an explicit disabled policy.
    ///
    /// Use only when a trusted deployment intentionally allows private-network
    /// outbound targets.
    #[must_use]
    pub const fn disabled() -> Self {
        Self { inner: None }
    }

    /// Create from builder for custom policies.
    #[must_use]
    pub const fn builder() -> SsrfGuardBuilder {
        SsrfGuardBuilder::new()
    }

    /// Get a reqwest DNS resolver that enforces this guard's policy.
    #[must_use]
    pub fn dns_resolver(&self) -> Option<Arc<dyn reqwest::dns::Resolve>> {
        self.inner.as_ref().map(|inner| inner.resolver.clone())
    }

    /// Check if an IP is blocked by this guard's policy.
    ///
    /// Useful for non-HTTP protocols (e.g., RTMP) where the DNS resolver
    /// cannot be injected.
    #[must_use]
    pub fn is_ip_blocked(&self, ip: &IpAddr) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|inner| !inner.policy.is_ip_allowed(ip))
    }

    /// Check if an IP is blocked for a resolved hostname by this guard's policy.
    ///
    /// Hostname allowlist entries are evaluated here, so callers that perform
    /// their own DNS resolution should use this instead of `is_ip_blocked`.
    #[must_use]
    pub fn is_ip_blocked_for_host(&self, host: &str, ip: &IpAddr) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|inner| !inner.policy.is_ip_allowed_for_host(host, ip))
    }

    /// Check if a hostname is blocked by this guard's policy.
    #[must_use]
    pub fn is_host_blocked(&self, host: &str) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|inner| inner.policy.is_host_blocked(host))
    }

    /// Access the underlying ACL for advanced use.
    #[must_use]
    pub fn acl(&self) -> Option<&HttpAcl> {
        self.inner.as_ref().map(|inner| &inner.acl)
    }
}

/// Builder for [`SsrfGuard`] with custom policies.
///
/// Starts with strict SSRF defaults and allows adding extra denied/allowed
/// ranges and hosts.
pub struct SsrfGuardBuilder {
    extra_denied_ip_ranges: Vec<IpNet>,
    extra_denied_hosts: Vec<String>,
    extra_allowed_ip_ranges: Vec<IpNet>,
    extra_allowed_hosts: Vec<String>,
    allow_private_network_targets: bool,
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
            allow_private_network_targets: false,
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

    /// Allow private, loopback, link-local, reserved, and metadata-network
    /// targets globally.
    #[must_use]
    pub const fn allow_private_network_targets(mut self, allow: bool) -> Self {
        self.allow_private_network_targets = allow;
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
        let allow_non_global_ip_ranges =
            self.allow_private_network_targets || !self.extra_allowed_ip_ranges.is_empty();
        let mut builder = HttpAcl::builder();
        let extra_allowed_ip_ranges = self.extra_allowed_ip_ranges;
        let extra_denied_ip_ranges = self.extra_denied_ip_ranges;
        let allowed_hosts = self
            .extra_allowed_hosts
            .into_iter()
            .map(|host| normalize_host(&host))
            .collect::<HashSet<_>>();
        let mut denied_hosts = DEFAULT_DENIED_HOSTS
            .iter()
            .map(|host| normalize_host(host))
            .collect::<HashSet<_>>();

        for host in self.extra_denied_hosts {
            denied_hosts.insert(normalize_host(&host));
        }

        // Apply default denied ranges
        for range_str in DEFAULT_EXTRA_DENIED_RANGES {
            let range: IpNet = range_str.parse().unwrap();
            builder = builder.add_denied_ip_range(range).expect("valid IP range");
        }

        // Apply denied hosts that were not explicitly allowlisted.
        for host in denied_hosts.difference(&allowed_hosts) {
            builder = builder
                .add_denied_host(host.clone())
                .expect("valid hostname");
        }

        // Apply extra denied ranges to the underlying ACL where they cannot
        // conflict with explicit allow ranges. The guard's own policy below is
        // authoritative for IP decisions.
        for range in &extra_denied_ip_ranges {
            builder = builder.add_denied_ip_range(*range).expect("valid IP range");
        }

        // Apply extra allowed hosts
        for host in &allowed_hosts {
            builder = builder
                .add_allowed_host(host.clone())
                .expect("valid hostname");
        }

        let acl = builder
            .non_global_ip_ranges(allow_non_global_ip_ranges)
            .ip_acl_default(true)
            .host_acl_default(true)
            .http(self.allow_http)
            .https(self.allow_https)
            .try_build()
            .expect("SSRF ACL configuration is valid");

        let mut default_denied_ip_ranges = parse_ranges(DEFAULT_NON_GLOBAL_DENIED_RANGES);
        default_denied_ip_ranges.extend(parse_ranges(DEFAULT_EXTRA_DENIED_RANGES));

        let policy = Arc::new(SsrfPolicy {
            allow_private_network_targets: self.allow_private_network_targets,
            default_denied_ip_ranges,
            host_allowlist_denied_ip_ranges: parse_ranges(HOST_ALLOWLIST_DENIED_RANGES),
            extra_denied_ip_ranges,
            extra_allowed_ip_ranges,
            denied_hosts,
            allowed_hosts,
        });
        let resolver = Arc::new(SsrfDnsResolver {
            acl: Arc::new(acl.clone()),
            inner: Arc::new(SystemDnsResolver),
            policy: policy.clone(),
        }) as Arc<dyn Resolve>;

        SsrfGuard {
            inner: Some(Arc::new(SsrfGuardInner {
                acl,
                resolver,
                policy,
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    struct StaticDnsResolver {
        addresses: Vec<SocketAddr>,
    }

    impl Resolve for StaticDnsResolver {
        fn resolve(&self, _name: Name) -> Resolving {
            let addresses = self.addresses.clone();
            Box::pin(async move { Ok(Box::new(addresses.into_iter()) as Addrs) })
        }
    }

    fn resolver_for_test(guard: &SsrfGuard, addresses: Vec<SocketAddr>) -> Arc<dyn Resolve> {
        let inner = guard
            .inner
            .as_ref()
            .expect("test guard should expose SSRF internals");
        Arc::new(SsrfDnsResolver {
            acl: Arc::new(inner.acl.clone()),
            inner: Arc::new(StaticDnsResolver { addresses }),
            policy: inner.policy.clone(),
        })
    }

    // Default policy tests (migrated from synctv-ssrf)

    #[test]
    fn test_acl_blocks_private_ipv4() {
        let guard = SsrfGuard::strict_policy();
        // Loopback
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
        // Private ranges
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255))));
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        // Link-local / cloud metadata
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1))));
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))));
        // CGNAT
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))));
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(100, 127, 255, 254))));
        // Current network
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
        // Multicast
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1))));
        // Reserved
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(240, 0, 0, 1))));
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::BROADCAST)));
    }

    #[test]
    fn test_acl_allows_public_ipv4() {
        let guard = SsrfGuard::strict_policy();
        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))));
    }

    #[test]
    fn test_acl_blocks_ipv6() {
        let guard = SsrfGuard::strict_policy();
        assert!(guard.is_ip_blocked(&IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(guard.is_ip_blocked(&IpAddr::V6(Ipv6Addr::UNSPECIFIED)));
        // Unique local
        assert!(guard.is_ip_blocked(&IpAddr::V6(Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 1))));
        assert!(guard.is_ip_blocked(&IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1))));
        // Link-local
        assert!(guard.is_ip_blocked(&IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1))));
        // Multicast
        assert!(guard.is_ip_blocked(&IpAddr::V6(Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 1))));
        // Teredo
        assert!(guard.is_ip_blocked(&IpAddr::V6(Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 1))));
        // 6to4
        assert!(guard.is_ip_blocked(&IpAddr::V6(Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 1))));
    }

    #[test]
    fn test_acl_allows_public_ipv6() {
        let guard = SsrfGuard::strict_policy();
        let google = IpAddr::V6(Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888));
        assert!(!guard.is_ip_blocked(&google));
        let cloudflare = IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111));
        assert!(!guard.is_ip_blocked(&cloudflare));
    }

    #[test]
    fn test_acl_blocks_hostnames() {
        let guard = SsrfGuard::strict_policy();
        assert!(guard.is_host_blocked("localhost"));
        assert!(guard.is_host_blocked("metadata.google.internal"));
        assert!(guard.is_host_blocked("instance-data"));
        assert!(guard.is_host_blocked("metadata.azure"));
    }

    #[test]
    fn test_acl_allows_public_hostnames() {
        let guard = SsrfGuard::strict_policy();
        let acl = guard.acl().expect("strict policy should expose ACL");
        assert!(acl.is_host_allowed("example.com").is_allowed());
        assert!(acl.is_host_allowed("api.bilibili.com").is_allowed());
        assert!(acl.is_host_allowed("github.com").is_allowed());
    }

    #[test]
    fn test_is_ip_blocked() {
        let guard = SsrfGuard::strict_policy();
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
    }

    #[test]
    fn test_ssrf_dns_resolver_creation() {
        assert!(SsrfGuard::strict_policy().dns_resolver().is_some());
    }

    #[test]
    fn test_disabled_policy_disables_ssrf_checks() {
        let guard = SsrfGuard::disabled();
        assert!(guard.acl().is_none());
        assert!(guard.dns_resolver().is_none());
        assert!(!guard.is_host_blocked("localhost"));
        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
    }

    #[test]
    fn test_ipv4_172_range_boundary() {
        let guard = SsrfGuard::strict_policy();
        // 172.15.x.x is NOT private (outside 172.16.0.0/12)
        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(172, 15, 255, 255))));
        // 172.16.0.0 through 172.31.255.255 IS private
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(172, 16, 0, 0))));
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255))));
        // 172.32.x.x is NOT private
        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(172, 32, 0, 0))));
    }

    #[test]
    fn test_ipv4_cgnat_boundary() {
        let guard = SsrfGuard::strict_policy();
        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(100, 63, 255, 255))));
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(100, 64, 0, 0))));
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(100, 127, 255, 255))));
        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(100, 128, 0, 0))));
    }

    // SsrfGuard struct tests

    #[test]
    fn test_guard_is_ip_blocked() {
        let guard = SsrfGuard::strict_policy();
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    #[test]
    fn test_guard_is_host_blocked() {
        let guard = SsrfGuard::strict_policy();
        assert!(guard.is_host_blocked("localhost"));
        assert!(guard.is_host_blocked("metadata.google.internal"));
        assert!(!guard.is_host_blocked("example.com"));
    }

    #[tokio::test]
    async fn test_dns_resolver_allows_private_ip_for_explicit_allowed_host() {
        let guard = SsrfGuard::builder()
            .extra_allowed_host("internal.example".to_string())
            .build();
        let resolver = resolver_for_test(&guard, vec![SocketAddr::from(([10, 0, 0, 42], 443))]);

        let resolved = resolver
            .resolve("internal.example".parse().expect("valid DNS name"))
            .await
            .expect("DNS resolution should succeed")
            .collect::<Vec<_>>();

        assert_eq!(resolved, vec![SocketAddr::from(([10, 0, 0, 42], 443))]);
        assert!(
            guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 42))),
            "host-specific allowlist must not globally allow private IPs"
        );
    }

    #[tokio::test]
    async fn test_dns_resolver_still_blocks_private_ip_for_non_allowlisted_host() {
        let guard = SsrfGuard::strict_policy();
        let resolver = resolver_for_test(&guard, vec![SocketAddr::from(([10, 0, 0, 42], 443))]);

        let resolved = resolver
            .resolve("example.com".parse().expect("valid DNS name"))
            .await
            .expect("DNS resolution should succeed")
            .collect::<Vec<_>>();

        assert!(
            resolved.is_empty(),
            "private DNS results should still be filtered for non-allowlisted hosts"
        );
    }

    #[tokio::test]
    async fn test_dns_resolver_blocks_metadata_ip_for_explicit_allowed_host() {
        let guard = SsrfGuard::builder()
            .extra_allowed_host("internal.example".to_string())
            .build();
        let resolver =
            resolver_for_test(&guard, vec![SocketAddr::from(([169, 254, 169, 254], 80))]);

        let resolved = resolver
            .resolve("internal.example".parse().expect("valid DNS name"))
            .await
            .expect("DNS resolution should succeed")
            .collect::<Vec<_>>();

        assert!(
            resolved.is_empty(),
            "hostname allowlist should not allow metadata IPs"
        );
    }

    #[test]
    fn test_host_context_allows_private_ip_for_explicit_allowed_host() {
        let guard = SsrfGuard::builder()
            .extra_allowed_host("internal.example".to_string())
            .build();
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 42));

        assert!(guard.is_ip_blocked(&ip));
        assert!(!guard.is_ip_blocked_for_host("internal.example", &ip));
        assert!(guard.is_ip_blocked_for_host("example.com", &ip));
    }

    #[test]
    fn test_allowed_ip_range_overrides_private_default() {
        let guard = SsrfGuard::builder()
            .extra_allowed_ip_range(
                "10.0.8.0/24"
                    .parse()
                    .expect("test CIDR must parse successfully"),
            )
            .build();

        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(10, 0, 8, 42))));
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(10, 0, 9, 42))));
    }

    #[test]
    fn test_allow_private_network_targets_allows_non_global_ips() {
        let guard = SsrfGuard::builder()
            .allow_private_network_targets(true)
            .build();

        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))));
        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
    }

    // Builder tests

    #[test]
    fn test_builder_extra_denied_ip_range() {
        // Use a global IP range (Cloudflare's 104.16.0.0/12) to test custom deny rules.
        // Non-global ranges like 203.0.113.0/24 are already blocked by default.
        let guard = SsrfGuard::builder()
            .extra_denied_ip_range(
                "104.16.0.0/12"
                    .parse()
                    .expect("test CIDR must parse successfully"),
            )
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
    fn test_builder_disallow_http() {
        let guard = SsrfGuard::builder().allow_http(false).build();
        let acl = guard.acl().expect("builder policy should expose ACL");
        // HTTP should be disallowed
        assert!(acl.is_scheme_allowed("http").is_denied());
        // HTTPS should still be allowed
        assert!(acl.is_scheme_allowed("https").is_allowed());
    }

    #[test]
    fn test_builder_disallow_https() {
        let guard = SsrfGuard::builder().allow_https(false).build();
        let acl = guard.acl().expect("builder policy should expose ACL");
        // HTTPS should be disallowed
        assert!(acl.is_scheme_allowed("https").is_denied());
        // HTTP should still be allowed
        assert!(acl.is_scheme_allowed("http").is_allowed());
    }
}
