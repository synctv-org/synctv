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
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

type BoxError = Box<dyn Error + Send + Sync>;

/// DNS resolution result rejected by the configured SSRF policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsrfResolutionBlocked {
    host: String,
}

impl SsrfResolutionBlocked {
    fn new(host: impl Into<String>) -> Self {
        Self { host: host.into() }
    }

    /// Hostname whose resolved addresses were rejected.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }
}

impl fmt::Display for SsrfResolutionBlocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "resolved addresses for `{}` are blocked by SSRF policy",
            self.host
        )
    }
}

impl Error for SsrfResolutionBlocked {}

/// Full URL target rejected by the configured SSRF policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SsrfTargetError {
    BlockedHost(String),
    BlockedIp(IpAddr),
    BlockedPort { port: u16 },
}

impl fmt::Display for SsrfTargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlockedHost(host) => {
                write!(f, "target host `{host}` is blocked by SSRF policy")
            }
            Self::BlockedIp(ip) => write!(f, "target IP `{ip}` is blocked by SSRF policy"),
            Self::BlockedPort { port } => {
                write!(f, "target port `{port}` is blocked by SSRF policy")
            }
        }
    }
}

impl Error for SsrfTargetError {}

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
    deny_all: bool,
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

fn parse_builtin_cidr(range: &str) -> Result<IpNet, String> {
    range
        .parse()
        .map_err(|error| format!("built-in SSRF CIDR `{range}` must parse: {error}"))
}

fn valid_acl_config<T, E: std::fmt::Display>(
    result: Result<T, E>,
    context: &str,
) -> Result<T, String> {
    result.map_err(|error| format!("{context}: {error}"))
}

fn parse_ranges(ranges: &[&str]) -> Result<Vec<IpNet>, String> {
    ranges
        .iter()
        .map(|range| parse_builtin_cidr(range))
        .collect()
}

fn contains_ip(ranges: &[IpNet], ip: &IpAddr) -> bool {
    ranges.iter().any(|range| range.contains(ip))
}

impl SsrfPolicy {
    fn is_ip_allowed(&self, ip: &IpAddr) -> bool {
        if self.deny_all {
            return false;
        }

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
        if self.deny_all {
            return true;
        }

        let host = normalize_host(host);
        !self.allowed_hosts.contains(&host) && self.denied_hosts.contains(&host)
    }

    fn allows_non_default_ports_for_ip(&self, ip: &IpAddr) -> bool {
        if self.deny_all {
            return false;
        }

        self.allow_private_network_targets && contains_ip(&self.default_denied_ip_ranges, ip)
            || contains_ip(&self.extra_allowed_ip_ranges, ip)
    }
}

impl Resolve for SsrfDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        if self.acl.is_host_allowed(&host).is_denied() {
            let err: BoxError = Box::new(SsrfResolutionBlocked::new(host));
            return Box::pin(std::future::ready(Err(err)));
        }

        let dns_client = self.inner.clone();
        let policy = self.policy.clone();

        Box::pin(async move {
            let resolved_addrs = dns_client.resolve(name).await?;
            let addresses = resolved_addrs.collect::<Vec<SocketAddr>>();
            let filtered = addresses
                .iter()
                .copied()
                .filter(|addr| policy.is_ip_allowed_for_host(&host, &addr.ip()))
                .collect::<Vec<SocketAddr>>();

            if !addresses.is_empty() && filtered.is_empty() {
                return Err(Box::new(SsrfResolutionBlocked::new(host)) as BoxError);
            }

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

    /// Check if a port is blocked for a concrete IP target.
    ///
    /// When private-network targets or explicit IP ranges are allowed, their
    /// non-default service ports must be reachable as part of the same trusted
    /// deployment decision.
    #[must_use]
    pub fn is_port_blocked_for_ip(&self, port: u16, ip: &IpAddr) -> bool {
        self.inner.as_ref().is_some_and(|inner| {
            !inner.policy.allows_non_default_ports_for_ip(ip)
                && inner.acl.is_port_allowed(port).is_denied()
        })
    }

    /// Check if a port is blocked for a hostname URL target.
    ///
    /// DNS filtering cannot use the URL port because reqwest's resolver API
    /// only receives a hostname. Callers that validate full URLs must run this
    /// check before connecting.
    #[must_use]
    pub fn is_port_blocked_for_host(&self, port: u16, host: &str) -> bool {
        self.inner.as_ref().is_some_and(|inner| {
            if inner.policy.deny_all {
                return true;
            }

            !inner.policy.allowed_hosts.contains(&normalize_host(host))
                && inner.acl.is_port_allowed(port).is_denied()
        })
    }

    /// Validate a URL host and port against host/IP/port SSRF policy.
    ///
    /// Callers that already parsed a URL can use this to keep port policy
    /// consistent across HTTP proxying and provider source validation.
    ///
    /// # Errors
    ///
    /// Returns [`SsrfTargetError`] when the host resolves to a blocked IP
    /// address, matches a blocked host policy, or uses a blocked port.
    pub fn validate_url_target(&self, host: &str, port: u16) -> Result<(), SsrfTargetError> {
        self.validate_url_target_with_optional_default_port(host, port, None)
    }

    /// Validate a URL host and port while allowing the scheme's default port.
    ///
    /// Non-HTTP callers such as RTMP validators can pass their protocol default
    /// port while retaining the same host/IP and custom-port policy.
    ///
    /// # Errors
    ///
    /// Returns [`SsrfTargetError`] when the host resolves to a blocked IP
    /// address, matches a blocked host policy, or uses a blocked non-default
    /// port.
    pub fn validate_url_target_with_default_port(
        &self,
        host: &str,
        port: u16,
        default_port: u16,
    ) -> Result<(), SsrfTargetError> {
        self.validate_url_target_with_optional_default_port(host, port, Some(default_port))
    }

    fn validate_url_target_with_optional_default_port(
        &self,
        host: &str,
        port: u16,
        default_port: Option<u16>,
    ) -> Result<(), SsrfTargetError> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            if self.is_ip_blocked(&ip) {
                return Err(SsrfTargetError::BlockedIp(ip));
            }
            if default_port == Some(port) {
                return Ok(());
            }
            if self.is_port_blocked_for_ip(port, &ip) {
                return Err(SsrfTargetError::BlockedPort { port });
            }
            return Ok(());
        }

        if self.is_host_blocked(host) {
            return Err(SsrfTargetError::BlockedHost(host.to_string()));
        }
        if default_port == Some(port) {
            return Ok(());
        }
        if self.is_port_blocked_for_host(port, host) {
            return Err(SsrfTargetError::BlockedPort { port });
        }
        Ok(())
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
    pub fn build(self) -> SsrfGuard {
        match self.try_build() {
            Ok(guard) => guard,
            Err(error) => {
                tracing::error!(
                    error = %error,
                    "Invalid SSRF guard configuration; falling back to deny-all policy"
                );
                SsrfGuard::deny_all_fallback(&error)
            }
        }
    }

    fn try_build(self) -> Result<SsrfGuard, String> {
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
            let range = parse_builtin_cidr(range_str)?;
            builder = valid_acl_config(builder.add_denied_ip_range(range), "valid IP range")?;
        }

        // Apply denied hosts that were not explicitly allowlisted.
        for host in denied_hosts.difference(&allowed_hosts) {
            builder = valid_acl_config(builder.add_denied_host(host.clone()), "valid hostname")?;
        }

        // Apply extra denied ranges to the underlying ACL where they cannot
        // conflict with explicit allow ranges. The guard's own policy below is
        // authoritative for IP decisions.
        for range in &extra_denied_ip_ranges {
            builder = valid_acl_config(builder.add_denied_ip_range(*range), "valid IP range")?;
        }

        // Apply extra allowed hosts
        for host in &allowed_hosts {
            builder = valid_acl_config(builder.add_allowed_host(host.clone()), "valid hostname")?;
        }

        let acl = builder
            .non_global_ip_ranges(allow_non_global_ip_ranges)
            .ip_acl_default(true)
            .host_acl_default(true)
            .http(self.allow_http)
            .https(self.allow_https)
            .try_build();
        let acl = valid_acl_config(acl, "SSRF ACL configuration is valid")?;

        let mut default_denied_ip_ranges = parse_ranges(DEFAULT_NON_GLOBAL_DENIED_RANGES)?;
        default_denied_ip_ranges.extend(parse_ranges(DEFAULT_EXTRA_DENIED_RANGES)?);

        let policy = Arc::new(SsrfPolicy {
            deny_all: false,
            allow_private_network_targets: self.allow_private_network_targets,
            default_denied_ip_ranges,
            host_allowlist_denied_ip_ranges: parse_ranges(HOST_ALLOWLIST_DENIED_RANGES)?,
            extra_denied_ip_ranges,
            extra_allowed_ip_ranges,
            denied_hosts,
            allowed_hosts,
        });

        Ok(SsrfGuard::from_acl_and_policy(acl, policy))
    }
}

impl SsrfGuard {
    /// Build a guard from a validated ACL and policy, wiring up the
    /// system DNS resolver. Shared by [`SsrfGuardBuilder::try_build`] and
    /// [`SsrfGuard::deny_all_fallback`].
    fn from_acl_and_policy(acl: HttpAcl, policy: Arc<SsrfPolicy>) -> Self {
        let resolver = Arc::new(SsrfDnsResolver {
            acl: Arc::new(acl.clone()),
            inner: Arc::new(SystemDnsResolver),
            policy: policy.clone(),
        }) as Arc<dyn Resolve>;

        Self {
            inner: Some(Arc::new(SsrfGuardInner {
                acl,
                resolver,
                policy,
            })),
        }
    }

    fn deny_all_fallback(reason: &str) -> Self {
        tracing::error!(
            reason,
            "Using deny-all SSRF fallback after configuration failure"
        );

        let acl = HttpAcl::builder()
            .try_build()
            .expect("default SSRF ACL configuration must build");
        let policy = Arc::new(SsrfPolicy {
            deny_all: true,
            allow_private_network_targets: false,
            default_denied_ip_ranges: Vec::new(),
            host_allowlist_denied_ip_ranges: Vec::new(),
            extra_denied_ip_ranges: Vec::new(),
            extra_allowed_ip_ranges: Vec::new(),
            denied_hosts: HashSet::new(),
            allowed_hosts: HashSet::new(),
        });

        Self::from_acl_and_policy(acl, policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    type TestResult<T = ()> = Result<T, String>;

    struct StaticDnsResolver {
        addresses: Vec<SocketAddr>,
    }

    impl Resolve for StaticDnsResolver {
        fn resolve(&self, _name: Name) -> Resolving {
            let addresses = self.addresses.clone();
            Box::pin(async move { Ok(Box::new(addresses.into_iter()) as Addrs) })
        }
    }

    fn resolver_for_test(
        guard: &SsrfGuard,
        addresses: Vec<SocketAddr>,
    ) -> TestResult<Arc<dyn Resolve>> {
        let inner = guard
            .inner
            .as_ref()
            .ok_or_else(|| "test guard should expose SSRF internals".to_string())?;
        Ok(Arc::new(SsrfDnsResolver {
            acl: Arc::new(inner.acl.clone()),
            inner: Arc::new(StaticDnsResolver { addresses }),
            policy: inner.policy.clone(),
        }))
    }

    fn parse_test_dns_name(value: &str) -> TestResult<Name> {
        value
            .parse()
            .map_err(|error| format!("valid DNS name `{value}` should parse: {error}"))
    }

    fn parse_test_cidr(value: &str) -> TestResult<IpNet> {
        value
            .parse()
            .map_err(|error| format!("test CIDR `{value}` should parse: {error}"))
    }

    fn acl_for_test(guard: &SsrfGuard) -> TestResult<&HttpAcl> {
        guard
            .acl()
            .ok_or_else(|| "test guard should expose ACL".to_string())
    }

    fn assert_resolution_blocked(
        result: Result<Addrs, BoxError>,
        expected_host: &str,
    ) -> TestResult {
        let Err(error) = result else {
            return Err(format!(
                "DNS results for `{expected_host}` should fail with a typed SSRF error"
            ));
        };
        let blocked = error
            .downcast_ref::<SsrfResolutionBlocked>()
            .ok_or_else(|| "DNS resolution error should expose typed SSRF denial".to_string())?;
        assert_eq!(blocked.host(), expected_host);
        Ok(())
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
    fn test_acl_allows_public_hostnames() -> TestResult {
        let guard = SsrfGuard::strict_policy();
        let acl = acl_for_test(&guard)?;
        assert!(acl.is_host_allowed("example.com").is_allowed());
        assert!(acl.is_host_allowed("api.bilibili.com").is_allowed());
        assert!(acl.is_host_allowed("github.com").is_allowed());
        Ok(())
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
    async fn test_dns_resolver_allows_private_ip_for_explicit_allowed_host() -> TestResult {
        let guard = SsrfGuard::builder()
            .extra_allowed_host("internal.example".to_string())
            .build();
        let dns_resolver =
            resolver_for_test(&guard, vec![SocketAddr::from(([10, 0, 0, 42], 443))])?;

        let resolved_addrs = dns_resolver
            .resolve(parse_test_dns_name("internal.example")?)
            .await
            .map_err(|error| format!("DNS resolution should succeed: {error}"))?
            .collect::<Vec<_>>();

        assert_eq!(
            resolved_addrs,
            vec![SocketAddr::from(([10, 0, 0, 42], 443))]
        );
        assert!(
            guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 42))),
            "host-specific allowlist must not globally allow private IPs"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_dns_resolver_still_blocks_private_ip_for_non_allowlisted_host() -> TestResult {
        let guard = SsrfGuard::strict_policy();
        let resolver = resolver_for_test(&guard, vec![SocketAddr::from(([10, 0, 0, 42], 443))])?;

        let result = resolver.resolve(parse_test_dns_name("example.com")?).await;
        assert_resolution_blocked(result, "example.com")
    }

    #[tokio::test]
    async fn test_dns_resolver_allows_public_ip_with_zero_port() -> TestResult {
        let guard = SsrfGuard::strict_policy();
        let dns_resolver = resolver_for_test(&guard, vec![SocketAddr::from(([8, 8, 8, 8], 0))])?;

        let resolved_addrs = dns_resolver
            .resolve(parse_test_dns_name("example.com")?)
            .await
            .map_err(|error| {
                format!("DNS resolution should accept public IPs with port 0: {error}")
            })?
            .collect::<Vec<_>>();

        assert_eq!(resolved_addrs, vec![SocketAddr::from(([8, 8, 8, 8], 0))]);
        Ok(())
    }

    #[tokio::test]
    async fn test_dns_resolver_blocks_metadata_ip_for_explicit_allowed_host() -> TestResult {
        let guard = SsrfGuard::builder()
            .extra_allowed_host("internal.example".to_string())
            .build();
        let resolver =
            resolver_for_test(&guard, vec![SocketAddr::from(([169, 254, 169, 254], 80))])?;

        let result = resolver
            .resolve(parse_test_dns_name("internal.example")?)
            .await;
        assert_resolution_blocked(result, "internal.example")
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
    fn test_allowed_ip_range_overrides_private_default() -> TestResult {
        let guard = SsrfGuard::builder()
            .extra_allowed_ip_range(parse_test_cidr("10.0.8.0/24")?)
            .build();

        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(10, 0, 8, 42))));
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(10, 0, 9, 42))));
        Ok(())
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

    #[test]
    fn test_allow_private_network_targets_allows_non_default_ports_for_private_ips() {
        let guard = SsrfGuard::builder()
            .allow_private_network_targets(true)
            .build();

        assert!(!guard.is_port_blocked_for_ip(18000, &IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(!guard.is_port_blocked_for_ip(15244, &IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(guard.is_port_blocked_for_ip(18000, &IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    #[test]
    fn test_extra_allowed_ip_range_allows_non_default_ports_for_that_range() -> TestResult {
        let guard = SsrfGuard::builder()
            .extra_allowed_ip_range(parse_test_cidr("10.0.8.0/24")?)
            .build();

        assert!(!guard.is_port_blocked_for_ip(8080, &IpAddr::V4(Ipv4Addr::new(10, 0, 8, 42))));
        assert!(guard.is_port_blocked_for_ip(8080, &IpAddr::V4(Ipv4Addr::new(10, 0, 9, 42))));
        Ok(())
    }

    #[test]
    fn test_hostname_port_acl_blocks_non_default_public_ports() {
        let guard = SsrfGuard::strict_policy();

        assert!(guard.is_port_blocked_for_host(25, "public.example"));
        assert!(!guard.is_port_blocked_for_host(443, "public.example"));
    }

    #[test]
    fn test_explicit_allowed_host_allows_non_default_ports() {
        let guard = SsrfGuard::builder()
            .extra_allowed_host("media.internal".to_string())
            .build();

        assert!(!guard.is_port_blocked_for_host(18000, "media.internal"));
        assert!(guard.is_port_blocked_for_host(18000, "public.example"));
    }

    #[test]
    fn test_validate_url_target_uses_host_aware_port_policy() {
        let guard = SsrfGuard::builder()
            .extra_allowed_host("media.internal".to_string())
            .build();

        assert_eq!(guard.validate_url_target("media.internal", 18000), Ok(()));
        assert_eq!(
            guard.validate_url_target("public.example", 18000),
            Err(SsrfTargetError::BlockedPort { port: 18000 })
        );
        assert_eq!(guard.validate_url_target("public.example", 443), Ok(()));
    }

    #[test]
    fn test_validate_url_target_with_default_port_allows_protocol_default() {
        let guard = SsrfGuard::strict_policy();

        assert_eq!(
            guard.validate_url_target_with_default_port("public.example", 1935, 1935),
            Ok(())
        );
        assert_eq!(
            guard.validate_url_target_with_default_port("public.example", 18000, 1935),
            Err(SsrfTargetError::BlockedPort { port: 18000 })
        );
    }

    #[test]
    fn test_validate_url_target_reports_blocked_literal_ip() {
        let guard = SsrfGuard::strict_policy();

        assert_eq!(
            guard.validate_url_target("127.0.0.1", 80),
            Err(SsrfTargetError::BlockedIp(IpAddr::V4(Ipv4Addr::LOCALHOST)))
        );
    }

    #[tokio::test]
    async fn test_invalid_builder_config_uses_deny_all_fallback() -> TestResult {
        let guard = SsrfGuard::builder()
            .extra_allowed_host("invalid host name".to_string())
            .build();
        let public_ip = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
        let resolver = resolver_for_test(&guard, vec![SocketAddr::from(([8, 8, 8, 8], 443))])?;

        assert!(guard.acl().is_some());
        assert!(guard.dns_resolver().is_some());
        assert!(guard.is_host_blocked("example.com"));
        assert!(guard.is_ip_blocked(&public_ip));
        assert!(guard.is_ip_blocked_for_host("example.com", &public_ip));
        assert!(guard.is_port_blocked_for_host(443, "example.com"));

        let result = resolver.resolve(parse_test_dns_name("example.com")?).await;
        assert_resolution_blocked(result, "example.com")
    }

    // Builder tests

    #[test]
    fn test_builder_extra_denied_ip_range() -> TestResult {
        // Use a global IP range (Cloudflare's 104.16.0.0/12) to test custom deny rules.
        // Non-global ranges like 203.0.113.0/24 are already blocked by default.
        let guard = SsrfGuard::builder()
            .extra_denied_ip_range(parse_test_cidr("104.16.0.0/12")?)
            .build();
        // Custom denied range is blocked
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(104, 16, 0, 1))));
        // Default blocks still apply
        assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
        // Public IPs outside the denied range still allowed
        assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        Ok(())
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
    fn test_builder_disallow_http() -> TestResult {
        let guard = SsrfGuard::builder().allow_http(false).build();
        let acl = acl_for_test(&guard)?;
        // HTTP should be disallowed
        assert!(acl.is_scheme_allowed("http").is_denied());
        // HTTPS should still be allowed
        assert!(acl.is_scheme_allowed("https").is_allowed());
        Ok(())
    }

    #[test]
    fn test_builder_disallow_https() -> TestResult {
        let guard = SsrfGuard::builder().allow_https(false).build();
        let acl = acl_for_test(&guard)?;
        // HTTPS should be disallowed
        assert!(acl.is_scheme_allowed("https").is_denied());
        // HTTP should still be allowed
        assert!(acl.is_scheme_allowed("http").is_allowed());
        Ok(())
    }
}
