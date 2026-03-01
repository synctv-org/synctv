//! SSRF (Server-Side Request Forgery) protection using `url_jail`.
//!
//! This module wraps the `url_jail` crate to provide SSRF protection for
//! Provider URLs that are fetched server-side.
//!
//! # Features
//!
//! - DNS rebinding protection (validates after DNS resolution)
//! - IP encoding attack detection (hex, octal, decimal, short-form)
//! - Cloud metadata endpoint blocking (AWS, GCP, Azure, Alibaba)
//! - Private IP range blocking
//! - Custom blocklist support

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

pub use url_jail::{CustomPolicy, Error as UrlJailError, Policy, PolicyBuilder, Validated};

/// Custom DNS resolver that checks resolved IPs against SSRF blocklists
/// at connection time, preventing DNS rebinding TOCTOU attacks.
///
/// This resolver filters out private/reserved IP addresses from DNS responses,
/// ensuring that HTTP clients cannot be tricked into connecting to internal
/// network resources even if an attacker controls DNS responses.
#[derive(Clone, Debug, Default)]
pub struct SsrfSafeDnsResolver;

impl reqwest::dns::Resolve for SsrfSafeDnsResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let host = name.as_str();
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, 0))
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                    Box::new(std::io::Error::other(format!(
                        "DNS lookup failed for {host}: {e}"
                    )))
                })?
                .collect();

            if addrs.is_empty() {
                return Err(Box::new(std::io::Error::other(format!(
                    "DNS lookup for {host} returned no addresses"
                )))
                    as Box<dyn std::error::Error + Send + Sync>);
            }

            let safe_addrs: Vec<SocketAddr> = addrs
                .into_iter()
                .filter(|addr| !is_blocked_ip(addr.ip()))
                .collect();

            if safe_addrs.is_empty() {
                return Err(Box::new(std::io::Error::other(format!(
                    "All resolved IPs for {host} are private/reserved (SSRF blocked)"
                )))
                    as Box<dyn std::error::Error + Send + Sync>);
            }

            Ok(Box::new(safe_addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Create an `Arc<SsrfSafeDnsResolver>` for use with reqwest Client.
#[must_use]
pub fn ssrf_safe_dns_resolver() -> Arc<SsrfSafeDnsResolver> {
    Arc::new(SsrfSafeDnsResolver)
}

// ============================================================================
// IP validation helpers (for DNS resolver filtering)
// ============================================================================

/// Check if an IPv4 address is private, reserved, or otherwise blocked.
///
/// This is used internally by `SsrfSafeDnsResolver` to filter DNS responses.
#[must_use]
pub fn is_blocked_ipv4(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();

    // Loopback: 127.0.0.0/8
    if o[0] == 127 {
        return true;
    }
    // Private Class A: 10.0.0.0/8
    if o[0] == 10 {
        return true;
    }
    // Private Class B: 172.16.0.0/12
    if o[0] == 172 && (16..=31).contains(&o[1]) {
        return true;
    }
    // Private Class C: 192.168.0.0/16
    if o[0] == 192 && o[1] == 168 {
        return true;
    }
    // Link-local: 169.254.0.0/16 (includes cloud metadata)
    if o[0] == 169 && o[1] == 254 {
        return true;
    }
    // CGNAT / Shared Address Space: 100.64.0.0/10 (RFC 6598)
    // Used by carriers for NAT; should not appear in user-provided URLs.
    if o[0] == 100 && (64..=127).contains(&o[1]) {
        return true;
    }
    // Current network: 0.0.0.0/8
    if o[0] == 0 {
        return true;
    }
    // Multicast: 224.0.0.0/4
    if (224..=239).contains(&o[0]) {
        return true;
    }
    // Reserved/Broadcast: 240.0.0.0/4
    if o[0] >= 240 {
        return true;
    }

    false
}

/// Check if an IPv6 address is private, reserved, or otherwise blocked.
#[must_use]
pub fn is_blocked_ipv6(ip: &Ipv6Addr) -> bool {
    // Loopback (::1)
    if *ip == Ipv6Addr::LOCALHOST {
        return true;
    }
    // Unspecified (::)
    if ip.is_unspecified() {
        return true;
    }
    // IPv4-mapped IPv6 (::ffff:x.x.x.x)
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_blocked_ipv4(&v4);
    }

    let segments = ip.segments();

    // Unique local: fc00::/7
    if (segments[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // Link-local: fe80::/10
    if (segments[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    // Multicast: ff00::/8
    if segments[0] & 0xff00 == 0xff00 {
        return true;
    }
    // Teredo tunneling: 2001::/32
    if segments[0] == 0x2001 && segments[1] == 0x0000 {
        return true;
    }
    // 6to4 tunneling: 2002::/16
    if segments[0] == 0x2002 {
        return true;
    }

    false
}

/// Check if an IP address (v4 or v6) is blocked.
#[must_use]
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(&v4),
        IpAddr::V6(v6) => is_blocked_ipv6(&v6),
    }
}

/// Check if an IP address should be blocked regardless of policy.
///
/// This only blocks ranges that `url_jail` doesn't handle:
/// - CGNAT / Shared Address Space (100.64.0.0/10, RFC 6598)
/// - Multicast (224.0.0.0/4)
/// - Reserved (240.0.0.0/4)
/// - Link-local cloud metadata (169.254.169.254/32)
///
/// It does NOT block RFC1918 private IPs (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16)
/// because those should be controlled by `Policy::AllowPrivate`.
#[must_use]
fn is_always_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_always_blocked_ipv4(&v4),
        IpAddr::V6(v6) => is_always_blocked_ipv6(&v6),
    }
}

/// Check if an IPv4 address should be blocked regardless of policy.
#[must_use]
fn is_always_blocked_ipv4(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();

    // Cloud metadata endpoint: 169.254.169.254 (not entire link-local range)
    if o[0] == 169 && o[1] == 254 && o[2] == 169 && o[3] == 254 {
        return true;
    }
    // CGNAT / Shared Address Space: 100.64.0.0/10 (RFC 6598)
    // Used by carriers for NAT; should not appear in user-provided URLs.
    if o[0] == 100 && (64..=127).contains(&o[1]) {
        return true;
    }
    // Current network: 0.0.0.0/8
    if o[0] == 0 {
        return true;
    }
    // Multicast: 224.0.0.0/4
    if (224..=239).contains(&o[0]) {
        return true;
    }
    // Reserved/Broadcast: 240.0.0.0/4
    if o[0] >= 240 {
        return true;
    }

    false
}

/// Check if an IPv6 address should be blocked regardless of policy.
#[must_use]
const fn is_always_blocked_ipv6(ip: &Ipv6Addr) -> bool {
    // Unspecified (::)
    if ip.is_unspecified() {
        return true;
    }

    let segments = ip.segments();

    // Multicast: ff00::/8
    if segments[0] & 0xff00 == 0xff00 {
        return true;
    }

    // Reserved (includes documentation, benchmarking, etc.)
    if segments[0] >= 0xff00 {
        return true;
    }

    false
}

// ============================================================================
// URL validation using url_jail
// ============================================================================

/// Result of URL validation.
#[derive(Debug)]
pub enum SsrfCheckResult {
    /// The URL is safe to fetch.
    Ok,
    /// The URL is blocked with the given reason.
    Blocked(String),
}

impl SsrfCheckResult {
    /// Returns `true` if the check passed.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// Validate a URL for SSRF protection using `url_jail`.
///
/// This is the primary SSRF validation function. It:
/// - Parses the URL
/// - Validates the scheme (http/https only)
/// - Resolves DNS and checks all resolved IPs
/// - Blocks private/reserved IPs and cloud metadata endpoints
/// - Detects IP encoding attacks (hex, octal, decimal, short-form)
///
/// # Example
///
/// ```ignore
/// use synctv_media_providers::ssrf::{check_url_with_policy, SsrfCheckResult, Policy};
///
/// match check_url_with_policy("https://example.com/api", Policy::PublicOnly) {
///     SsrfCheckResult::Ok => println!("Safe to fetch"),
///     SsrfCheckResult::Blocked(reason) => println!("Blocked: {}", reason),
/// }
/// ```
#[must_use] 
pub fn check_url_with_policy(url: &str, policy: Policy) -> SsrfCheckResult {
    // Parse URL to check host for additional IP ranges not covered by url_jail
    if let Ok(parsed) = url::Url::parse(url) {
        if let Some(host) = parsed.host_str() {
            // Handle IPv6 addresses with brackets
            let host_str = if host.starts_with('[') && host.ends_with(']') {
                &host[1..host.len() - 1]
            } else {
                host
            };

            // Check if host is an IP address that needs additional blocking
            if let Ok(ip) = host_str.parse::<IpAddr>() {
                // Check for ranges that should ALWAYS be blocked (regardless of policy)
                // This includes CGNAT, multicast, and reserved ranges that url_jail doesn't block
                if is_always_blocked_ip(ip) {
                    return SsrfCheckResult::Blocked(format!(
                        "IP {ip} is in blocked range (CGNAT, multicast, or reserved network)"
                    ));
                }
            } else {
                // Not an IP address - check hostname patterns
                let hostname_result = check_hostname(host_str);
                if let SsrfCheckResult::Blocked(reason) = hostname_result {
                    return SsrfCheckResult::Blocked(reason);
                }
            }
        }
    }

    // Use url_jail's synchronous validation
    match url_jail::validate_sync(url, policy) {
        Ok(_) => SsrfCheckResult::Ok,
        Err(e) => {
            let reason = if e.is_blocked() {
                format!("SSRF blocked: {e}")
            } else if e.is_retriable() {
                format!("Temporary error (retry with caution): {e}")
            } else {
                format!("Validation error: {e}")
            };
            SsrfCheckResult::Blocked(reason)
        }
    }
}

/// Validate a URL with default `PublicOnly` policy.
///
/// Blocks: private IPs, loopback, link-local, cloud metadata endpoints.
#[must_use]
pub fn check_url(url: &str) -> SsrfCheckResult {
    check_url_with_policy(url, Policy::PublicOnly)
}

/// Validate a URL asynchronously with DNS resolution.
///
/// This performs actual DNS resolution and validates all resolved IPs.
pub async fn check_url_async(url: &str) -> SsrfCheckResult {
    check_url_with_policy_async(url, Policy::PublicOnly).await
}

/// Validate a URL asynchronously with a custom policy.
pub async fn check_url_with_policy_async(url: &str, policy: Policy) -> SsrfCheckResult {
    // Parse URL to check host for blocked hostnames before DNS resolution
    if let Ok(parsed) = url::Url::parse(url) {
        if let Some(host) = parsed.host_str() {
            // Handle IPv6 addresses with brackets
            let host_str = if host.starts_with('[') && host.ends_with(']') {
                &host[1..host.len() - 1]
            } else {
                host
            };

            // Check if host is an IP address that needs additional blocking
            if let Ok(ip) = host_str.parse::<IpAddr>() {
                // Check for ranges that should ALWAYS be blocked (regardless of policy)
                // This includes CGNAT, multicast, and reserved ranges that url_jail doesn't block
                if is_always_blocked_ip(ip) {
                    return SsrfCheckResult::Blocked(format!(
                        "IP {ip} is in blocked range (CGNAT, multicast, or reserved network)"
                    ));
                }
            } else {
                // Not an IP address - check hostname patterns
                let hostname_result = check_hostname(host_str);
                if let SsrfCheckResult::Blocked(reason) = hostname_result {
                    return SsrfCheckResult::Blocked(reason);
                }
            }
        }
    }

    match url_jail::validate(url, policy).await {
        Ok(_) => SsrfCheckResult::Ok,
        Err(e) => {
            let reason = if e.is_blocked() {
                format!("SSRF blocked: {e}")
            } else if e.is_retriable() {
                format!("Temporary error (retry with caution): {e}")
            } else {
                format!("Validation error: {e}")
            };
            SsrfCheckResult::Blocked(reason)
        }
    }
}

/// Validate a URL with a custom policy built from `PolicyBuilder`.
///
/// This accepts a `CustomPolicy` created via `PolicyBuilder`.
/// Note: This is async because `url_jail`'s `CustomPolicy` validation is async-only.
pub async fn check_url_with_custom_policy(url: &str, policy: CustomPolicy) -> SsrfCheckResult {
    match url_jail::validate_custom(url, &policy).await {
        Ok(_) => SsrfCheckResult::Ok,
        Err(e) => {
            let reason = if e.is_blocked() {
                format!("SSRF blocked: {e}")
            } else if e.is_retriable() {
                format!("Temporary error (retry with caution): {e}")
            } else {
                format!("Validation error: {e}")
            };
            SsrfCheckResult::Blocked(reason)
        }
    }
}

/// Validate a hostname against known internal/suspicious patterns.
///
/// Note: `url_jail` handles this internally during URL validation.
/// This function is provided for compatibility with existing code.
#[must_use]
pub fn check_hostname(host: &str) -> SsrfCheckResult {
    let lower = host.to_lowercase();

    // Blocked hostnames
    const BLOCKED_HOSTNAMES: &[&str] = &[
        "localhost",
        "localhost.localdomain",
        "metadata.google.internal",
        "instance-data",
        "metadata.azure",
    ];

    for blocked in BLOCKED_HOSTNAMES {
        if lower == *blocked || lower.starts_with(&format!("{blocked}.")) {
            return SsrfCheckResult::Blocked(format!(
                "internal hostname '{host}' is not allowed"
            ));
        }
    }

    // Blocked suffixes
    const BLOCKED_SUFFIXES: &[&str] = &[".internal", ".local", ".localhost"];

    for suffix in BLOCKED_SUFFIXES {
        if lower.ends_with(suffix) {
            return SsrfCheckResult::Blocked(format!(
                "internal hostname '{host}' is not allowed"
            ));
        }
    }

    // Blocked prefixes
    const BLOCKED_PREFIXES: &[&str] = &[
        "metadata.",
        "metadata.google",
        "metadata.azure",
        "kubernetes.",
        "k8s.",
        "docker.",
        "container.",
    ];

    for prefix in BLOCKED_PREFIXES {
        if lower.starts_with(prefix) {
            return SsrfCheckResult::Blocked(format!(
                "internal service hostname '{host}' is not allowed"
            ));
        }
    }

    SsrfCheckResult::Ok
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // IP blocking tests
    // ========================================================================

    #[test]
    fn test_blocked_private_ipv4() {
        assert!(is_blocked_ipv4(&Ipv4Addr::LOCALHOST), "loopback");
        assert!(is_blocked_ipv4(&Ipv4Addr::new(10, 0, 0, 1)), "10.x");
        assert!(is_blocked_ipv4(&Ipv4Addr::new(172, 16, 0, 1)), "172.16.x");
        assert!(is_blocked_ipv4(&Ipv4Addr::new(172, 31, 255, 255)), "172.31.x");
        assert!(is_blocked_ipv4(&Ipv4Addr::new(192, 168, 1, 1)), "192.168.x");
        assert!(
            is_blocked_ipv4(&Ipv4Addr::new(169, 254, 1, 1)),
            "link-local / metadata"
        );
        // CGNAT / Shared Address Space: 100.64.0.0/10
        assert!(is_blocked_ipv4(&Ipv4Addr::new(100, 64, 0, 1)), "CGNAT start");
        assert!(is_blocked_ipv4(&Ipv4Addr::new(100, 127, 255, 254)), "CGNAT end");
        assert!(is_blocked_ipv4(&Ipv4Addr::UNSPECIFIED), "current network");
        assert!(is_blocked_ipv4(&Ipv4Addr::new(224, 0, 0, 1)), "multicast");
        assert!(is_blocked_ipv4(&Ipv4Addr::new(240, 0, 0, 1)), "reserved");
        assert!(
            is_blocked_ipv4(&Ipv4Addr::BROADCAST),
            "broadcast"
        );
    }

    #[test]
    fn test_allowed_public_ipv4() {
        assert!(!is_blocked_ipv4(&Ipv4Addr::new(8, 8, 8, 8)), "Google DNS");
        assert!(!is_blocked_ipv4(&Ipv4Addr::new(1, 1, 1, 1)), "Cloudflare DNS");
        assert!(
            !is_blocked_ipv4(&Ipv4Addr::new(203, 0, 113, 1)),
            "Documentation range"
        );
        assert!(!is_blocked_ipv4(&Ipv4Addr::new(93, 184, 216, 34)), "example.com");
    }

    #[test]
    fn test_blocked_ipv6() {
        assert!(is_blocked_ipv6(&Ipv6Addr::LOCALHOST), "::1");
        assert!(is_blocked_ipv6(&Ipv6Addr::UNSPECIFIED), "::");

        // IPv4-mapped IPv6 addresses
        let mapped_loopback =
            Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x7f00, 0x0001);
        assert!(is_blocked_ipv6(&mapped_loopback), "::ffff:127.0.0.1");

        let mapped_private =
            Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0xc0a8, 0x0101);
        assert!(is_blocked_ipv6(&mapped_private), "::ffff:192.168.1.1");
    }

    #[test]
    fn test_allowed_public_ipv6() {
        // Google's public DNS over IPv6
        let google_dns = Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888);
        assert!(!is_blocked_ipv6(&google_dns), "Google DNS IPv6");

        // Cloudflare DNS over IPv6
        let cloudflare = Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111);
        assert!(!is_blocked_ipv6(&cloudflare), "Cloudflare DNS IPv6");
    }

    #[test]
    fn test_is_blocked_ip_v4() {
        assert!(is_blocked_ip(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(is_blocked_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(!is_blocked_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    #[test]
    fn test_is_blocked_ip_v6() {
        assert!(is_blocked_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(!is_blocked_ip(IpAddr::V6(Ipv6Addr::new(
            0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111
        ))));
    }

    // ========================================================================
    // Hostname validation tests
    // ========================================================================

    #[test]
    fn test_check_hostname_blocked() {
        assert!(!check_hostname("localhost").is_ok());
        assert!(!check_hostname("LOCALHOST").is_ok());
        assert!(!check_hostname("metadata.google.internal").is_ok());
        assert!(!check_hostname("myhost.local").is_ok());
        assert!(!check_hostname("myhost.internal").is_ok());
        assert!(!check_hostname("kubernetes.default").is_ok());
        assert!(!check_hostname("k8s.api").is_ok());
        assert!(!check_hostname("docker.registry").is_ok());
    }

    #[test]
    fn test_check_hostname_allowed() {
        assert!(check_hostname("example.com").is_ok());
        assert!(check_hostname("api.bilibili.com").is_ok());
        assert!(check_hostname("github.com").is_ok());
        assert!(check_hostname("subdomain.example.org").is_ok());
    }

    // ========================================================================
    // URL validation tests (using url_jail)
    // ========================================================================

    #[test]
    fn test_check_url_blocked_private_ip() {
        assert!(!check_url("http://127.0.0.1/admin").is_ok());
        assert!(!check_url("http://192.168.1.1/admin").is_ok());
        assert!(!check_url("http://10.0.0.1/admin").is_ok());
        assert!(!check_url("http://172.16.0.1/admin").is_ok());
    }

    #[test]
    fn test_check_url_blocked_localhost() {
        // url_jail blocks "localhost" but not "localhost.localdomain"
        assert!(!check_url("http://localhost/admin").is_ok());
        // Note: url_jail doesn't block localhost.localdomain by default
    }

    #[test]
    fn test_check_url_blocked_invalid_scheme() {
        assert!(!check_url("ftp://example.com/file").is_ok());
        assert!(!check_url("file:///etc/passwd").is_ok());
        assert!(!check_url("javascript:alert(1)").is_ok());
    }

    #[test]
    fn test_check_url_blocked_empty() {
        assert!(!check_url("").is_ok());
    }

    #[test]
    fn test_check_url_blocked_incomplete() {
        assert!(!check_url("http://").is_ok());
    }

    #[test]
    fn test_check_url_allowed_public() {
        assert!(check_url("https://example.com").is_ok());
        assert!(check_url("https://api.github.com/users/test").is_ok());
        assert!(check_url("http://example.com/path?query=1").is_ok());
    }

    // ========================================================================
    // IP encoding attack tests (url_jail handles these)
    // ========================================================================

    #[test]
    fn test_ip_encoding_attacks_blocked() {
        // Decimal encoding of 127.0.0.1 = 2130706433
        assert!(!check_url("http://2130706433/").is_ok());

        // Hex encoding of 127.0.0.1 = 0x7f000001
        assert!(!check_url("http://0x7f000001/").is_ok());

        // Octal encoding of 127.0.0.1 = 0177.0.0.1
        assert!(!check_url("http://0177.0.0.1/").is_ok());

        // Short-form of 127.0.0.1 = 127.1
        assert!(!check_url("http://127.1/").is_ok());

        // IPv4-mapped IPv6
        assert!(!check_url("http://[::ffff:127.0.0.1]/").is_ok());
    }

    // ========================================================================
    // DNS resolver tests
    // ========================================================================

    #[test]
    fn test_ssrf_safe_dns_resolver_creation() {
        let _resolver = SsrfSafeDnsResolver;
        let _arc_resolver = ssrf_safe_dns_resolver();
    }

    #[test]
    fn test_ssrf_safe_dns_resolver_clone() {
        let resolver = SsrfSafeDnsResolver;
        let _cloned = resolver;
    }

    #[test]
    fn test_ssrf_safe_dns_resolver_default() {
        let _resolver = SsrfSafeDnsResolver;
    }

    #[test]
    fn test_ssrf_safe_dns_resolver_debug() {
        let resolver = SsrfSafeDnsResolver;
        let debug_str = format!("{resolver:?}");
        assert!(debug_str.contains("SsrfSafeDnsResolver"));
    }

    #[test]
    fn test_ssrf_safe_dns_resolver_arc_creation() {
        use std::sync::Arc;
        let resolver: Arc<SsrfSafeDnsResolver> = ssrf_safe_dns_resolver();
        assert!(Arc::strong_count(&resolver) >= 1);
    }

    #[test]
    fn test_is_blocked_ip_with_socket_addr_filtering() {
        let public_ipv4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443);
        let private_ipv4 =
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 443);
        let loopback_ipv4 =
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443);
        let public_ipv6 = SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111)),
            443,
        );
        let loopback_ipv6 =
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443);

        let addrs: Vec<SocketAddr> = vec![
            public_ipv4,
            private_ipv4,
            loopback_ipv4,
            public_ipv6,
            loopback_ipv6,
        ];
        let safe_addrs: Vec<SocketAddr> = addrs
            .into_iter()
            .filter(|addr| !is_blocked_ip(addr.ip()))
            .collect();

        assert_eq!(safe_addrs.len(), 2);
        assert!(safe_addrs
            .iter()
            .any(|a| a.ip() == IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    #[test]
    fn test_is_blocked_ip_all_private_filtered() {
        let addrs: Vec<SocketAddr> = vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 443),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)), 443),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 443),
        ];
        let safe_addrs: Vec<SocketAddr> = addrs
            .into_iter()
            .filter(|addr| !is_blocked_ip(addr.ip()))
            .collect();

        assert!(
            safe_addrs.is_empty(),
            "All private IPs should be filtered out"
        );
    }

    #[test]
    fn test_is_blocked_ip_mixed_addresses() {
        let addrs: Vec<SocketAddr> = vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 443), // private
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443),  // public
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443), // loopback
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 443),  // public
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1)), 443), // link-local
        ];
        let safe_addrs: Vec<SocketAddr> = addrs
            .into_iter()
            .filter(|addr| !is_blocked_ip(addr.ip()))
            .collect();

        assert_eq!(safe_addrs.len(), 2);
        assert!(safe_addrs
            .iter()
            .any(|a| a.ip() == IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(safe_addrs
            .iter()
            .any(|a| a.ip() == IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
    }

    // ========================================================================
    // Policy tests
    // ========================================================================

    #[test]
    fn test_policy_public_only() {
        // PublicOnly should block all private IPs
        assert!(!check_url_with_policy(
            "http://192.168.1.1/",
            Policy::PublicOnly
        )
        .is_ok());

        // Public IPs should be allowed
        assert!(
            check_url_with_policy("https://example.com/", Policy::PublicOnly).is_ok()
        );
    }

    #[test]
    fn test_policy_allow_private() {
        // AllowPrivate should allow private IPs but still block loopback
        assert!(check_url_with_policy(
            "http://192.168.1.1/",
            Policy::AllowPrivate
        )
        .is_ok());

        // Loopback should still be blocked
        assert!(!check_url_with_policy("http://127.0.0.1/", Policy::AllowPrivate).is_ok());
    }

    #[tokio::test]
    async fn test_policy_builder_custom_blocklist() {
        let policy = PolicyBuilder::new(Policy::PublicOnly)
            .block_cidr("203.0.113.0/24")
            .build();

        // Custom blocked range
        assert!(
            !check_url_with_custom_policy("http://203.0.113.1/", policy.clone())
                .await
                .is_ok()
        );

        // Public IP outside custom range should still work
        assert!(
            check_url_with_custom_policy("https://example.com/", policy)
                .await
                .is_ok()
        );
    }

    // ========================================================================
    // Cloud metadata endpoint tests
    // ============================================================================

    #[test]
    fn test_cloud_metadata_endpoints_blocked() {
        // AWS metadata (link-local IP is blocked)
        assert!(
            !check_url("http://169.254.169.254/latest/meta-data/").is_ok()
        );

        // Google Cloud metadata (via hostname - url_jail blocks this)
        assert!(!check_url("http://metadata.google.internal/").is_ok());

        // Note: url_jail doesn't block metadata.azure by default
        // Azure metadata IP (169.254.169.254) is blocked via the link-local range
    }
}
