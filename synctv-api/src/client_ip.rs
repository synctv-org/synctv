use axum::http::HeaderMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientIpHeaderError {
    NonAscii { header: &'static str },
    InvalidIp { header: &'static str, value: String },
}

impl fmt::Display for ClientIpHeaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonAscii { header } => write!(f, "Invalid {header} header: value must be ASCII"),
            Self::InvalidIp { header, value } => {
                write!(f, "Invalid {header} header: '{value}' is not an IP address")
            }
        }
    }
}

impl std::error::Error for ClientIpHeaderError {}

/// Resolve the effective client IP when the direct peer may be a trusted proxy.
///
/// If the peer IP is configured as a trusted proxy, `X-Forwarded-For` takes
/// precedence and `X-Real-IP` is used only when `X-Forwarded-For` is absent.
/// Otherwise the direct peer IP is returned unchanged.
#[must_use]
pub fn extract_client_ip_from_headers(
    config: &synctv_core::Config,
    peer_ip: std::net::IpAddr,
    headers: &HeaderMap,
) -> Result<std::net::IpAddr, ClientIpHeaderError> {
    if config.server.is_trusted_proxy(&peer_ip) {
        Ok(forwarded_header_ip(headers)?.unwrap_or(peer_ip))
    } else {
        Ok(peer_ip)
    }
}

/// Parse the first valid forwarded client IP from HTTP-style headers.
pub fn forwarded_header_ip(
    headers: &HeaderMap,
) -> Result<Option<std::net::IpAddr>, ClientIpHeaderError> {
    if let Some(forwarded_for) = headers.get("x-forwarded-for") {
        let value = forwarded_for
            .to_str()
            .map_err(|_| ClientIpHeaderError::NonAscii {
                header: "x-forwarded-for",
            })?
            .split(',')
            .next()
            .unwrap_or_default()
            .trim();
        return value.parse::<std::net::IpAddr>().map(Some).map_err(|_| {
            ClientIpHeaderError::InvalidIp {
                header: "x-forwarded-for",
                value: value.to_string(),
            }
        });
    }

    if let Some(real_ip) = headers.get("x-real-ip") {
        let value = real_ip
            .to_str()
            .map_err(|_| ClientIpHeaderError::NonAscii {
                header: "x-real-ip",
            })?
            .trim();
        return value.parse::<std::net::IpAddr>().map(Some).map_err(|_| {
            ClientIpHeaderError::InvalidIp {
                header: "x-real-ip",
                value: value.to_string(),
            }
        });
    }

    Ok(None)
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
            Ok("203.0.113.50".parse::<std::net::IpAddr>().unwrap())
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
            Ok("192.168.1.100".parse::<std::net::IpAddr>().unwrap())
        );
    }

    #[test]
    fn test_forwarded_header_ip_rejects_invalid_forwarded_for() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "not-an-ip".parse().unwrap());
        headers.insert("x-real-ip", "198.51.100.42".parse().unwrap());

        assert!(matches!(
            forwarded_header_ip(&headers),
            Err(ClientIpHeaderError::InvalidIp {
                header: "x-forwarded-for",
                ..
            })
        ));
    }

    #[test]
    fn test_forwarded_header_ip_uses_x_real_ip_when_forwarded_for_absent() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", "198.51.100.42".parse().unwrap());

        assert_eq!(
            forwarded_header_ip(&headers),
            Ok(Some("198.51.100.42".parse::<std::net::IpAddr>().unwrap()))
        );
    }
}
