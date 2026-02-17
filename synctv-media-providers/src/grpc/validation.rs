//! Shared validation utilities for gRPC server layers
//!
//! Provides SSRF-safe host validation and common field validators.
//!
//! IP/hostname blocklists are delegated to [`crate::ssrf`] which is the single
//! source of truth for SSRF primitives across the entire workspace (both this
//! crate and `synctv-core` use it).

use std::net::IpAddr;
use tonic::Status;

use crate::ssrf;

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
    match ssrf::check_url(host) {
        ssrf::SsrfCheckResult::Ok => Ok(()),
        ssrf::SsrfCheckResult::Blocked(reason) => {
            Err(Status::invalid_argument(reason))
        }
    }
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
        if ssrf::is_blocked_ip(addr.ip()) {
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
