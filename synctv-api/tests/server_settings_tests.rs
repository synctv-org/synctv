use std::net::IpAddr;

use synctv_api::{ApiRuntimeSettings, ApiServerSettings};

fn ip(value: &str) -> IpAddr {
    value.parse().expect("IP address should parse")
}

fn server_settings(proxies: Vec<String>) -> ApiServerSettings {
    ApiServerSettings {
        trusted_proxies: proxies,
        ..ApiServerSettings::default()
    }
}

#[test]
fn test_api_runtime_address_uses_server_bind_address() {
    let runtime_settings = ApiRuntimeSettings {
        server: ApiServerSettings {
            bind_address: "127.0.0.1:18080".to_string(),
            ..ApiServerSettings::default()
        },
        ..ApiRuntimeSettings::default()
    };

    assert_eq!(runtime_settings.api_address(), "127.0.0.1:18080");
    assert!(runtime_settings
        .api_address()
        .parse::<std::net::SocketAddr>()
        .is_ok());
}

#[test]
fn test_is_trusted_proxy_empty_list() {
    let settings = server_settings(vec![]);

    assert!(!settings.is_trusted_proxy(&ip("10.0.0.1")));
}

#[test]
fn test_is_trusted_proxy_single_ip() {
    let settings = server_settings(vec!["10.0.0.1".to_string()]);

    assert!(settings.is_trusted_proxy(&ip("10.0.0.1")));
    assert!(!settings.is_trusted_proxy(&ip("10.0.0.2")));
}

#[test]
fn test_is_trusted_proxy_cidr() {
    let settings = server_settings(vec!["10.0.0.0/8".to_string()]);

    assert!(settings.is_trusted_proxy(&ip("10.0.0.1")));
    assert!(settings.is_trusted_proxy(&ip("10.255.255.255")));
    assert!(!settings.is_trusted_proxy(&ip("11.0.0.1")));
}

#[test]
fn test_is_trusted_proxy_multiple_entries() {
    let settings = server_settings(vec!["10.0.0.0/8".to_string(), "192.168.1.100".to_string()]);

    assert!(settings.is_trusted_proxy(&ip("10.1.2.3")));
    assert!(settings.is_trusted_proxy(&ip("192.168.1.100")));
    assert!(!settings.is_trusted_proxy(&ip("192.168.1.101")));
}

#[test]
fn test_is_trusted_proxy_ipv6() {
    let settings = server_settings(vec!["::1".to_string()]);

    assert!(settings.is_trusted_proxy(&ip("::1")));
    assert!(!settings.is_trusted_proxy(&ip("::2")));
}

#[test]
fn test_is_trusted_proxy_ipv6_cidr() {
    let settings = server_settings(vec!["fd00::/8".to_string()]);

    assert!(settings.is_trusted_proxy(&ip("fd00::1")));
    assert!(settings.is_trusted_proxy(&ip("fdff::1")));
    assert!(!settings.is_trusted_proxy(&ip("fe80::1")));
}

#[test]
fn test_is_trusted_proxy_invalid_entry_ignored() {
    let settings = server_settings(vec![
        "not-a-valid-ip-or-cidr".to_string(),
        "10.0.0.1".to_string(),
    ]);

    assert!(settings.is_trusted_proxy(&ip("10.0.0.1")));
    assert!(!settings.is_trusted_proxy(&ip("10.0.0.2")));
}
