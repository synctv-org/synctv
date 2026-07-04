use super::*;
use crate::test_helpers::TestResultExt;

/// Test that `resolve_alist_client` returns local client when no remote connection is provided
#[test]
fn test_resolve_alist_client_returns_local_without_remote_connection() {
    let manager =
        ProviderClientManager::new_for_tests().checked("default provider HTTP client should build");
    let local_client = manager.local_alist_client();
    let resolved_client = manager.resolve_alist_client(None);

    assert!(Arc::ptr_eq(&local_client, &resolved_client));
}

/// Test that `resolve_bilibili_client` returns local client when no remote connection is provided
#[test]
fn test_resolve_bilibili_client_returns_local_without_remote_connection() {
    let manager =
        ProviderClientManager::new_for_tests().checked("default provider HTTP client should build");
    let local_client = manager.local_bilibili_client();
    let resolved_client = manager.resolve_bilibili_client(None);

    assert!(Arc::ptr_eq(&local_client, &resolved_client));
}

/// Test that `resolve_emby_client` returns local client when no remote connection is provided
#[test]
fn test_resolve_emby_client_returns_local_without_remote_connection() {
    let manager =
        ProviderClientManager::new_for_tests().checked("default provider HTTP client should build");
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
            .checked("provider HTTP client should build"),
    );
    let custom_bilibili: BilibiliClientArc = Arc::new(
        synctv_media_providers::bilibili::BilibiliService::new()
            .checked("provider HTTP client should build"),
    );
    let custom_emby: EmbyClientArc = Arc::new(
        synctv_media_providers::emby::EmbyService::new()
            .checked("provider HTTP client should build"),
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
fn test_validate_auth_secret_rejects_empty_secret() {
    let error = validate_auth_secret(Some("   ")).failed("empty secret must fail");
    assert!(matches!(
        error,
        ProviderError::InvalidConfig(message)
            if message.contains("auth secret must not be empty")
    ));
}

#[test]
fn test_validate_auth_secret_allows_absent_secret_only_for_non_remote_callers() {
    assert_eq!(
        validate_auth_secret(None).checked("operation should succeed"),
        None
    );
    assert_eq!(
        validate_auth_secret(Some("  shared-secret  ")).checked("operation should succeed"),
        Some("shared-secret")
    );
}

#[test]
fn test_validate_auth_secret_rejects_non_ascii_secret() {
    let error = validate_auth_secret(Some("sëcret")).failed("non-ASCII secret must fail");
    assert!(matches!(
        error,
        ProviderError::InvalidConfig(message)
            if message.contains("valid ASCII remote transport metadata")
    ));
}

#[test]
fn test_validate_auth_secret_rejects_control_characters() {
    let error = validate_auth_secret(Some("shared\nsecret")).failed("control chars must fail");
    assert!(matches!(
        error,
        ProviderError::InvalidConfig(message)
            if message.contains("valid ASCII remote transport metadata")
    ));
}
