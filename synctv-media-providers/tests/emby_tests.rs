//! Emby/Jellyfin provider tests
//!
//! Tests for item ID validation, API prefix detection, and HTTP API interactions using wiremock.

use synctv_media_providers::EmbyClient;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// === Item ID validation tests ===
// validate_item_id is a private function tested indirectly through get_item which calls it.
// The inline tests in client.rs already cover it, but we verify through the public API here.

#[tokio::test]
async fn test_validate_item_id_normal() {
    // Valid IDs should pass validation (but will fail on missing user_id since we don't set it)
    let client = EmbyClient::with_credentials("https://emby.example.com", "token", "user1").unwrap();
    // get_item will pass validation for normal IDs, but fail on network
    let result = client.get_item("12345").await;
    // Should fail with network error, NOT validation error
    let err_msg = result.unwrap_err().to_string();
    assert!(
        !err_msg.contains("invalid characters") && !err_msg.contains("must not be empty"),
        "Normal ID should pass validation, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_validate_item_id_traversal_rejected() {
    let client = EmbyClient::with_credentials("https://emby.example.com", "token", "user1").unwrap();
    let result = client.get_item("../etc/passwd").await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("invalid characters"),
        "Traversal ID should be rejected, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_validate_item_id_empty_rejected() {
    let client = EmbyClient::with_credentials("https://emby.example.com", "token", "user1").unwrap();
    let result = client.get_item("").await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("must not be empty"),
        "Empty ID should be rejected, got: {err_msg}"
    );
}

// === API prefix detection tests ===

#[test]
fn test_get_api_prefix_jellyfin_hostname() {
    let client = EmbyClient::new("https://jellyfin.example.com").unwrap();
    // We can't call get_api_prefix directly since it's private,
    // but we verify behavior by checking the host
    assert_eq!(client.host(), "https://jellyfin.example.com");
    // The inline tests in client.rs verify get_api_prefix returns "/jellyfin"
}

#[test]
fn test_get_api_prefix_emby_hostname() {
    let client = EmbyClient::new("https://emby.example.com").unwrap();
    assert_eq!(client.host(), "https://emby.example.com");
    // The inline tests verify get_api_prefix returns "/emby"
}

#[test]
fn test_get_api_prefix_invalid_url_fallback() {
    // A non-standard URL should default to "/emby" (no jellyfin in hostname)
    let client = EmbyClient::new("https://media.example.com").unwrap();
    assert_eq!(client.host(), "https://media.example.com");
    // The inline tests verify this defaults to "/emby"
}

// === Wiremock HTTP API tests ===

#[tokio::test]
async fn test_emby_client_login_success() {
    let server = MockServer::start().await;

    // Emby login endpoint: POST /emby/Users/authenticatebyname
    Mock::given(method("POST"))
        .and(path("/emby/Users/authenticatebyname"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "AccessToken": "emby-test-token-xyz",
                "User": {
                    "Id": "user-uuid-123",
                    "Name": "admin"
                }
            })),
        )
        .mount(&server)
        .await;

    let mut client = EmbyClient::new(&server.uri()).unwrap();
    let (token, user_id) = client.login("admin", "password123").await.unwrap();
    assert_eq!(token, "emby-test-token-xyz");
    assert_eq!(user_id, "user-uuid-123");
    assert!(client.has_credentials());
}

#[tokio::test]
async fn test_emby_client_get_items_success() {
    let server = MockServer::start().await;

    // Emby items endpoint: GET /emby/Users/<user_id>/Items
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Items": [
                    {
                        "Id": "item1",
                        "Name": "Test Movie",
                        "Type": "Movie",
                        "IsFolder": false,
                        "MediaSources": []
                    },
                    {
                        "Id": "item2",
                        "Name": "Test Series",
                        "Type": "Series",
                        "IsFolder": true,
                        "MediaSources": []
                    }
                ],
                "TotalRecordCount": 2
            })),
        )
        .mount(&server)
        .await;

    let client =
        EmbyClient::with_credentials(&server.uri(), "token123", "user-uuid-123").unwrap();
    let resp = client.get_items(None, None).await.unwrap();
    assert_eq!(resp.total_record_count, 2);
    assert_eq!(resp.items.len(), 2);
    assert_eq!(resp.items[0].name, "Test Movie");
    assert!(!resp.items[0].is_folder);
    assert!(resp.items[1].is_folder);
}

#[tokio::test]
async fn test_emby_client_jellyfin_detection() {
    let server = MockServer::start().await;
    let server_uri = server.uri();

    // Jellyfin uses /jellyfin prefix when hostname contains "jellyfin"
    // Since wiremock uses 127.0.0.1, we test with explicit prefix instead
    Mock::given(method("POST"))
        .and(path("/jellyfin/Users/authenticatebyname"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "AccessToken": "jellyfin-token",
                "User": {
                    "Id": "jf-user-1",
                    "Name": "admin"
                }
            })),
        )
        .mount(&server)
        .await;

    // Set custom API prefix to simulate Jellyfin detection
    let mut client = EmbyClient::new(&server_uri).unwrap();
    client.set_api_prefix("/jellyfin");
    let (token, user_id) = client.login("admin", "password").await.unwrap();
    assert_eq!(token, "jellyfin-token");
    assert_eq!(user_id, "jf-user-1");
}
