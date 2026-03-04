//! Shared gRPC error mapping utilities.
//!
//! Provides a common function for converting `ProviderClientError` into
//! `tonic::Status` with appropriate gRPC status codes.
//! This replaces the near-identical `map_*_error` functions that were
//! duplicated in each `*_server.rs` module.
//!
//! Since `AlistError` and `EmbyError` are type aliases for
//! `ProviderClientError`, a single mapping function handles all providers.

use tonic::Status;

use crate::error::ProviderClientError;

/// Map a `ProviderClientError` to a `tonic::Status` with an appropriate gRPC
/// status code.
///
/// The `context` parameter is a human-readable label for the RPC being called
/// (e.g., "login", "`fs_get`") and is included in the status message.
///
/// Mapping rules:
/// - Auth errors -> `UNAUTHENTICATED`
/// - HTTP 401/403 -> `PERMISSION_DENIED`
/// - HTTP 404 -> `NOT_FOUND`
/// - HTTP 429 -> `RESOURCE_EXHAUSTED`
/// - HTTP 5xx -> `UNAVAILABLE`
/// - Network errors -> `UNAVAILABLE`
/// - Parse errors -> `INTERNAL`
/// - Invalid config -> `INVALID_ARGUMENT`
/// - Not implemented -> `UNIMPLEMENTED`
/// - Response too large -> `RESOURCE_EXHAUSTED`
/// - API errors -> mapped by code (401/403/404 -> corresponding status, others -> `INTERNAL`)
#[must_use]
pub fn map_provider_error(context: &str, e: &ProviderClientError) -> Status {
    match e {
        ProviderClientError::Auth(_) => {
            Status::unauthenticated(format!("{context}: authentication failed"))
        }
        ProviderClientError::Http { status, .. } => match status.as_u16() {
            401 | 403 => Status::permission_denied(format!("{context}: access denied")),
            404 => Status::not_found(format!("{context}: resource not found")),
            429 => Status::resource_exhausted(format!("{context}: rate limited")),
            s if s >= 500 => Status::unavailable(format!("{context}: upstream server error")),
            _ => Status::internal(format!("{context}: request failed")),
        },
        ProviderClientError::Network(_) => Status::unavailable(format!("{context}: network error")),
        ProviderClientError::Parse(_) => {
            Status::internal(format!("{context}: failed to parse response"))
        }
        ProviderClientError::InvalidConfig(_) => {
            Status::invalid_argument(format!("{context}: invalid configuration"))
        }
        ProviderClientError::InvalidHeader(_) => {
            Status::internal(format!("{context}: invalid header"))
        }
        ProviderClientError::NotImplemented(_) => {
            Status::unimplemented(format!("{context}: not implemented"))
        }
        ProviderClientError::ResponseTooLarge { size } => {
            Status::resource_exhausted(format!("{context}: response too large ({size} bytes)"))
        }
        ProviderClientError::Api { code, .. } => match code {
            401 | 403 => Status::permission_denied(format!("{context}: access denied")),
            404 => Status::not_found(format!("{context}: resource not found")),
            -412 | 429 => Status::resource_exhausted(format!("{context}: rate limited")),
            _ => Status::internal(format!("{context}: API error (code {code})")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_error_maps_to_unauthenticated() {
        let err = ProviderClientError::Auth("bad token".to_string());
        let status = map_provider_error("login", &err);
        assert_eq!(status.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn http_404_maps_to_not_found() {
        let err = ProviderClientError::Http {
            status: reqwest::StatusCode::NOT_FOUND,
            url: "https://example.com".to_string(),
            retry_after_secs: None,
            body: String::new(),
        };
        let status = map_provider_error("fs_get", &err);
        assert_eq!(status.code(), tonic::Code::NotFound);
    }

    #[test]
    fn http_500_maps_to_unavailable() {
        let err = ProviderClientError::Http {
            status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            url: "https://example.com".to_string(),
            retry_after_secs: None,
            body: String::new(),
        };
        let status = map_provider_error("get_items", &err);
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    #[test]
    fn network_error_maps_to_unavailable() {
        let err = ProviderClientError::Network("timeout".to_string());
        let status = map_provider_error("get_items", &err);
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    #[test]
    fn parse_error_maps_to_internal() {
        let err = ProviderClientError::Parse("unexpected EOF".to_string());
        let status = map_provider_error("fetch", &err);
        assert_eq!(status.code(), tonic::Code::Internal);
    }

    #[test]
    fn response_too_large_maps_to_resource_exhausted() {
        let err = ProviderClientError::ResponseTooLarge { size: 20_000_000 };
        let status = map_provider_error("fetch", &err);
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
    }

    #[test]
    fn api_error_403_maps_to_permission_denied() {
        let err = ProviderClientError::Api {
            code: 403,
            message: "forbidden".to_string(),
        };
        let status = map_provider_error("access", &err);
        assert_eq!(status.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn api_error_neg412_maps_to_resource_exhausted() {
        let err = ProviderClientError::Api {
            code: -412,
            message: "rate limited".to_string(),
        };
        let status = map_provider_error("get_video_url", &err);
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
    }

    #[test]
    fn api_error_unknown_maps_to_internal() {
        let err = ProviderClientError::Api {
            code: -999,
            message: "unknown error".to_string(),
        };
        let status = map_provider_error("get_video_url", &err);
        assert_eq!(status.code(), tonic::Code::Internal);
    }

    #[test]
    fn invalid_config_maps_to_invalid_argument() {
        let err = ProviderClientError::InvalidConfig("missing host".to_string());
        let status = map_provider_error("init", &err);
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn not_implemented_maps_to_unimplemented() {
        let err = ProviderClientError::NotImplemented("feature X".to_string());
        let status = map_provider_error("call", &err);
        assert_eq!(status.code(), tonic::Code::Unimplemented);
    }

    #[test]
    fn http_429_maps_to_resource_exhausted() {
        let err = ProviderClientError::Http {
            status: reqwest::StatusCode::TOO_MANY_REQUESTS,
            url: "https://example.com".to_string(),
            retry_after_secs: Some(60),
            body: String::new(),
        };
        let status = map_provider_error("get_video_url", &err);
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
    }
}
