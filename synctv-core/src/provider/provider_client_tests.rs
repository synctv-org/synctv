use super::*;
use crate::test_helpers::TestResultExt;

/// Test that `resolve_alist_client` returns local client when no remote connection is provided
#[test]
fn test_resolve_alist_client_returns_local_without_remote_connection() {
    let manager = ProviderClientManager::new().checked("default provider HTTP client should build");
    let local_client = manager.local_alist_client();
    let resolved_client = manager.resolve_alist_client(None);

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
fn provider_not_found_errors_map_to_not_found() {
    let api_error = synctv_media_providers::ProviderClientError::Api {
        code: 404,
        message: "missing".to_string(),
    };
    assert!(matches!(
        ProviderError::from(api_error),
        ProviderError::NotFound
    ));

    let http_error = synctv_media_providers::ProviderClientError::Http {
        status: reqwest::StatusCode::NOT_FOUND,
        url: "https://provider.example/items/missing".to_string(),
        retry_after_secs: None,
        body: String::new(),
    };
    assert!(matches!(
        ProviderError::from(http_error),
        ProviderError::NotFound
    ));
}
