//! Shared validation utilities for gRPC server layers
//!
//! Provides SSRF-safe host validation and common field validators.
//!
//! IP blocklist mirrors the canonical implementation in `synctv_core::validation`.
//! Note: `synctv-core` depends on `synctv-media-providers`, so we cannot import
//! from it directly (would create a cyclic dependency). Keep this in sync with
//! `synctv_core::validation::SSRFValidator`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use tonic::Status;

/// Blocked private/internal hostnames (case-insensitive check)
const BLOCKED_HOSTNAMES: &[&str] = &[
    "localhost",
    "metadata.google.internal",
];

/// Blocked hostname suffixes (case-insensitive check)
const BLOCKED_HOSTNAME_SUFFIXES: &[&str] = &[
    ".internal",
    ".local",
];

/// Check if an IP is private, reserved, or otherwise not a valid public HTTP target.
/// Covers: loopback, private RFC1918, link-local, CGNAT, multicast, broadcast,
/// unspecified, IPv6 loopback, IPv4-mapped IPv6 private addresses.
fn is_blocked_ip(ip: IpAddr) -> bool {
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return true;
    }
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(&v4),
        IpAddr::V6(v6) => is_blocked_ipv6(&v6),
    }
}

fn is_blocked_ipv4(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    // 10.0.0.0/8
    o[0] == 10
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
    // 240.0.0.0/4 (reserved/broadcast)
    || o[0] >= 240
}

fn is_blocked_ipv6(ip: &Ipv6Addr) -> bool {
    // Check IPv4-mapped IPv6 (::ffff:x.x.x.x)
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_blocked_ipv4(&v4);
    }
    // Unique local (fc00::/7)
    let segments = ip.segments();
    (segments[0] & 0xfe00) == 0xfc00
    // Link-local (fe80::/10)
    || (segments[0] & 0xffc0) == 0xfe80
}

/// Validate that a host string is a non-empty, valid URL with SSRF protections.
///
/// Checks:
/// - URL is parseable
/// - Scheme is http or https only
/// - Host is not a private IP range
/// - Host is not a known internal hostname
///
/// NOTE: This performs only string-level checks. For full DNS-rebinding
/// protection, use [`validate_host_with_dns`] in async contexts.
#[allow(clippy::result_large_err)] // tonic::Status is inherently large; boxing would break gRPC API
pub fn validate_host(host: &str) -> Result<(), Status> {
    validate_host_static(host)
}

/// Synchronous string-level URL validation (shared between sync and async paths).
#[allow(clippy::result_large_err)]
fn validate_host_static(host: &str) -> Result<(), Status> {
    if host.is_empty() {
        return Err(Status::invalid_argument("host must not be empty"));
    }

    let parsed = url::Url::parse(host)
        .map_err(|e| Status::invalid_argument(format!("invalid host URL: {e}")))?;

    // Verify scheme is http or https only
    match parsed.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(Status::invalid_argument(format!(
                "unsupported URL scheme: {scheme} (only http and https are allowed)"
            )));
        }
    }

    let url_host = parsed
        .host_str()
        .ok_or_else(|| Status::invalid_argument("host URL must contain a hostname"))?;

    let host_lower = url_host.to_lowercase();

    // Block known internal hostnames
    for blocked in BLOCKED_HOSTNAMES {
        if host_lower == *blocked {
            return Err(Status::invalid_argument(format!(
                "host URL must not target internal address: {url_host}"
            )));
        }
    }
    for suffix in BLOCKED_HOSTNAME_SUFFIXES {
        if host_lower.ends_with(suffix) {
            return Err(Status::invalid_argument(format!(
                "host URL must not target internal address: {url_host}"
            )));
        }
    }

    // Try to parse as IP address and block private ranges
    if let Ok(ip) = url_host.parse::<IpAddr>() {
        if is_blocked_ip(ip) {
            return Err(Status::invalid_argument(format!(
                "host URL must not target private/reserved IP: {url_host}"
            )));
        }
    }

    // Also handle bracket-wrapped IPv6 like [::1]
    if url_host.starts_with('[') && url_host.ends_with(']') {
        if let Ok(ip) = url_host[1..url_host.len() - 1].parse::<IpAddr>() {
            if is_blocked_ip(ip) {
                return Err(Status::invalid_argument(format!(
                    "host URL must not target private/reserved IP: {url_host}"
                )));
            }
        }
    }

    Ok(())
}

/// Async host validation with DNS resolution to prevent DNS rebinding attacks.
///
/// Performs all the checks of [`validate_host`] plus resolves the hostname
/// and verifies that none of the resolved IP addresses are private/reserved.
#[allow(clippy::result_large_err)]
pub async fn validate_host_with_dns(host: &str) -> Result<(), Status> {
    // First run the synchronous string-level checks
    validate_host_static(host)?;

    // Parse URL again to extract hostname for DNS resolution
    let parsed = url::Url::parse(host)
        .map_err(|e| Status::invalid_argument(format!("invalid host URL: {e}")))?;

    let url_host = parsed
        .host_str()
        .ok_or_else(|| Status::invalid_argument("host URL must contain a hostname"))?;

    // Only resolve if the host is NOT already a literal IP (already checked above)
    if url_host.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    // Also skip if it's a bracketed IPv6 literal
    if url_host.starts_with('[') && url_host.ends_with(']')
        && url_host[1..url_host.len() - 1].parse::<IpAddr>().is_ok()
    {
        return Ok(());
    }

    let port = parsed
        .port()
        .unwrap_or(if parsed.scheme() == "https" { 443 } else { 80 });

    let addrs = tokio::net::lookup_host((url_host, port))
        .await
        .map_err(|e| {
            Status::invalid_argument(format!("DNS lookup failed for {url_host}: {e}"))
        })?;

    let mut found = false;
    for addr in addrs {
        if is_blocked_ip(addr.ip()) {
            return Err(Status::invalid_argument(format!(
                "hostname {url_host} resolves to private/reserved IP {}",
                addr.ip()
            )));
        }
        found = true;
    }

    if !found {
        return Err(Status::invalid_argument(format!(
            "hostname {url_host} resolved to no addresses"
        )));
    }

    Ok(())
}

/// Validate that a required string field is non-empty.
#[allow(clippy::result_large_err)] // tonic::Status is inherently large; boxing would break gRPC API
pub fn validate_required(field_name: &str, value: &str) -> Result<(), Status> {
    if value.is_empty() {
        return Err(Status::invalid_argument(format!(
            "{field_name} must not be empty"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_hosts() {
        assert!(validate_host("https://example.com").is_ok());
        assert!(validate_host("http://my-alist.example.com:5244").is_ok());
        assert!(validate_host("https://emby.myserver.org/emby").is_ok());
    }

    #[test]
    fn test_blocked_schemes() {
        assert!(validate_host("ftp://example.com").is_err());
        assert!(validate_host("file:///etc/passwd").is_err());
        assert!(validate_host("gopher://evil.com").is_err());
    }

    #[test]
    fn test_blocked_private_ips() {
        assert!(validate_host("http://127.0.0.1").is_err());
        assert!(validate_host("http://10.0.0.1").is_err());
        assert!(validate_host("http://172.16.0.1").is_err());
        assert!(validate_host("http://192.168.1.1").is_err());
        assert!(validate_host("http://169.254.1.1").is_err());
        assert!(validate_host("http://0.0.0.0").is_err());
    }

    #[test]
    fn test_blocked_hostnames() {
        assert!(validate_host("http://localhost").is_err());
        assert!(validate_host("http://LOCALHOST").is_err());
        assert!(validate_host("http://metadata.google.internal").is_err());
        assert!(validate_host("http://something.internal").is_err());
        assert!(validate_host("http://myhost.local").is_err());
    }

    #[test]
    fn test_empty_host() {
        assert!(validate_host("").is_err());
    }

    #[test]
    fn test_invalid_url() {
        assert!(validate_host("not-a-url").is_err());
    }

    // === Extended SSRF Protection Tests ===

    #[test]
    fn test_blocked_cgnat() {
        assert!(validate_host("http://100.64.0.1").is_err());
        assert!(validate_host("http://100.127.255.255").is_err());
    }

    #[test]
    fn test_blocked_multicast() {
        assert!(validate_host("http://224.0.0.1").is_err());
        assert!(validate_host("http://239.255.255.255").is_err());
    }

    #[test]
    fn test_blocked_broadcast() {
        assert!(validate_host("http://255.255.255.255").is_err());
    }

    #[test]
    fn test_blocked_link_local() {
        assert!(validate_host("http://169.254.1.1").is_err());
        assert!(validate_host("http://169.254.169.254").is_err()); // Cloud metadata
    }

    #[test]
    fn test_blocked_ipv6_loopback() {
        assert!(validate_host("http://[::1]").is_err());
    }

    #[test]
    fn test_blocked_ipv6_unspecified() {
        assert!(validate_host("http://[::]").is_err());
    }

    #[test]
    fn test_public_ips_allowed() {
        assert!(validate_host("http://8.8.8.8").is_ok());
        assert!(validate_host("http://1.1.1.1").is_ok());
        assert!(validate_host("https://203.0.113.1").is_ok());
    }

    #[test]
    fn test_valid_public_hosts() {
        assert!(validate_host("https://api.example.com").is_ok());
        assert!(validate_host("http://my-server.org:8096").is_ok());
        assert!(validate_host("https://cdn.provider.io/path").is_ok());
    }

    #[test]
    fn test_validate_required_empty() {
        let result = validate_required("username", "");
        assert!(result.is_err());
        let status = result.unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("username"));
    }

    #[test]
    fn test_validate_required_non_empty() {
        assert!(validate_required("username", "alice").is_ok());
        assert!(validate_required("token", "abc123").is_ok());
    }

    #[test]
    fn test_validate_host_with_port() {
        assert!(validate_host("https://example.com:443").is_ok());
        assert!(validate_host("http://example.com:8080").is_ok());
        assert!(validate_host("http://127.0.0.1:8080").is_err());
    }

    #[test]
    fn test_validate_host_with_path() {
        assert!(validate_host("https://example.com/api/v1").is_ok());
        assert!(validate_host("https://example.com/emby").is_ok());
    }
}
