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
    Http {
        status: reqwest::StatusCode,
        url: String,
        /// Retry-After header value in seconds (from HTTP 429 responses).
        retry_after_secs: Option<u64>,
    },

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
///
/// For HTTP 429 responses, the `Retry-After` header is parsed and stored
/// in the error so that callers can respect the server's backoff hint.
pub fn check_response(resp: reqwest::Response) -> Result<reqwest::Response, ProviderClientError> {
    let status = resp.status();
    if status.is_client_error() || status.is_server_error() {
        let retry_after_secs = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            resp.headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
        } else {
            None
        };
        return Err(ProviderClientError::Http {
            status,
            url: resp.url().to_string(),
            retry_after_secs,
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

impl ProviderClientError {
    /// Whether this error is transient and the request should be retried.
    ///
    /// Network errors, server errors (5xx), and HTTP 429 (Too Many Requests)
    /// are retryable. Other client errors (4xx), parse errors, and auth errors
    /// are not.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Network(_) => true,
            Self::Http { status, .. } => {
                status.is_server_error() || *status == reqwest::StatusCode::TOO_MANY_REQUESTS
            }
            _ => false,
        }
    }
}

/// Create the standard exponential backoff for provider API calls.
///
/// Starts at 200ms, doubles each attempt, caps at 5s, with up to 3 retries.
/// Includes jitter (±25%) to prevent thundering herd on retry storms.
#[must_use]
pub fn provider_backoff() -> backon::ExponentialBuilder {
    backon::ExponentialBuilder::default()
        .with_min_delay(std::time::Duration::from_millis(200))
        .with_max_delay(std::time::Duration::from_secs(5))
        .with_max_times(3)
        .with_jitter() // Add jitter to prevent thundering herd
}

/// Execute an async operation with retry and exponential backoff.
///
/// Only retries on transient errors (network errors and 5xx server errors).
/// Client errors (4xx), parse errors, and auth errors fail immediately.
pub async fn with_retry<F, Fut, T>(op: F) -> Result<T, ProviderClientError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, ProviderClientError>>,
{
    use backon::Retryable;
    op.retry(provider_backoff())
        .when(|e: &ProviderClientError| e.is_retryable())
        .await
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
            retry_after_secs: None,
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

    // === Retryable error tests ===

    #[test]
    fn test_is_retryable_network() {
        let err = ProviderClientError::Network("timeout".to_string());
        assert!(err.is_retryable());
    }

    #[test]
    fn test_is_retryable_server_error() {
        let err = ProviderClientError::Http {
            status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            url: "https://example.com".to_string(),
            retry_after_secs: None,
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn test_is_retryable_429_too_many_requests() {
        let err = ProviderClientError::Http {
            status: reqwest::StatusCode::TOO_MANY_REQUESTS,
            url: "https://api.bilibili.com/x/player/wbi/playurl".to_string(),
            retry_after_secs: Some(5),
        };
        assert!(err.is_retryable(), "HTTP 429 should be retryable");
        // Verify retry_after_secs is captured
        if let ProviderClientError::Http { retry_after_secs, .. } = &err {
            assert_eq!(*retry_after_secs, Some(5));
        }
    }

    #[test]
    fn test_is_not_retryable_404() {
        let err = ProviderClientError::Http {
            status: reqwest::StatusCode::NOT_FOUND,
            url: "https://example.com".to_string(),
            retry_after_secs: None,
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_is_not_retryable_403() {
        let err = ProviderClientError::Http {
            status: reqwest::StatusCode::FORBIDDEN,
            url: "https://example.com".to_string(),
            retry_after_secs: None,
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_is_not_retryable_parse() {
        let err = ProviderClientError::Parse("bad json".to_string());
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_is_not_retryable_auth() {
        let err = ProviderClientError::Auth("expired".to_string());
        assert!(!err.is_retryable());
    }
}
