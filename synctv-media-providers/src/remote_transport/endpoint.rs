use crate::ProviderClientError;
use synctv_common::ssrf::SsrfGuard;

/// Validate endpoint URL structure and apply the configured runtime SSRF
/// policy to hostnames and IP literals.
///
/// DNS validation runs again inside the transport connector at connection time.
pub fn validate_endpoint_ssrf(
    endpoint: &str,
    guard: &SsrfGuard,
) -> Result<(), ProviderClientError> {
    let url = url::Url::parse(endpoint).map_err(|e| {
        ProviderClientError::InvalidConfig(format!("SSRF validation: invalid URL: {e}"))
    })?;

    match url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(ProviderClientError::InvalidConfig(format!(
                "Remote provider endpoint scheme '{scheme}' is not supported; use http:// for plaintext transport, or https:// for TLS"
            )));
        }
    }

    let host = url.host_str().ok_or_else(|| {
        ProviderClientError::InvalidConfig("SSRF validation: missing host".to_string())
    })?;

    if guard.is_host_blocked(host) {
        return Err(ProviderClientError::InvalidConfig(format!(
            "SSRF validation: host '{host}' is blocked (internal/reserved)"
        )));
    }

    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if guard.is_ip_blocked(&ip) {
            return Err(ProviderClientError::InvalidConfig(format!(
                "SSRF validation: IP '{ip}' is blocked (internal/private)"
            )));
        }
    }

    if let Some(port) = url.port() {
        if port == 0 {
            return Err(ProviderClientError::InvalidConfig(
                "SSRF validation: port 0 is not valid".to_string(),
            ));
        }
    }

    Ok(())
}

pub fn normalized_transport_endpoint(endpoint: &str) -> Result<String, ProviderClientError> {
    let url = url::Url::parse(endpoint).map_err(|e| {
        ProviderClientError::InvalidConfig(format!("Remote provider endpoint is invalid: {e}"))
    })?;

    let normalized_scheme = match url.scheme() {
        "http" => "http",
        "https" => "https",
        scheme => {
            return Err(ProviderClientError::InvalidConfig(format!(
                "Remote provider endpoint scheme '{scheme}' is not supported; use http:// for plaintext transport, or https:// for TLS"
            )));
        }
    };

    let host = url.host_str().ok_or_else(|| {
        ProviderClientError::InvalidConfig("Remote provider endpoint is missing host".to_string())
    })?;
    let mut normalized = format!("{normalized_scheme}://{host}");
    if let Some(port) = url.port() {
        normalized.push(':');
        normalized.push_str(&port.to_string());
    }
    let path = url.path();
    if !path.is_empty() && path != "/" {
        normalized.push_str(path);
    }
    if let Some(query) = url.query() {
        normalized.push('?');
        normalized.push_str(query);
    }

    Ok(normalized)
}

pub fn required_auth_secret<'a>(
    instance_name: &str,
    jwt_secret: Option<&'a str>,
) -> Result<&'a str, ProviderClientError> {
    let secret = jwt_secret.ok_or_else(|| {
        ProviderClientError::InvalidConfig(format!(
            "Remote provider instance '{instance_name}' requires a non-empty jwt_secret",
        ))
    })?;
    let trimmed = secret.trim();
    if trimmed.is_empty() {
        return Err(ProviderClientError::InvalidConfig(format!(
            "Remote provider instance '{instance_name}' requires a non-empty jwt_secret",
        )));
    }
    Ok(trimmed)
}
