use super::*;
use reqwest::StatusCode;
use synctv_media_providers::ProviderClientError;
use tonic::metadata::MetadataValue;

/// Test that `resolve_alist_client` returns local client when no channel is provided
#[test]
fn test_resolve_alist_client_returns_local_when_no_channel() {
    let manager =
        ProviderClientManager::new_for_tests().expect("default provider HTTP client should build");
    let local_client = manager.local_alist_client();
    let resolved_client = manager.resolve_alist_client(None);

    assert!(Arc::ptr_eq(&local_client, &resolved_client));
}

/// Test that `resolve_bilibili_client` returns local client when no channel is provided
#[test]
fn test_resolve_bilibili_client_returns_local_when_no_channel() {
    let manager =
        ProviderClientManager::new_for_tests().expect("default provider HTTP client should build");
    let local_client = manager.local_bilibili_client();
    let resolved_client = manager.resolve_bilibili_client(None);

    assert!(Arc::ptr_eq(&local_client, &resolved_client));
}

/// Test that `resolve_emby_client` returns local client when no channel is provided
#[test]
fn test_resolve_emby_client_returns_local_when_no_channel() {
    let manager =
        ProviderClientManager::new_for_tests().expect("default provider HTTP client should build");
    let local_client = manager.local_emby_client();
    let resolved_client = manager.resolve_emby_client(None);

    assert!(Arc::ptr_eq(&local_client, &resolved_client));
}

/// Test that `ProviderClientManager::with_custom_clients` allows mock injection
#[test]
fn test_custom_clients_injection() {
    // Create custom clients
    let custom_alist: AlistClientArc = Arc::new(
        synctv_media_providers::alist::AlistService::new()
            .expect("provider HTTP client should build"),
    );
    let custom_bilibili: BilibiliClientArc = Arc::new(
        synctv_media_providers::bilibili::BilibiliService::new()
            .expect("provider HTTP client should build"),
    );
    let custom_emby: EmbyClientArc = Arc::new(
        synctv_media_providers::emby::EmbyService::new()
            .expect("provider HTTP client should build"),
    );

    // Store Arc pointers for comparison
    let alist_ptr = Arc::as_ptr(&custom_alist);
    let bilibili_ptr = Arc::as_ptr(&custom_bilibili);
    let emby_ptr = Arc::as_ptr(&custom_emby);

    // Create manager with custom clients
    let manager =
        ProviderClientManager::with_custom_clients(custom_alist, custom_bilibili, custom_emby);

    // Verify the manager uses the custom clients
    let alist = manager.local_alist_client();
    let bilibili = manager.local_bilibili_client();
    let emby = manager.local_emby_client();

    assert_eq!(Arc::as_ptr(&alist), alist_ptr);
    assert_eq!(Arc::as_ptr(&bilibili), bilibili_ptr);
    assert_eq!(Arc::as_ptr(&emby), emby_ptr);
}

#[test]
fn test_build_grpc_request_inserts_x_provider_secret() {
    let request = build_grpc_request(Some("shared-secret"), 42_u32).expect("request should build");
    assert_eq!(request.get_ref(), &42_u32);
    assert_eq!(
        request.metadata().get("x-provider-secret"),
        Some(&MetadataValue::from_static("shared-secret"))
    );
}

#[test]
fn test_build_grpc_request_omits_header_when_secret_is_blank() {
    let request = build_grpc_request(Some("   "), 42_u32).expect("request should build");
    assert_eq!(request.get_ref(), &42_u32);
    assert!(
        request.metadata().get("x-provider-secret").is_none(),
        "blank secrets must not produce a malformed header"
    );
}

#[test]
fn test_validate_auth_secret_rejects_empty_secret() {
    let error = validate_auth_secret(Some("   ")).expect_err("empty secret must fail");
    assert!(matches!(
        error,
        ProviderError::InvalidConfig(message)
            if message.contains("auth secret must not be empty")
    ));
}

#[test]
fn test_validate_auth_secret_allows_absent_secret_only_for_non_remote_callers() {
    assert_eq!(validate_auth_secret(None).unwrap(), None);
    assert_eq!(
        validate_auth_secret(Some("  shared-secret  ")).unwrap(),
        Some("shared-secret")
    );
}

#[test]
fn test_validate_auth_secret_rejects_non_ascii_secret() {
    let error = validate_auth_secret(Some("sëcret")).expect_err("non-ASCII secret must fail");
    assert!(matches!(
        error,
        ProviderError::InvalidConfig(message)
            if message.contains("valid ASCII gRPC metadata")
    ));
}

#[test]
fn test_validate_auth_secret_rejects_control_characters() {
    let error = validate_auth_secret(Some("shared\nsecret")).expect_err("control chars must fail");
    assert!(matches!(
        error,
        ProviderError::InvalidConfig(message)
            if message.contains("valid ASCII gRPC metadata")
    ));
}

#[test]
fn test_map_grpc_status_unauthenticated_to_auth() {
    let error = map_grpc_status("login", &Status::unauthenticated("Invalid provider secret"));
    assert!(matches!(
        error,
        ProviderClientError::Auth(message) if message == "Invalid provider secret"
    ));
}

#[test]
fn test_map_grpc_status_invalid_argument_to_invalid_config() {
    let error = map_grpc_status(
        "fs_get",
        &Status::invalid_argument("missing host parameter"),
    );
    assert!(matches!(
        error,
        ProviderClientError::InvalidConfig(message) if message == "missing host parameter"
    ));
}

#[test]
fn test_map_grpc_status_not_found_to_http_404() {
    let error = map_grpc_status("me", &Status::not_found("user not found"));
    assert!(matches!(
        error,
        ProviderClientError::Http { status, ref url, ref body, retry_after_secs: None }
            if status == StatusCode::NOT_FOUND
                && url == "http://remote/me"
                && body == "user not found"
    ));
}

#[test]
fn test_map_grpc_status_unimplemented_to_not_implemented() {
    let error = map_grpc_status("future_method", &Status::unimplemented("not available"));
    assert!(matches!(
        error,
        ProviderClientError::NotImplemented(message) if message == "not available"
    ));
}
