//! Shared SSRF (Server-Side Request Forgery) protection primitives.
//!
//! This module contains the canonical IP and hostname validation logic used by
//! both the gRPC validation layer (in this crate) and `synctv-core`'s
//! `SSRFValidator`. By living here, the logic exists in exactly one place and
//! cannot diverge.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Blocked private/internal hostnames (case-insensitive check).
const BLOCKED_HOSTNAMES: &[&str] = &[
    "localhost",
    "localhost.localdomain",
    "metadata.google.internal",
    "instance-data",
    "metadata.azure",
];

/// Blocked hostname suffixes (case-insensitive check).
const BLOCKED_HOSTNAME_SUFFIXES: &[&str] = &[
    ".internal",
    ".local",
];

/// Blocked hostname prefixes for internal services (case-insensitive check).
const BLOCKED_HOSTNAME_PREFIXES: &[&str] = &[
    "metadata.",
    "metadata.google",
    "metadata.azure",
    "kubernetes.",
    "k8s.",
    "docker.",
    "container.",
];

/// Check if an IPv4 address is private, reserved, or otherwise not a valid
/// public HTTP target.
///
/// Covers: loopback, private RFC1918, link-local, CGNAT, multicast, broadcast,
/// unspecified, current-network.
#[must_use]
pub fn is_blocked_ipv4(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    // 127.0.0.0/8 (loopback)
    o[0] == 127
    // 10.0.0.0/8
    || o[0] == 10
    // 172.16.0.0/12
    || (o[0] == 172 && (16..=31).contains(&o[1]))
    // 192.168.0.0/16
    || (o[0] == 192 && o[1] == 168)
    // 169.254.0.0/16 (link-local, cloud metadata)
    || (o[0] == 169 && o[1] == 254)
    // 100.64.0.0/10 (CGNAT)
    || (o[0] == 100 && (64..=127).contains(&o[1]))
    // 0.0.0.0/8 (current network)
    || o[0] == 0
    // 224.0.0.0/4 (multicast)
    || (224..=239).contains(&o[0])
    // 240.0.0.0/4 (reserved/broadcast)
    || o[0] >= 240
}

/// Check if an IPv6 address is private, reserved, or otherwise not a valid
/// public HTTP target.
///
/// Covers: loopback (`::1`), unspecified (::), unique local (`fc00::/7`),
/// link-local (`fe80::/10`), IPv4-mapped private addresses.
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
    // Check IPv4-mapped IPv6 (::ffff:x.x.x.x)
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_blocked_ipv4(&v4);
    }
    let segments = ip.segments();
    // Unique local (fc00::/7)
    if (segments[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // Link-local (fe80::/10)
    if (segments[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    // Multicast (ff00::/8)
    if segments[0] & 0xff00 == 0xff00 {
        return true;
    }
    // Teredo (2001::/32) - tunnels IPv4 via UDP, can reach private networks
    if segments[0] == 0x2001 && segments[1] == 0x0000 {
        return true;
    }
    // 6to4 (2002::/16) - tunnels IPv4 in IPv6, similarly dangerous
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

/// Result of hostname validation.
#[derive(Debug)]
pub enum SsrfCheckResult {
    /// The value is safe.
    Ok,
    /// The value is blocked with the given reason.
    Blocked(String),
}

impl SsrfCheckResult {
    /// Returns `true` if the check passed.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// Validate a hostname against known internal/suspicious patterns.
///
/// Returns `SsrfCheckResult::Blocked` with a reason if the hostname is
/// internal or suspicious. Does NOT perform DNS resolution.
#[must_use]
pub fn check_hostname(host: &str) -> SsrfCheckResult {
    let lower = host.to_lowercase();

    // Block known internal hostnames
    for blocked in BLOCKED_HOSTNAMES {
        if lower == *blocked || lower.starts_with(&format!("{blocked}.")) {
            return SsrfCheckResult::Blocked(format!(
                "internal hostname '{host}' is not allowed"
            ));
        }
    }

    // Block suffixes (.local, .internal)
    for suffix in BLOCKED_HOSTNAME_SUFFIXES {
        if lower.ends_with(suffix) {
            return SsrfCheckResult::Blocked(format!(
                "internal hostname '{host}' is not allowed"
            ));
        }
    }

    // Block suspicious prefixes (kubernetes., k8s., docker., container., metadata.)
    for prefix in BLOCKED_HOSTNAME_PREFIXES {
        if lower.starts_with(prefix) {
            return SsrfCheckResult::Blocked(format!(
                "internal service hostname '{host}' is not allowed"
            ));
        }
    }

    SsrfCheckResult::Ok
}

/// Validate a URL for basic SSRF protections (string-level, no DNS).
///
/// Checks:
/// - URL is parseable
/// - Scheme is http or https only
/// - Host is not a private IP range
/// - Host is not a known internal hostname
///
/// Returns `SsrfCheckResult::Blocked` with a reason on failure.
#[must_use]
pub fn check_url(url: &str) -> SsrfCheckResult {
    if url.is_empty() {
        return SsrfCheckResult::Blocked("URL must not be empty".to_string());
    }

    let parsed = match url::Url::parse(url) {
        Ok(p) => p,
        Err(e) => return SsrfCheckResult::Blocked(format!("invalid URL: {e}")),
    };

    // Verify scheme
    match parsed.scheme() {
        "http" | "https" => {}
        scheme => {
            return SsrfCheckResult::Blocked(format!(
                "unsupported URL scheme: {scheme} (only http and https are allowed)"
            ));
        }
    }

    let url_host = match parsed.host_str() {
        Some(h) => h,
        None => return SsrfCheckResult::Blocked("URL must contain a hostname".to_string()),
    };

    // Try to parse as IP address and block private ranges
    if let Ok(ip) = url_host.parse::<IpAddr>() {
        if is_blocked_ip(ip) {
            return SsrfCheckResult::Blocked(format!(
                "URL must not target private/reserved IP: {url_host}"
            ));
        }
    }

    // Handle bracket-wrapped IPv6 like [::1]
    if url_host.starts_with('[') && url_host.ends_with(']') {
        if let Ok(ip) = url_host[1..url_host.len() - 1].parse::<IpAddr>() {
            if is_blocked_ip(ip) {
                return SsrfCheckResult::Blocked(format!(
                    "URL must not target private/reserved IP: {url_host}"
                ));
            }
        }
    }

    // Check hostname patterns
    check_hostname(url_host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blocked_private_ipv4() {
        assert!(is_blocked_ipv4(&Ipv4Addr::new(127, 0, 0, 1)));
        assert!(is_blocked_ipv4(&Ipv4Addr::new(10, 0, 0, 1)));
        assert!(is_blocked_ipv4(&Ipv4Addr::new(172, 16, 0, 1)));
        assert!(is_blocked_ipv4(&Ipv4Addr::new(192, 168, 1, 1)));
        assert!(is_blocked_ipv4(&Ipv4Addr::new(169, 254, 1, 1)));
        assert!(is_blocked_ipv4(&Ipv4Addr::new(100, 64, 0, 1)));
        assert!(is_blocked_ipv4(&Ipv4Addr::new(0, 0, 0, 0)));
        assert!(is_blocked_ipv4(&Ipv4Addr::new(240, 0, 0, 1)));
        assert!(is_blocked_ipv4(&Ipv4Addr::new(255, 255, 255, 255)));
    }

    #[test]
    fn test_allowed_public_ipv4() {
        assert!(!is_blocked_ipv4(&Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_blocked_ipv4(&Ipv4Addr::new(1, 1, 1, 1)));
        assert!(!is_blocked_ipv4(&Ipv4Addr::new(203, 0, 113, 1)));
    }

    #[test]
    fn test_blocked_ipv6() {
        assert!(is_blocked_ipv6(&Ipv6Addr::LOCALHOST));
        assert!(is_blocked_ipv6(&Ipv6Addr::UNSPECIFIED));
    }

    #[test]
    fn test_check_hostname_blocked() {
        assert!(!check_hostname("localhost").is_ok());
        assert!(!check_hostname("metadata.google.internal").is_ok());
        assert!(!check_hostname("myhost.local").is_ok());
        assert!(!check_hostname("kubernetes.default").is_ok());
    }

    #[test]
    fn test_check_hostname_allowed() {
        assert!(check_hostname("example.com").is_ok());
        assert!(check_hostname("api.bilibili.com").is_ok());
    }

    #[test]
    fn test_check_url_blocked() {
        assert!(!check_url("http://127.0.0.1").is_ok());
        assert!(!check_url("http://192.168.1.1").is_ok());
        assert!(!check_url("http://localhost").is_ok());
        assert!(!check_url("ftp://example.com").is_ok());
        assert!(!check_url("").is_ok());
    }

    #[test]
    fn test_check_url_allowed() {
        assert!(check_url("https://example.com").is_ok());
        assert!(check_url("http://8.8.8.8").is_ok());
    }
}
