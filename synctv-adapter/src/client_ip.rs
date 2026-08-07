use std::fmt;

use http::{HeaderMap, HeaderValue};

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

#[must_use]
pub fn extract_client_ip_from_headers(
    is_trusted_proxy: impl Fn(&std::net::IpAddr) -> bool,
    peer_ip: std::net::IpAddr,
    headers: &HeaderMap,
) -> Result<std::net::IpAddr, ClientIpHeaderError> {
    if is_trusted_proxy(&peer_ip) {
        Ok(forwarded_header_ip(headers)?.unwrap_or(peer_ip))
    } else {
        Ok(peer_ip)
    }
}

pub fn forwarded_header_ip(
    headers: &HeaderMap,
) -> Result<Option<std::net::IpAddr>, ClientIpHeaderError> {
    if let Some(forwarded_for) = headers.get("x-forwarded-for") {
        return parse_forwarded_ip_header("x-forwarded-for", forwarded_for, true).map(Some);
    }

    if let Some(real_ip) = headers.get("x-real-ip") {
        return parse_forwarded_ip_header("x-real-ip", real_ip, false).map(Some);
    }

    Ok(None)
}

fn parse_forwarded_ip_header(
    header: &'static str,
    value: &HeaderValue,
    comma_separated: bool,
) -> Result<std::net::IpAddr, ClientIpHeaderError> {
    let raw = value
        .to_str()
        .map_err(|_| ClientIpHeaderError::NonAscii { header })?;
    let value = if comma_separated {
        raw.split(',').next().unwrap_or("").trim()
    } else {
        raw.trim()
    };
    if value.is_empty() {
        return Err(ClientIpHeaderError::InvalidIp {
            header,
            value: value.to_string(),
        });
    }
    value
        .parse::<std::net::IpAddr>()
        .map_err(|_| ClientIpHeaderError::InvalidIp {
            header,
            value: value.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn extract_client_ip_from_headers_uses_forwarded_for_when_proxy_trusted() -> TestResult {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.50, 70.41.3.18".parse()?);
        let peer_ip = "127.0.0.1".parse::<std::net::IpAddr>()?;
        let expected_ip = "203.0.113.50".parse::<std::net::IpAddr>()?;

        assert_eq!(
            extract_client_ip_from_headers(|ip| *ip == peer_ip, peer_ip, &headers),
            Ok(expected_ip)
        );
        Ok(())
    }

    #[test]
    fn extract_client_ip_from_headers_falls_back_to_peer_when_proxy_untrusted() -> TestResult {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.50".parse()?);
        headers.insert("x-real-ip", "198.51.100.42".parse()?);
        let peer_ip = "192.168.1.100".parse::<std::net::IpAddr>()?;

        assert_eq!(
            extract_client_ip_from_headers(|_| false, peer_ip, &headers),
            Ok(peer_ip)
        );
        Ok(())
    }

    #[test]
    fn forwarded_header_ip_rejects_invalid_forwarded_for() -> TestResult {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "not-an-ip".parse()?);
        headers.insert("x-real-ip", "198.51.100.42".parse()?);

        assert!(matches!(
            forwarded_header_ip(&headers),
            Err(ClientIpHeaderError::InvalidIp {
                header: "x-forwarded-for",
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn forwarded_header_ip_rejects_empty_forwarded_for() -> TestResult {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "".parse()?);

        assert!(matches!(
            forwarded_header_ip(&headers),
            Err(ClientIpHeaderError::InvalidIp {
                header: "x-forwarded-for",
                value
            }) if value.is_empty()
        ));
        Ok(())
    }

    #[test]
    fn forwarded_header_ip_rejects_empty_first_forwarded_for_entry() -> TestResult {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", ", 198.51.100.42".parse()?);

        assert!(matches!(
            forwarded_header_ip(&headers),
            Err(ClientIpHeaderError::InvalidIp {
                header: "x-forwarded-for",
                value
            }) if value.is_empty()
        ));
        Ok(())
    }

    #[test]
    fn forwarded_header_ip_uses_x_real_ip_when_forwarded_for_absent() -> TestResult {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", "198.51.100.42".parse()?);
        let expected_ip = "198.51.100.42".parse::<std::net::IpAddr>()?;

        assert_eq!(forwarded_header_ip(&headers), Ok(Some(expected_ip)));
        Ok(())
    }
}
