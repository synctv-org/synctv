use axum::http::HeaderMap;

/// Resolve the effective client IP when the direct peer may be a trusted proxy.
///
/// If the peer IP is configured as a trusted proxy, `X-Forwarded-For` takes
/// precedence and falls back to `X-Real-IP`. Otherwise the direct peer IP is
/// returned unchanged.
#[must_use]
pub fn extract_client_ip_from_headers(
    config: &synctv_core::Config,
    peer_ip: std::net::IpAddr,
    headers: &HeaderMap,
) -> std::net::IpAddr {
    if config.server.is_trusted_proxy(&peer_ip) {
        forwarded_header_ip(headers).unwrap_or(peer_ip)
    } else {
        peer_ip
    }
}

/// Parse the first valid forwarded client IP from HTTP-style headers.
#[must_use]
pub fn forwarded_header_ip(headers: &HeaderMap) -> Option<std::net::IpAddr> {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(str::trim)
        .and_then(|s| s.parse::<std::net::IpAddr>().ok())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .and_then(|s| s.parse::<std::net::IpAddr>().ok())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_client_ip_from_headers_uses_forwarded_for_when_proxy_trusted() {
        let mut config = synctv_core::Config::default();
        config.server.trusted_proxies = vec!["127.0.0.1".to_string()];

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "203.0.113.50, 70.41.3.18".parse().unwrap(),
        );

        assert_eq!(
            extract_client_ip_from_headers(
                &config,
                "127.0.0.1".parse::<std::net::IpAddr>().unwrap(),
                &headers
            ),
            "203.0.113.50".parse::<std::net::IpAddr>().unwrap()
        );
    }

    #[test]
    fn test_extract_client_ip_from_headers_falls_back_to_peer_when_proxy_untrusted() {
        let mut config = synctv_core::Config::default();
        config.server.trusted_proxies = vec!["127.0.0.1".to_string()];

        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.50".parse().unwrap());
        headers.insert("x-real-ip", "198.51.100.42".parse().unwrap());

        assert_eq!(
            extract_client_ip_from_headers(
                &config,
                "192.168.1.100".parse::<std::net::IpAddr>().unwrap(),
                &headers
            ),
            "192.168.1.100".parse::<std::net::IpAddr>().unwrap()
        );
    }

    #[test]
    fn test_forwarded_header_ip_falls_back_to_x_real_ip_when_forwarded_for_invalid() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "not-an-ip".parse().unwrap());
        headers.insert("x-real-ip", "198.51.100.42".parse().unwrap());

        assert_eq!(
            forwarded_header_ip(&headers),
            Some("198.51.100.42".parse::<std::net::IpAddr>().unwrap())
        );
    }
}
