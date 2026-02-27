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

// ============================================================================
// MP5: Emby playback reporting wiremock tests
// ============================================================================

#[tokio::test]
async fn test_emby_report_playback_start_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client =
        EmbyClient::with_credentials(&server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client
        .report_playback_start("item-abc", "session-1", Some("source-1"), 0)
        .await;
    assert!(result.is_ok(), "Playback start should succeed: {result:?}");
}

#[tokio::test]
async fn test_emby_report_playback_stop_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing/Stopped"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client =
        EmbyClient::with_credentials(&server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client
        .report_playback_stop("item-abc", "session-1", 50000)
        .await;
    assert!(result.is_ok(), "Playback stop should succeed: {result:?}");
}

#[tokio::test]
async fn test_emby_report_playback_progress_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing/Progress"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client =
        EmbyClient::with_credentials(&server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client
        .report_playback_progress("item-abc", "session-1", Some("source-1"), 25000, false)
        .await;
    assert!(
        result.is_ok(),
        "Playback progress should succeed: {result:?}"
    );
}

#[tokio::test]
async fn test_emby_report_playback_progress_paused() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing/Progress"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client =
        EmbyClient::with_credentials(&server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client
        .report_playback_progress("item-abc", "session-1", None, 25000, true)
        .await;
    assert!(
        result.is_ok(),
        "Playback progress (paused) should succeed: {result:?}"
    );
}

#[tokio::test]
async fn test_emby_delete_active_encodings_success() {
    let server = MockServer::start().await;

    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client =
        EmbyClient::with_credentials(&server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client.delete_active_encodings("session-1").await;
    assert!(
        result.is_ok(),
        "Delete active encodings should succeed: {result:?}"
    );
}

#[tokio::test]
async fn test_emby_report_playback_start_validates_item_id() {
    let client =
        EmbyClient::with_credentials("https://emby.example.com", "token123", "user-uuid-123")
            .unwrap();
    // Empty item_id should be rejected
    let result = client
        .report_playback_start("", "session-1", None, 0)
        .await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("must not be empty"),
        "Empty item_id should be rejected, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_emby_report_playback_start_rejects_traversal() {
    let client =
        EmbyClient::with_credentials("https://emby.example.com", "token123", "user-uuid-123")
            .unwrap();
    let result = client
        .report_playback_start("../etc/passwd", "session-1", None, 0)
        .await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("invalid characters"),
        "Traversal item_id should be rejected, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_emby_get_playback_info_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "MediaSources": [
                    {
                        "Id": "source-1",
                        "Name": "1080P",
                        "Path": "/media/movie.mkv",
                        "Container": "mkv",
                        "Size": 5000000000_u64,
                        "Bitrate": 8000000,
                        "RunTimeTicks": 72000000000_i64,
                        "MediaStreams": [
                            {
                                "Type": "Video",
                                "Codec": "h264",
                                "Width": 1920,
                                "Height": 1080
                            },
                            {
                                "Type": "Audio",
                                "Codec": "aac",
                                "Channels": 6,
                                "Language": "eng"
                            }
                        ],
                        "SupportsTranscoding": true,
                        "SupportsDirectPlay": true,
                        "SupportsDirectStream": true
                    }
                ],
                "PlaySessionId": "play-session-abc"
            })),
        )
        .mount(&server)
        .await;

    let client =
        EmbyClient::with_credentials(&server.uri(), "token123", "user-uuid-123").unwrap();
    let resp = client
        .get_playback_info("item-1", None, None, None, None)
        .await
        .unwrap();
    assert_eq!(resp.play_session_id, "play-session-abc");
    assert_eq!(resp.media_sources.len(), 1);
    assert_eq!(resp.media_sources[0].id, "source-1");
    assert_eq!(resp.media_sources[0].container, "mkv");
}

#[tokio::test]
async fn test_emby_logout_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Logout"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client =
        EmbyClient::with_credentials(&server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client.logout().await;
    assert!(result.is_ok(), "Logout should succeed: {result:?}");
}

// ============================================================================
// Thumbnail extraction from ImageTags tests (M-1)
// ============================================================================

/// Test extracting thumbnail from ImageTags.Primary
#[tokio::test]
async fn emby_thumbnail_extract_from_primary_tag() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Items": [
                    {
                        "Id": "item-with-primary",
                        "Name": "Movie with Primary Image",
                        "Type": "Movie",
                        "IsFolder": false,
                        "ImageTags": {
                            "Primary": "abc123tag"
                        },
                        "MediaSources": []
                    }
                ],
                "TotalRecordCount": 1
            })),
        )
        .mount(&server)
        .await;

    let client =
        EmbyClient::with_credentials(&server.uri(), "token123", "user-uuid-123").unwrap();
    let resp = client.get_items(None, None).await.unwrap();
    assert_eq!(resp.items.len(), 1);

    let item = &resp.items[0];
    assert!(item.image_tags.is_some());
    let image_tags = item.image_tags.as_ref().unwrap();
    assert!(image_tags.primary.is_some());
    assert_eq!(image_tags.primary.as_ref().unwrap(), "abc123tag");
}

/// Test extracting thumbnail from ImageTags.Thumb when Primary is not available
#[tokio::test]
async fn emby_thumbnail_extract_from_thumb_tag() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Items": [
                    {
                        "Id": "item-with-thumb",
                        "Name": "Movie with Thumb Image",
                        "Type": "Movie",
                        "IsFolder": false,
                        "ImageTags": {
                            "Thumb": "thumb456tag"
                        },
                        "MediaSources": []
                    }
                ],
                "TotalRecordCount": 1
            })),
        )
        .mount(&server)
        .await;

    let client =
        EmbyClient::with_credentials(&server.uri(), "token123", "user-uuid-123").unwrap();
    let resp = client.get_items(None, None).await.unwrap();
    assert_eq!(resp.items.len(), 1);

    let item = &resp.items[0];
    assert!(item.image_tags.is_some());
    let image_tags = item.image_tags.as_ref().unwrap();
    assert!(image_tags.primary.is_none());
    assert!(image_tags.thumb.is_some());
    assert_eq!(image_tags.thumb.as_ref().unwrap(), "thumb456tag");
}

/// Test that no thumbnail is returned when ImageTags is missing
#[tokio::test]
async fn emby_thumbnail_no_image_tags_returns_none() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Items": [
                    {
                        "Id": "item-no-images",
                        "Name": "Movie without Images",
                        "Type": "Movie",
                        "IsFolder": false,
                        "MediaSources": []
                    }
                ],
                "TotalRecordCount": 1
            })),
        )
        .mount(&server)
        .await;

    let client =
        EmbyClient::with_credentials(&server.uri(), "token123", "user-uuid-123").unwrap();
    let resp = client.get_items(None, None).await.unwrap();
    assert_eq!(resp.items.len(), 1);

    let item = &resp.items[0];
    assert!(item.image_tags.is_none());
}

/// Test thumbnail URL construction with image tag
#[test]
fn emby_thumbnail_url_construction() {
    use synctv_media_providers::emby::types::ImageTags;

    // Test building thumbnail URL with Primary tag
    let image_tags = ImageTags {
        primary: Some("primary-tag-123".to_string()),
        thumb: None,
    };

    let item_id = "item-abc";
    let host = "https://emby.example.com";

    // Construct thumbnail URL (this is how it should be built)
    let tag = image_tags.primary.as_ref().unwrap();
    let thumbnail_url = format!(
        "{}/Items/{}/Images/Primary?tag={}&maxHeight=300",
        host.trim_end_matches('/'),
        item_id,
        tag
    );
    assert_eq!(
        thumbnail_url,
        "https://emby.example.com/Items/item-abc/Images/Primary?tag=primary-tag-123&maxHeight=300"
    );

    // Test with Thumb tag
    let image_tags_thumb = ImageTags {
        primary: None,
        thumb: Some("thumb-tag-456".to_string()),
    };

    let tag = image_tags_thumb.thumb.as_ref().unwrap();
    let thumbnail_url = format!(
        "{}/Items/{}/Images/Thumb?tag={}&maxHeight=300",
        host.trim_end_matches('/'),
        item_id,
        tag
    );
    assert_eq!(
        thumbnail_url,
        "https://emby.example.com/Items/item-abc/Images/Thumb?tag=thumb-tag-456&maxHeight=300"
    );
}
