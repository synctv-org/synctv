//! Shared provider client error types
//!
//! Common error enum and utilities used by all provider clients (Alist, Bilibili, Emby).

use thiserror::Error;

/// Maximum response body size for provider HTTP calls (16 MB).
/// Prevents OOM from malicious or misconfigured upstream servers.
pub const MAX_RESPONSE_SIZE: usize = 16 * 1024 * 1024;

/// Common error type for all provider HTTP clients.
#[derive(Debug, Error)]
pub enum ProviderClientError {
    #[error("Network error: {0}")]
    Network(String),

    #[error("HTTP error {status} for {url}")]
    Http { status: reqwest::StatusCode, url: String },

    #[error("API error (code {code}): {message}")]
    Api { code: i64, message: String },

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Authentication failed: {0}")]
    Auth(String),

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("Invalid header value: {0}")]
    InvalidHeader(String),

    #[error("Not implemented: {0}")]
    NotImplemented(String),

    #[error("Response too large ({size} bytes, max {MAX_RESPONSE_SIZE})")]
    ResponseTooLarge { size: u64 },
}

/// Read a response body with size limit and deserialize as JSON.
///
/// Checks `Content-Length` hint first (if available), then enforces the
/// limit on the actual body bytes before deserializing.
pub async fn json_with_limit<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, ProviderClientError> {
    if let Some(cl) = response.content_length() {
        if cl as usize > MAX_RESPONSE_SIZE {
            return Err(ProviderClientError::ResponseTooLarge { size: cl });
        }
    }
    let bytes = response.bytes().await?;
    if bytes.len() > MAX_RESPONSE_SIZE {
        return Err(ProviderClientError::ResponseTooLarge { size: bytes.len() as u64 });
    }
    serde_json::from_slice(&bytes).map_err(Into::into)
}

/// Check HTTP response status before processing body.
pub fn check_response(resp: reqwest::Response) -> Result<reqwest::Response, ProviderClientError> {
    let status = resp.status();
    if status.is_client_error() || status.is_server_error() {
        return Err(ProviderClientError::Http {
            status,
            url: resp.url().to_string(),
        });
    }
    Ok(resp)
}

impl From<reqwest::Error> for ProviderClientError {
    fn from(err: reqwest::Error) -> Self {
        Self::Network(err.to_string())
    }
}

impl From<serde_json::Error> for ProviderClientError {
    fn from(err: serde_json::Error) -> Self {
        Self::Parse(err.to_string())
    }
}

impl From<reqwest::header::InvalidHeaderValue> for ProviderClientError {
    fn from(err: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display_network() {
        let err = ProviderClientError::Network("connection refused".to_string());
        assert_eq!(err.to_string(), "Network error: connection refused");
    }

    #[test]
    fn test_error_display_http() {
        let err = ProviderClientError::Http {
            status: reqwest::StatusCode::NOT_FOUND,
            url: "https://example.com/api".to_string(),
        };
        assert_eq!(err.to_string(), "HTTP error 404 Not Found for https://example.com/api");
    }

    #[test]
    fn test_error_display_api() {
        let err = ProviderClientError::Api {
            code: 62002,
            message: "invalid token".to_string(),
        };
        assert_eq!(err.to_string(), "API error (code 62002): invalid token");
    }

    #[test]
    fn test_error_display_parse() {
        let err = ProviderClientError::Parse("unexpected EOF".to_string());
        assert_eq!(err.to_string(), "Parse error: unexpected EOF");
    }

    #[test]
    fn test_error_display_auth() {
        let err = ProviderClientError::Auth("session expired".to_string());
        assert_eq!(err.to_string(), "Authentication failed: session expired");
    }

    #[test]
    fn test_error_display_invalid_config() {
        let err = ProviderClientError::InvalidConfig("missing host".to_string());
        assert_eq!(err.to_string(), "Invalid configuration: missing host");
    }

    #[test]
    fn test_error_display_response_too_large() {
        let err = ProviderClientError::ResponseTooLarge { size: 20_000_000 };
        let msg = err.to_string();
        assert!(msg.contains("20000000"));
        assert!(msg.contains(&MAX_RESPONSE_SIZE.to_string()));
    }

    #[test]
    fn test_error_from_serde_json() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid json").unwrap_err();
        let err: ProviderClientError = json_err.into();
        assert!(matches!(err, ProviderClientError::Parse(_)));
    }

    #[test]
    fn test_max_response_size() {
        assert_eq!(MAX_RESPONSE_SIZE, 16 * 1024 * 1024);
    }
}
