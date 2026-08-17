//! Shared provider client error types
//!
//! Common error enum and utilities used by all provider clients (Alist, Bilibili, Emby).

use thiserror::Error;

/// Maximum response body size for provider HTTP calls (16 MB).
/// Prevents OOM from malicious or misconfigured upstream servers.
pub const MAX_RESPONSE_SIZE: usize = 16 * 1024 * 1024;

/// Maximum number of bytes captured from an error response body.
const MAX_ERROR_BODY_PREVIEW_BYTES: usize = 1024;

/// Shared User-Agent string for all provider HTTP clients.
///
/// Using a consistent browser-like User-Agent across all providers prevents
/// request fingerprinting and ensures uniform behavior with upstream APIs.
pub const PROVIDER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// Common error type for all provider HTTP clients.
#[derive(Debug, Error)]
pub enum ProviderClientError {
    #[error("Network error: {0}")]
    Network(String),

    #[error("HTTP error {status} for {url}: {body}")]
    Http {
        status: reqwest::StatusCode,
        url: String,
        /// Retry-After header value in seconds (from HTTP 429 responses).
        retry_after_secs: Option<u64>,
        /// Response body text for debugging (truncated to 1 KB).
        body: String,
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

    #[error("Response too large ({size} bytes, max {MAX_RESPONSE_SIZE})")]
    ResponseTooLarge { size: u64 },
}

/// Fetch JSON from a request: send + check status + deserialize with size limit.
///
/// Combines `check_response()` and `json_with_limit()` into a single helper.
/// This is the recommended way to fetch JSON from provider APIs.
pub async fn fetch_json<T: serde::de::DeserializeOwned>(
    request: reqwest::RequestBuilder,
) -> Result<T, ProviderClientError> {
    let response = request.send().await?;
    let response = check_response(response).await?;
    json_with_limit(response).await
}

/// Read a response body with the shared provider size limit.
///
/// Checks `Content-Length` first when available, then enforces the limit on
/// every received chunk.
pub async fn bytes_with_limit(
    mut response: reqwest::Response,
) -> Result<Vec<u8>, ProviderClientError> {
    if let Some(cl) = response.content_length() {
        if usize::try_from(cl).map_or(true, |s| s > MAX_RESPONSE_SIZE) {
            return Err(ProviderClientError::ResponseTooLarge { size: cl });
        }
    }
    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .and_then(|size| usize::try_from(size).ok())
            .unwrap_or(0),
    );
    while let Some(chunk) = response.chunk().await? {
        let next_len = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or(ProviderClientError::ResponseTooLarge { size: u64::MAX })?;
        if next_len > MAX_RESPONSE_SIZE {
            return Err(ProviderClientError::ResponseTooLarge {
                size: next_len as u64,
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Read a response body with size limit and deserialize it as JSON.
pub async fn json_with_limit<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, ProviderClientError> {
    serde_json::from_slice(&bytes_with_limit(response).await?).map_err(Into::into)
}

/// Read a UTF-8 response body with the shared provider size limit.
pub async fn text_with_limit(response: reqwest::Response) -> Result<String, ProviderClientError> {
    String::from_utf8(bytes_with_limit(response).await?)
        .map_err(|error| ProviderClientError::Parse(error.to_string()))
}

/// Check HTTP response status before processing body.
///
/// For HTTP 429 (Too Many Requests) and 503 (Service Unavailable) responses,
/// the `Retry-After` header is parsed and stored in the error so that callers
/// can respect the server's backoff hint.
pub async fn check_response(
    resp: reqwest::Response,
) -> Result<reqwest::Response, ProviderClientError> {
    let status = resp.status();
    if status.is_client_error() || status.is_server_error() {
        let retry_after_secs = if status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
        {
            resp.headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
        } else {
            None
        };
        let url = resp.url().to_string();
        let body = read_error_body_preview(resp).await;
        return Err(ProviderClientError::Http {
            status,
            url,
            retry_after_secs,
            body,
        });
    }
    Ok(resp)
}

async fn read_error_body_preview(resp: reqwest::Response) -> String {
    let mut resp = resp;
    let mut preview = Vec::with_capacity(MAX_ERROR_BODY_PREVIEW_BYTES);
    let mut truncated = false;

    while preview.len() < MAX_ERROR_BODY_PREVIEW_BYTES {
        let next = resp.chunk().await;
        let Some(chunk) = next.ok().flatten() else {
            break;
        };

        let remaining = MAX_ERROR_BODY_PREVIEW_BYTES - preview.len();
        if chunk.len() > remaining {
            preview.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }

        preview.extend_from_slice(&chunk);
    }

    if preview.len() >= MAX_ERROR_BODY_PREVIEW_BYTES {
        truncated = true;
    }

    let text = String::from_utf8_lossy(&preview).into_owned();
    if truncated {
        format!("{text}...(truncated)")
    } else {
        text
    }
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

impl From<prost::DecodeError> for ProviderClientError {
    fn from(err: prost::DecodeError) -> Self {
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
///
/// When the error contains a `retry_after_secs` value (from HTTP 429 responses),
/// the retry will sleep for at least that duration before the next attempt,
/// even if the backoff schedule would have used a shorter delay.
pub async fn with_retry<F, Fut, T>(op: F) -> Result<T, ProviderClientError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, ProviderClientError>>,
{
    use backon::Retryable;
    use std::sync::{Arc, Mutex};

    // Shared cell that the notify closure writes and the sleep closure reads.
    // notify() is called synchronously with the error and the scheduled backoff
    // duration; if a Retry-After header is present and exceeds the backoff we
    // store the TOTAL desired sleep so the async sleep() closure can await it.
    let extra_sleep: Arc<Mutex<Option<std::time::Duration>>> = Arc::new(Mutex::new(None));
    let extra_sleep_notify = Arc::clone(&extra_sleep);
    let extra_sleep_sleeper = Arc::clone(&extra_sleep);

    op.retry(provider_backoff())
        .when(|e: &ProviderClientError| e.is_retryable())
        .notify(move |e: &ProviderClientError, dur: std::time::Duration| {
            // If the server sent a Retry-After header, honor it by sleeping
            // for the total time requested by the server.
            if let ProviderClientError::Http { retry_after_secs: Some(secs), .. } = e {
                let retry_after = std::time::Duration::from_secs(*secs);
                if retry_after > dur {
                    tracing::info!(
                        "Honoring Retry-After: sleeping {retry_after:?} (server requested {secs}s, backoff was {dur:?})"
                    );
                    if let Ok(mut guard) = extra_sleep_notify.lock() {
                        *guard = Some(retry_after);
                    }
                    return;
                }
            }
            // No Retry-After override; clear any leftover value.
            if let Ok(mut guard) = extra_sleep_notify.lock() {
                *guard = None;
            }
        })
        .sleep(move |dur: std::time::Duration| {
            // Check whether the notify closure requested a longer sleep.
            let sleep_dur = extra_sleep_sleeper
                .lock()
                .ok()
                .and_then(|mut guard| guard.take())
                .unwrap_or(dur);
            tokio::time::sleep(sleep_dur)
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

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
            body: String::new(),
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn test_is_retryable_429_too_many_requests() {
        let err = ProviderClientError::Http {
            status: reqwest::StatusCode::TOO_MANY_REQUESTS,
            url: "https://api.bilibili.com/x/player/wbi/playurl".to_string(),
            retry_after_secs: Some(5),
            body: String::new(),
        };
        assert!(err.is_retryable(), "HTTP 429 should be retryable");
        // Verify retry_after_secs is captured
        if let ProviderClientError::Http {
            retry_after_secs, ..
        } = &err
        {
            assert_eq!(*retry_after_secs, Some(5));
        }
    }

    #[test]
    fn test_is_not_retryable_404() {
        let err = ProviderClientError::Http {
            status: reqwest::StatusCode::NOT_FOUND,
            url: "https://example.com".to_string(),
            retry_after_secs: None,
            body: String::new(),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_is_not_retryable_403() {
        let err = ProviderClientError::Http {
            status: reqwest::StatusCode::FORBIDDEN,
            url: "https://example.com".to_string(),
            retry_after_secs: None,
            body: String::new(),
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
