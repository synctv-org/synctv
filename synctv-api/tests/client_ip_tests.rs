//! Tests for `extract_client_ip` in synctv-api/src/http/auth.rs
//!
//! Verifies correct IP extraction from trusted/untrusted proxies,
//! X-Forwarded-For and X-Real-IP header handling.

#![allow(clippy::unwrap_used)]
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use synctv_api::extract_http_client_ip as extract_client_ip;
use synctv_api::ApiRuntimeSettings as Config;

/// Create a config with the given trusted proxies.
fn config_with_proxies(proxies: Vec<&str>) -> Config {
    let mut config = Config::default();
    config.server.trusted_proxies = proxies.into_iter().map(String::from).collect();
    config
}

/// Create a socket address from an IPv4 address.
const fn socket(ip: [u8; 4], port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3])), port)
}

#[test]
fn test_trusted_proxy_x_forwarded_for() {
    let config = config_with_proxies(vec!["10.0.0.1"]);
    let socket_addr = socket([10, 0, 0, 1], 5000);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-forwarded-for", "203.0.113.50".parse().unwrap());

    let ip = extract_client_ip(&config, socket_addr, &headers).expect("client ip should parse");
    assert_eq!(ip, "203.0.113.50".parse::<IpAddr>().unwrap());
}

#[test]
fn test_trusted_proxy_x_real_ip_fallback() {
    let config = config_with_proxies(vec!["10.0.0.1"]);
    let socket_addr = socket([10, 0, 0, 1], 5000);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-real-ip", "198.51.100.42".parse().unwrap());

    let ip = extract_client_ip(&config, socket_addr, &headers).expect("client ip should parse");
    assert_eq!(ip, "198.51.100.42".parse::<IpAddr>().unwrap());
}

#[test]
fn test_untrusted_proxy_ignores_headers() {
    let config = config_with_proxies(vec!["10.0.0.1"]);
    let socket_addr = socket([192, 168, 1, 100], 5000);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-forwarded-for", "203.0.113.50".parse().unwrap());
    headers.insert("x-real-ip", "198.51.100.42".parse().unwrap());

    let ip = extract_client_ip(&config, socket_addr, &headers).expect("client ip should parse");
    assert_eq!(ip, "192.168.1.100".parse::<IpAddr>().unwrap());
}

#[test]
fn test_xff_multiple_hops() {
    let config = config_with_proxies(vec!["10.0.0.1"]);
    let socket_addr = socket([10, 0, 0, 1], 5000);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        "203.0.113.50, 172.16.0.1".parse().unwrap(),
    );

    let ip = extract_client_ip(&config, socket_addr, &headers).expect("client ip should parse");
    assert_eq!(ip, "203.0.113.50".parse::<IpAddr>().unwrap());
}

#[test]
fn test_no_headers_returns_socket_ip() {
    let config = Config::default();
    let socket_addr = socket([192, 168, 1, 50], 12345);
    let headers = axum::http::HeaderMap::new();

    let ip = extract_client_ip(&config, socket_addr, &headers).expect("client ip should parse");
    assert_eq!(ip, "192.168.1.50".parse::<IpAddr>().unwrap());
}

#[test]
fn test_no_trusted_proxies_configured_ignores_xff() {
    let config = Config::default();
    let socket_addr = socket([10, 0, 0, 1], 5000);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-forwarded-for", "203.0.113.50".parse().unwrap());

    let ip = extract_client_ip(&config, socket_addr, &headers).expect("client ip should parse");
    assert_eq!(ip, "10.0.0.1".parse::<IpAddr>().unwrap());
}

#[test]
fn test_xff_with_whitespace_trimmed() {
    let config = config_with_proxies(vec!["10.0.0.1"]);
    let socket_addr = socket([10, 0, 0, 1], 5000);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        " 203.0.113.50 , 172.16.0.1".parse().unwrap(),
    );

    let ip = extract_client_ip(&config, socket_addr, &headers).expect("client ip should parse");
    assert_eq!(ip, "203.0.113.50".parse::<IpAddr>().unwrap());
}

#[test]
fn test_xff_invalid_ip_is_rejected() {
    let config = config_with_proxies(vec!["10.0.0.1"]);
    let socket_addr = socket([10, 0, 0, 1], 5000);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-forwarded-for", "not-an-ip".parse().unwrap());
    headers.insert("x-real-ip", "198.51.100.42".parse().unwrap());

    assert!(extract_client_ip(&config, socket_addr, &headers).is_err());
}

#[test]
fn test_trusted_proxy_cidr_notation() {
    let config = config_with_proxies(vec!["10.0.0.0/8"]);
    let socket_addr = socket([10, 1, 2, 3], 5000);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-forwarded-for", "203.0.113.50".parse().unwrap());

    let ip = extract_client_ip(&config, socket_addr, &headers).expect("client ip should parse");
    assert_eq!(ip, "203.0.113.50".parse::<IpAddr>().unwrap());
}
