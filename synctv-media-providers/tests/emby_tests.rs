//! Emby/Jellyfin provider tests
//!
//! Tests for item ID validation, API prefix detection, and HTTP API interactions using wiremock.

#![allow(clippy::unwrap_used)]
use synctv_media_providers::emby::{EmbyInterface, EmbyService};
use synctv_media_providers::transport_dto::emby::{login_req, LoginReq, LoginResp};
use synctv_media_providers::EmbyClient;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// validate_item_id is a private function tested indirectly through get_item which calls it.
// The inline tests in client.rs already cover it, but we verify through the public API here.

async fn mock_public_info(server: &MockServer, prefix: &str) {
    let public_info_path = if prefix.is_empty() {
        "/System/Info/Public".to_string()
    } else {
        format!("{prefix}/System/Info/Public")
    };

    Mock::given(method("GET"))
        .and(path(public_info_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Id": "server-1",
            "ServerName": "Mock Emby Compatible Server",
            "Version": "4.8.0"
        })))
        .mount(server)
        .await;
}

async fn login_through_service(password: &str) -> LoginResp {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    Mock::given(method("POST"))
        .and(path("/emby/Users/authenticatebyname"))
        .and(body_partial_json(serde_json::json!({
            "Username": "guest",
            "Pw": password,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "AccessToken": "guest-token",
            "User": {
                "Id": "guest-id",
                "Name": "guest"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/emby/Users/guest-id"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Id": "guest-id",
            "Name": "guest",
            "ServerId": "server-1"
        })))
        .expect(1)
        .mount(&server)
        .await;

    EmbyService::with_client(reqwest::Client::new())
        .login(LoginReq {
            host: server.uri(),
            username: "guest".to_string(),
            credential: Some(login_req::Credential::Password(password.to_string())),
        })
        .await
        .expect("Emby password login should succeed")
}

#[tokio::test]
async fn test_emby_service_allows_passwordless_accounts() {
    let response = login_through_service("").await;

    assert_eq!(response.token, "guest-token");
    assert_eq!(response.user_id, "guest-id");
}

#[tokio::test]
async fn test_emby_service_preserves_password_whitespace() {
    let response = login_through_service("  secret  ").await;

    assert_eq!(response.token, "guest-token");
}

#[tokio::test]
async fn test_validate_item_id_normal() {
    // Valid IDs should pass validation (but will fail on missing user_id since we don't set it)
    let client =
        EmbyClient::with_credentials("https://emby.example.com", "token", "user1").unwrap();
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
    let client =
        EmbyClient::with_credentials("https://emby.example.com", "token", "user1").unwrap();
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
    let client =
        EmbyClient::with_credentials("https://emby.example.com", "token", "user1").unwrap();
    let result = client.get_item("").await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("must not be empty"),
        "Empty ID should be rejected, got: {err_msg}"
    );
}

#[test]
fn test_get_api_prefix_jellyfin_hostname() {
    let client = EmbyClient::new("https://jellyfin.example.com").unwrap();
    assert_eq!(client.host(), "https://jellyfin.example.com");
}

#[test]
fn test_get_api_prefix_emby_hostname() {
    let client = EmbyClient::new("https://emby.example.com").unwrap();
    assert_eq!(client.host(), "https://emby.example.com");
}

#[test]
fn test_get_api_prefix_invalid_url_fallback() {
    // Hostnames are not used to infer Emby/Jellyfin deployment paths.
    let client = EmbyClient::new("https://media.example.com").unwrap();
    assert_eq!(client.host(), "https://media.example.com");
}

#[tokio::test]
async fn test_emby_client_login_success() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    // Emby login endpoint: POST /emby/Users/authenticatebyname
    Mock::given(method("POST"))
        .and(path("/emby/Users/authenticatebyname"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "AccessToken": "emby-test-token-xyz",
            "User": {
                "Id": "user-uuid-123",
                "Name": "admin"
            }
        })))
        .mount(&server)
        .await;

    let mut client = EmbyClient::new(server.uri()).unwrap();
    let (token, user_id) = client.login("admin", "password123").await.unwrap();
    assert_eq!(token, "emby-test-token-xyz");
    assert_eq!(user_id, "user-uuid-123");
    assert!(client.has_credentials());
}

#[tokio::test]
async fn test_emby_client_get_items_success() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    // Emby items endpoint: GET /emby/Users/<user_id>/Items
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
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
        })))
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let resp = client.get_items(None, None).await.unwrap();
    assert_eq!(resp.total_record_count, 2);
    assert_eq!(resp.items.len(), 2);
    assert_eq!(resp.items[0].name, "Test Movie");
    assert!(!resp.items[0].is_folder);
    assert!(resp.items[1].is_folder);
}

#[tokio::test]
async fn test_emby_client_me_uses_current_user_endpoint_when_user_id_is_empty() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    Mock::given(method("GET"))
        .and(path("/emby/Users/Me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Id": "user-uuid-123",
            "Name": "admin",
            "ServerId": "server-1"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "").unwrap();
    let resp = client.me().await.unwrap();
    assert_eq!(resp.id, "user-uuid-123");
    assert_eq!(resp.name, "admin");
}

#[tokio::test]
async fn test_emby_client_list_users_success() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    Mock::given(method("GET"))
        .and(path("/emby/Users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "Id": "user-uuid-123",
                "Name": "admin",
                "ServerId": "server-1",
                "Policy": {
                    "IsAdministrator": true
                }
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "").unwrap();
    let users = client.list_users().await.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].id, "user-uuid-123");
    assert_eq!(users[0].name, "admin");
    assert!(users[0]
        .policy
        .as_ref()
        .is_some_and(|policy| policy.is_administrator));
}

#[tokio::test]
async fn test_emby_client_jellyfin_detection() {
    let server = MockServer::start().await;
    let server_uri = server.uri();
    mock_public_info(&server, "/jellyfin").await;

    // SyncTV detects Jellyfin-compatible deployments by probing public system endpoints.
    Mock::given(method("POST"))
        .and(path("/jellyfin/Users/authenticatebyname"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "AccessToken": "jellyfin-token",
            "User": {
                "Id": "jf-user-1",
                "Name": "admin"
            }
        })))
        .mount(&server)
        .await;

    let mut client = EmbyClient::new(&server_uri).unwrap();
    let (token, user_id) = client.login("admin", "password").await.unwrap();
    assert_eq!(token, "jellyfin-token");
    assert_eq!(user_id, "jf-user-1");
}

#[tokio::test]
async fn test_emby_client_reuses_detected_api_base_path_within_client() {
    let server = MockServer::start().await;
    let public_info_path = "/emby/System/Info/Public";

    Mock::given(method("GET"))
        .and(path(public_info_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Id": "server-cache",
            "ServerName": "Cached Emby Compatible Server",
            "Version": "4.8.0"
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/emby/Users/authenticatebyname"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "AccessToken": "cached-token",
            "User": {
                "Id": "cached-user",
                "Name": "admin"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/emby/Users/cached-user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Id": "cached-user",
            "Name": "admin",
            "ServerId": "server-cache"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = EmbyClient::new(server.uri()).unwrap();
    client.login("admin", "password").await.unwrap();
    let current_user = client.me().await.unwrap();
    assert_eq!(current_user.id, "cached-user");
}

#[tokio::test]
async fn test_emby_client_respects_host_path_prefix() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/proxy/emby").await;

    Mock::given(method("GET"))
        .and(path("/proxy/emby/System/Info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Id": "server-1",
            "ServerName": "Reverse Proxy Emby",
            "Version": "4.8.0.0"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = EmbyClient::new(format!("{}/proxy/emby", server.uri())).unwrap();
    let info = client.get_system_info().await.unwrap();
    assert_eq!(info.id, "server-1");
    assert_eq!(info.server_name, "Reverse Proxy Emby");
}

#[tokio::test]
async fn test_emby_report_playback_start_success() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client
        .report_playback_start("item-abc", "session-1", Some("source-1"), 0)
        .await;
    assert!(result.is_ok(), "Playback start should succeed: {result:?}");
}

#[tokio::test]
async fn test_emby_report_playback_stop_success() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing/Stopped"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client
        .report_playback_stop("item-abc", "session-1", 50000)
        .await;
    assert!(result.is_ok(), "Playback stop should succeed: {result:?}");
}

#[tokio::test]
async fn test_emby_report_playback_progress_success() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing/Progress"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
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
    mock_public_info(&server, "/emby").await;

    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing/Progress"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
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
    mock_public_info(&server, "/emby").await;

    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
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
    let result = client.report_playback_start("", "session-1", None, 0).await;
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
    mock_public_info(&server, "/emby").await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "MediaSources": [
                {
                    "Id": "source-1",
                    "Name": "1080P",
                    "Path": "/media/movie.mkv",
                    "Container": "mkv",
                    "Size": 5_000_000_000_u64,
                    "Bitrate": 8_000_000,
                    "RunTimeTicks": 72_000_000_000_i64,
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
        })))
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let resp = client
        .get_playback_info(synctv_media_providers::emby::PlaybackInfoRequest {
            item_id: "item-1",
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(resp.play_session_id, "play-session-abc");
    assert_eq!(resp.media_sources.len(), 1);
    assert_eq!(resp.media_sources[0].id, "source-1");
    assert_eq!(resp.media_sources[0].container, "mkv");
}

#[tokio::test]
async fn test_emby_get_playback_info_sends_playback_request_controls() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    Mock::given(method("POST"))
        .and(path("/emby/Items/item-1/PlaybackInfo"))
        .and(body_partial_json(serde_json::json!({
            "UserId": "user-uuid-123",
            "MaxStreamingBitrate": 8_000_000_i64,
            "MaxAudioChannels": 2,
            "EnableDirectPlay": false,
            "EnableDirectStream": false,
            "EnableTranscoding": true,
            "DeviceProfile": {
                "DirectPlayProfiles": [],
                "TranscodingProfiles": [
                    {
                        "Container": "ts",
                        "Protocol": "hls",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac"
                    }
                ],
                "SubtitleProfiles": [
                    {
                        "Format": "srt",
                        "Method": "External"
                    }
                ]
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "MediaSources": [],
            "PlaySessionId": "play-session-xyz"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let resp = client
        .get_playback_info(synctv_media_providers::emby::PlaybackInfoRequest {
            item_id: "item-1",
            max_streaming_bitrate: Some(8_000_000),
            max_audio_channels: Some(2),
            enable_direct_play: Some(false),
            enable_direct_stream: Some(false),
            enable_transcoding: Some(true),
            device_profile: Some(
                &synctv_media_providers::grpc::emby::PlaybackInfoDeviceProfile {
                    direct_play_profiles: Vec::new(),
                    transcoding_container: "ts".to_string(),
                    transcoding_protocol: "hls".to_string(),
                    transcoding_video_codec: "h264".to_string(),
                    transcoding_audio_codec: "aac".to_string(),
                    subtitle_profiles: vec![
                        synctv_media_providers::grpc::emby::SubtitleProfileHint {
                            format: "srt".to_string(),
                            method:
                                synctv_media_providers::grpc::emby::SubtitleDeliveryMethod::External
                                    as i32,
                        },
                    ],
                },
            ),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(resp.play_session_id, "play-session-xyz");
}

#[tokio::test]
async fn test_emby_get_playback_info_preserves_explicit_empty_subtitle_profiles() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    Mock::given(method("POST"))
        .and(path("/emby/Items/item-1/PlaybackInfo"))
        .and(body_partial_json(serde_json::json!({
            "UserId": "user-uuid-123",
            "DeviceProfile": {
                "SubtitleProfiles": []
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "MediaSources": [],
            "PlaySessionId": "play-session-no-subtitles"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let resp = client
        .get_playback_info(synctv_media_providers::emby::PlaybackInfoRequest {
            item_id: "item-1",
            device_profile: Some(
                &synctv_media_providers::grpc::emby::PlaybackInfoDeviceProfile {
                    direct_play_profiles: Vec::new(),
                    transcoding_container: "ts".to_string(),
                    transcoding_protocol: "hls".to_string(),
                    transcoding_video_codec: "h264".to_string(),
                    transcoding_audio_codec: "aac".to_string(),
                    subtitle_profiles: Vec::new(),
                },
            ),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(resp.play_session_id, "play-session-no-subtitles");
}

#[tokio::test]
async fn test_emby_logout_success() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Logout"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client.logout().await;
    assert!(result.is_ok(), "Logout should succeed: {result:?}");
}

// Thumbnail extraction from ImageTags tests (M-1)

/// Test extracting thumbnail from ImageTags.Primary
#[tokio::test]
async fn emby_thumbnail_extract_from_primary_tag() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
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
        })))
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
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
    mock_public_info(&server, "/emby").await;

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
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
        })))
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let resp = client.get_items(None, None).await.unwrap();
    assert_eq!(resp.items.len(), 1);

    let item = &resp.items[0];
    assert!(item.image_tags.is_some());
    let image_tags = item.image_tags.as_ref().unwrap();
    assert!(image_tags.primary.is_none());
    assert!(image_tags.thumb.is_some());
    assert_eq!(image_tags.thumb.as_ref().unwrap(), "thumb456tag");
}

/// Test that no thumbnail is returned when `ImageTags` is missing
#[tokio::test]
async fn emby_thumbnail_no_image_tags_returns_none() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
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
        })))
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let resp = client.get_items(None, None).await.unwrap();
    assert_eq!(resp.items.len(), 1);

    let item = &resp.items[0];
    assert!(item.image_tags.is_none());
}

/// Test thumbnail URL construction with image tag
#[test]
fn emby_thumbnail_url_construction() {
    use synctv_media_providers::emby::ImageTags;

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

// Retry mechanism tests for report_playback_* and logout methods

/// Test that report_playback_start retries on 5xx server errors
#[tokio::test]
async fn test_emby_report_playback_start_retries_on_5xx() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    // First request returns 500, second returns 204
    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal error"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client
        .report_playback_start("item-abc", "session-1", Some("source-1"), 0)
        .await;
    assert!(
        result.is_ok(),
        "Playback start should succeed after retry: {result:?}"
    );
}

/// Test that report_playback_stop retries on 5xx server errors
#[tokio::test]
async fn test_emby_report_playback_stop_retries_on_5xx() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    // First request returns 502, second returns 204
    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing/Stopped"))
        .respond_with(ResponseTemplate::new(502).set_body_string("Bad Gateway"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing/Stopped"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client
        .report_playback_stop("item-abc", "session-1", 50000)
        .await;
    assert!(
        result.is_ok(),
        "Playback stop should succeed after retry: {result:?}"
    );
}

/// Test that report_playback_progress retries on 5xx server errors
#[tokio::test]
async fn test_emby_report_playback_progress_retries_on_5xx() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    // First request returns 503, second returns 204
    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing/Progress"))
        .respond_with(ResponseTemplate::new(503).set_body_string("Service Unavailable"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing/Progress"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client
        .report_playback_progress("item-abc", "session-1", Some("source-1"), 25000, false)
        .await;
    assert!(
        result.is_ok(),
        "Playback progress should succeed after retry: {result:?}"
    );
}

/// Test that logout retries on 5xx server errors
#[tokio::test]
async fn test_emby_logout_retries_on_5xx() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    // First request returns 500, second returns 204
    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Logout"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal error"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Logout"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client.logout().await;
    assert!(
        result.is_ok(),
        "Logout should succeed after retry: {result:?}"
    );
}

/// Test that delete_active_encodings retries on 5xx server errors
#[tokio::test]
async fn test_emby_delete_active_encodings_retries_on_5xx() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    // First request returns 500, second returns 204
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal error"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client.delete_active_encodings("session-1").await;
    assert!(
        result.is_ok(),
        "Delete active encodings should succeed after retry: {result:?}"
    );
}

/// Test that report_playback_start does NOT retry on 4xx client errors
#[tokio::test]
async fn test_emby_report_playback_start_no_retry_on_4xx() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    // 401 should NOT be retried
    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
        .expect(1) // Should only be called once (no retries)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client
        .report_playback_start("item-abc", "session-1", None, 0)
        .await;
    assert!(result.is_err(), "4xx errors should fail immediately");
    let err = result.unwrap_err();
    // Verify it's an HTTP error with 401 status
    let err_string = err.to_string();
    assert!(
        err_string.contains("401"),
        "Expected 401 error, got: {err_string}"
    );
}

/// Test that logout does NOT retry on 4xx client errors
#[tokio::test]
async fn test_emby_logout_no_retry_on_4xx() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    // 403 should NOT be retried
    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Logout"))
        .respond_with(ResponseTemplate::new(403).set_body_string("Forbidden"))
        .expect(1) // Should only be called once (no retries)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client.logout().await;
    assert!(result.is_err(), "4xx errors should fail immediately");
    let err = result.unwrap_err();
    let err_string = err.to_string();
    assert!(
        err_string.contains("403"),
        "Expected 403 error, got: {err_string}"
    );
}

/// Test that report_playback_start retries on 429 (Too Many Requests)
#[tokio::test]
async fn test_emby_report_playback_start_retries_on_429() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    // First request returns 429, second returns 204
    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing"))
        .respond_with(ResponseTemplate::new(429).set_body_string("Rate limited"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client
        .report_playback_start("item-abc", "session-1", None, 0)
        .await;
    assert!(
        result.is_ok(),
        "429 should be retried and succeed: {result:?}"
    );
}

/// Test that multiple 5xx errors are retried up to max times
#[tokio::test]
async fn test_emby_report_playback_stop_multiple_retries_exhausted() {
    let server = MockServer::start().await;
    mock_public_info(&server, "/emby").await;

    // All requests return 500 (more than max retries)
    Mock::given(method("POST"))
        .and(path("/emby/Sessions/Playing/Stopped"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal error"))
        .expect(4) // 1 initial + 3 retries = 4 total calls
        .mount(&server)
        .await;

    let client = EmbyClient::with_credentials(server.uri(), "token123", "user-uuid-123").unwrap();
    let result = client
        .report_playback_stop("item-abc", "session-1", 50000)
        .await;
    assert!(
        result.is_err(),
        "Should fail after exhausting retries: {result:?}"
    );
    let err = result.unwrap_err();
    let err_string = err.to_string();
    assert!(
        err_string.contains("500"),
        "Expected 500 error, got: {err_string}"
    );
}
