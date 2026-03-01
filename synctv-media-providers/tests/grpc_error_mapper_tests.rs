//! Integration tests for gRPC `error_mapper`
//!
//! Verifies that `AlistError`, `BilibiliError`, and `EmbyError` all correctly
//! map to the expected `tonic::Status` codes through the shared
//! `map_provider_error` function.

#![allow(clippy::unwrap_used)]
use synctv_media_providers::grpc::error_mapper::map_provider_error;

// ============================================================================
// AlistError -> tonic::Status mapping
// ============================================================================

#[test]
fn test_alist_auth_error_to_unauthenticated() {
    use synctv_media_providers::alist::AlistError;
    let err = AlistError::Auth("invalid token".to_string());
    let status = map_provider_error("alist_login", &err);
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
    assert!(status.message().contains("alist_login"));
}

#[test]
fn test_alist_api_error_401_to_permission_denied() {
    use synctv_media_providers::alist::AlistError;
    let err = AlistError::Api {
        code: 401,
        message: "token expired".to_string(),
    };
    let status = map_provider_error("fs_get", &err);
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
}

#[test]
fn test_alist_api_error_404_to_not_found() {
    use synctv_media_providers::alist::AlistError;
    let err = AlistError::Api {
        code: 404,
        message: "file not found".to_string(),
    };
    let status = map_provider_error("fs_get", &err);
    assert_eq!(status.code(), tonic::Code::NotFound);
}

#[test]
fn test_alist_network_to_unavailable() {
    use synctv_media_providers::alist::AlistError;
    let err = AlistError::Network("connection refused".to_string());
    let status = map_provider_error("alist_connect", &err);
    assert_eq!(status.code(), tonic::Code::Unavailable);
}

#[test]
fn test_alist_parse_to_internal() {
    use synctv_media_providers::alist::AlistError;
    let err = AlistError::Parse("invalid JSON".to_string());
    let status = map_provider_error("alist_parse", &err);
    assert_eq!(status.code(), tonic::Code::Internal);
}

#[test]
fn test_alist_invalid_config_to_invalid_argument() {
    use synctv_media_providers::alist::AlistError;
    let err = AlistError::InvalidConfig("missing host".to_string());
    let status = map_provider_error("alist_init", &err);
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

// ============================================================================
// BilibiliError -> tonic::Status mapping
// ============================================================================

#[test]
fn test_bilibili_auth_error_to_unauthenticated() {
    use synctv_media_providers::bilibili::BilibiliError;
    let err = BilibiliError::Auth("session expired".to_string());
    let status = map_provider_error("bilibili_user_info", &err);
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
}

#[test]
fn test_bilibili_http_403_to_permission_denied() {
    use synctv_media_providers::bilibili::BilibiliError;
    let err = BilibiliError::Http {
        status: reqwest::StatusCode::FORBIDDEN,
        url: "https://api.bilibili.com/x/player/wbi/playurl".to_string(),
        retry_after_secs: None,
        body: "access denied".to_string(),
    };
    let status = map_provider_error("get_video_url", &err);
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
}

#[test]
fn test_bilibili_http_429_to_resource_exhausted() {
    use synctv_media_providers::bilibili::BilibiliError;
    let err = BilibiliError::Http {
        status: reqwest::StatusCode::TOO_MANY_REQUESTS,
        url: "https://api.bilibili.com".to_string(),
        retry_after_secs: Some(30),
        body: String::new(),
    };
    // HTTP 429 Too Many Requests -> ResourceExhausted
    let status = map_provider_error("bilibili_api", &err);
    assert_eq!(status.code(), tonic::Code::ResourceExhausted);
}

#[test]
fn test_bilibili_http_502_to_unavailable() {
    use synctv_media_providers::bilibili::BilibiliError;
    let err = BilibiliError::Http {
        status: reqwest::StatusCode::BAD_GATEWAY,
        url: "https://api.bilibili.com".to_string(),
        retry_after_secs: None,
        body: String::new(),
    };
    let status = map_provider_error("bilibili_api", &err);
    assert_eq!(status.code(), tonic::Code::Unavailable);
}

#[test]
fn test_bilibili_network_to_unavailable() {
    use synctv_media_providers::bilibili::BilibiliError;
    let err = BilibiliError::Network("DNS resolution failed".to_string());
    let status = map_provider_error("bilibili_fetch", &err);
    assert_eq!(status.code(), tonic::Code::Unavailable);
}

#[test]
fn test_bilibili_api_error_code_to_internal() {
    use synctv_media_providers::bilibili::BilibiliError;
    let err = BilibiliError::Api {
        code: -412,
        message: "request intercepted".to_string(),
    };
    let status = map_provider_error("get_video_url", &err);
    assert_eq!(status.code(), tonic::Code::Internal);
    assert!(status.message().contains("-412"));
}

#[test]
fn test_bilibili_response_too_large_to_resource_exhausted() {
    use synctv_media_providers::bilibili::BilibiliError;
    let err = BilibiliError::ResponseTooLarge { size: 20_000_000 };
    let status = map_provider_error("fetch_page", &err);
    assert_eq!(status.code(), tonic::Code::ResourceExhausted);
    assert!(status.message().contains("20000000"));
}

// ============================================================================
// EmbyError -> tonic::Status mapping
// ============================================================================

#[test]
fn test_emby_auth_error_to_unauthenticated() {
    use synctv_media_providers::emby::EmbyError;
    let err = EmbyError::Auth("invalid API key".to_string());
    let status = map_provider_error("emby_login", &err);
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
}

#[test]
fn test_emby_http_401_to_permission_denied() {
    use synctv_media_providers::emby::EmbyError;
    let err = EmbyError::Http {
        status: reqwest::StatusCode::UNAUTHORIZED,
        url: "https://emby.example.com/emby/Items".to_string(),
        retry_after_secs: None,
        body: "unauthorized".to_string(),
    };
    let status = map_provider_error("get_items", &err);
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
}

#[test]
fn test_emby_http_503_to_unavailable() {
    use synctv_media_providers::emby::EmbyError;
    let err = EmbyError::Http {
        status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
        url: "https://emby.example.com".to_string(),
        retry_after_secs: None,
        body: "service unavailable".to_string(),
    };
    let status = map_provider_error("emby_fetch", &err);
    assert_eq!(status.code(), tonic::Code::Unavailable);
}

#[test]
fn test_emby_invalid_config_to_invalid_argument() {
    use synctv_media_providers::emby::EmbyError;
    let err = EmbyError::InvalidConfig("missing user_id".to_string());
    let status = map_provider_error("emby_init", &err);
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

#[test]
fn test_emby_not_implemented_to_unimplemented() {
    use synctv_media_providers::emby::EmbyError;
    let err = EmbyError::NotImplemented("live TV".to_string());
    let status = map_provider_error("emby_live_tv", &err);
    assert_eq!(status.code(), tonic::Code::Unimplemented);
}

#[test]
fn test_emby_invalid_header_to_internal() {
    use synctv_media_providers::emby::EmbyError;
    let err = EmbyError::InvalidHeader("non-ascii value".to_string());
    let status = map_provider_error("emby_build_headers", &err);
    assert_eq!(status.code(), tonic::Code::Internal);
}

// ============================================================================
// Context string is included in status message
// ============================================================================

#[test]
fn test_context_string_included_in_all_error_types() {
    use synctv_media_providers::error::ProviderClientError;

    let test_cases: Vec<(&str, ProviderClientError)> = vec![
        (
            "my_rpc",
            ProviderClientError::Auth("bad".to_string()),
        ),
        (
            "my_rpc",
            ProviderClientError::Network("timeout".to_string()),
        ),
        (
            "my_rpc",
            ProviderClientError::Parse("eof".to_string()),
        ),
        (
            "my_rpc",
            ProviderClientError::InvalidConfig("missing".to_string()),
        ),
        (
            "my_rpc",
            ProviderClientError::NotImplemented("feature".to_string()),
        ),
        (
            "my_rpc",
            ProviderClientError::ResponseTooLarge { size: 100 },
        ),
    ];

    for (context, err) in test_cases {
        let status = map_provider_error(context, &err);
        assert!(
            status.message().contains(context),
            "Status message should contain context '{}', got: '{}'",
            context,
            status.message()
        );
    }
}
