use super::*;

type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn missing(message: &'static str) -> Box<dyn std::error::Error + Send + Sync> {
    anyhow::anyhow!(message).into()
}

#[test]
fn test_client_creation() -> TestResult {
    let client = EmbyClient::new("https://emby.example.com")?;
    assert_eq!(client.host(), "https://emby.example.com");
    assert!(!client.has_credentials());

    let client_with_creds =
        EmbyClient::with_credentials("https://emby.example.com", "test_token", "user123")?;
    assert!(client_with_creds.has_credentials());
    Ok(())
}

#[test]
fn test_detection_candidates_do_not_use_hostname_heuristics() -> TestResult {
    let emby_client = EmbyClient::new("https://emby.example.com")?;
    assert_eq!(
        emby_client.detection_candidates()?,
        vec![String::new(), "/emby".to_string(), "/jellyfin".to_string()]
    );

    let jellyfin_client = EmbyClient::new("https://jellyfin.example.com")?;
    assert_eq!(
        jellyfin_client.detection_candidates()?,
        vec![String::new(), "/emby".to_string(), "/jellyfin".to_string()]
    );
    Ok(())
}

#[test]
fn test_client_host() -> TestResult {
    let client = EmbyClient::new("https://emby.myserver.com:8096")?;
    assert_eq!(client.host(), "https://emby.myserver.com:8096");
    Ok(())
}

#[test]
fn test_client_credentials() -> TestResult {
    let client = EmbyClient::with_credentials("https://emby.example.com", "token123", "user456")?;
    assert!(client.has_credentials());
    Ok(())
}

#[test]
fn test_set_credentials() -> TestResult {
    let mut client = EmbyClient::new("https://emby.example.com")?;
    assert!(!client.has_credentials());
    client.set_credentials("token", "user");
    assert!(client.has_credentials());
    Ok(())
}

#[test]
fn test_auth_response_deserialize() -> TestResult {
    let json = r#"{
        "AccessToken": "abc123xyz",
        "User": {"Id": "user1", "Name": "Admin"}
    }"#;
    let resp: crate::emby::types::AuthResponse = serde_json::from_str(json)?;
    assert_eq!(resp.access_token, "abc123xyz");
    assert_eq!(resp.user.id, "user1");
    assert_eq!(resp.user.name, "Admin");
    Ok(())
}

#[test]
fn test_items_response_deserialize() -> TestResult {
    let json = r#"{
        "Items": [
            {
                "Id": "item1",
                "Name": "Movie 1",
                "Type": "Movie",
                "IsFolder": false
            },
            {
                "Id": "folder1",
                "Name": "Series",
                "Type": "Series",
                "IsFolder": true
            }
        ],
        "TotalRecordCount": 2
    }"#;
    let resp: crate::emby::types::ItemsResponse = serde_json::from_str(json)?;
    assert_eq!(resp.total_record_count, 2);
    assert_eq!(resp.items.len(), 2);
    assert_eq!(resp.items[0].name, "Movie 1");
    assert!(!resp.items[0].is_folder);
    assert!(resp.items[1].is_folder);
    Ok(())
}

#[test]
fn test_item_with_media_sources() -> TestResult {
    let json = r#"{
        "Id": "video1",
        "Name": "Test Video",
        "Type": "Movie",
        "IsFolder": false,
        "MediaSources": [
            {
                "Id": "src1",
                "Name": "Direct",
                "Path": "/path/to/video.mkv",
                "Container": "mkv",
                "Protocol": "File",
                "SupportsDirectPlay": true,
                "SupportsTranscoding": true,
                "MediaStreams": [
                    {"Codec": "h264", "Type": "Video", "Index": 0, "IsDefault": true},
                    {"Codec": "aac", "Type": "Audio", "Language": "eng", "Index": 1, "IsDefault": true}
                ]
            }
        ],
        "RunTimeTicks": 72000000000
    }"#;
    let item: crate::emby::types::Item = serde_json::from_str(json)?;
    assert_eq!(item.media_sources.len(), 1);
    assert_eq!(item.media_sources[0].container, "mkv");
    assert_eq!(item.media_sources[0].media_streams.len(), 2);
    assert!(item.media_sources[0].supports_direct_play);
    assert_eq!(item.run_time_ticks, Some(72_000_000_000));
    Ok(())
}

#[test]
fn test_user_info_deserialize() -> TestResult {
    let json = r#"{
        "Id": "user1",
        "Name": "TestUser",
        "ServerId": "server1",
        "Policy": {
            "IsAdministrator": true,
            "IsHidden": false,
            "IsDisabled": false,
            "EnableAllFolders": true
        }
    }"#;
    let user: crate::emby::types::UserInfo = serde_json::from_str(json)?;
    assert_eq!(user.id, "user1");
    let policy = user
        .policy
        .as_ref()
        .ok_or_else(|| missing("user policy should deserialize"))?;
    assert!(policy.is_administrator);
    assert!(!policy.is_disabled);
    Ok(())
}

#[test]
fn test_user_info_no_policy() -> TestResult {
    let json = r#"{"Id": "user1", "Name": "TestUser", "ServerId": "server1"}"#;
    let user: crate::emby::types::UserInfo = serde_json::from_str(json)?;
    assert!(user.policy.is_none());
    Ok(())
}

#[test]
fn test_playback_info_response_deserialize() -> TestResult {
    let json = r#"{
        "PlaySessionId": "session123",
        "MediaSources": [
            {"Id": "src1", "Container": "mp4", "Protocol": "Http", "SupportsDirectPlay": true, "SupportsTranscoding": false}
        ]
    }"#;
    let resp: crate::emby::types::PlaybackInfoResponse = serde_json::from_str(json)?;
    assert_eq!(resp.play_session_id, "session123");
    assert_eq!(resp.media_sources.len(), 1);
    Ok(())
}

#[test]
fn test_default_device_profile() -> TestResult {
    let profile = crate::emby::types::default_device_profile();
    assert!(profile.get("DirectPlayProfiles").is_some());
    assert!(profile.get("TranscodingProfiles").is_some());
    assert!(profile.get("SubtitleProfiles").is_some());
    let direct_play = profile["DirectPlayProfiles"]
        .as_array()
        .ok_or_else(|| missing("DirectPlayProfiles should be an array"))?;
    assert!(!direct_play.is_empty());
    Ok(())
}

#[test]
fn test_media_stream_to_proto() {
    let stream = crate::emby::types::MediaStream {
        codec: "h264".to_string(),
        language: "eng".to_string(),
        stream_type: "Video".to_string(),
        title: String::new(),
        display_title: "1080p H.264".to_string(),
        display_language: "English".to_string(),
        is_default: true,
        index: 0,
        protocol: String::new(),
        delivery_url: String::new(),
    };
    let proto: crate::transport_dto::emby::MediaStreamInfo = stream.into();
    assert_eq!(proto.codec, "h264");
    assert_eq!(proto.language, "eng");
    assert!(proto.is_default);
}

#[test]
fn test_item_to_proto() {
    let item = crate::emby::types::Item {
        id: "item1".to_string(),
        name: "Test Movie".to_string(),
        item_type: "Movie".to_string(),
        overview: None,
        short_overview: None,
        is_folder: false,
        parent_id: Some("parent1".to_string()),
        series_name: None,
        series_id: None,
        season_name: None,
        season_id: None,
        collection_type: None,
        media_sources: vec![],
        run_time_ticks: None,
        production_year: Some(2024),
        image_tags: None,
    };
    let proto: crate::transport_dto::emby::Item = item.into();
    assert_eq!(proto.id, "item1");
    assert_eq!(proto.name, "Test Movie");
    assert_eq!(proto.parent_id, "parent1");
    assert_eq!(proto.series_name, ""); // None -> empty
    assert!(!proto.has_thumbnail);
}

#[test]
fn test_item_to_proto_preserves_thumbnail_presence() {
    let item = crate::emby::types::Item {
        id: "item1".to_string(),
        name: "Test Movie".to_string(),
        item_type: "Movie".to_string(),
        overview: None,
        short_overview: None,
        is_folder: false,
        parent_id: None,
        series_name: None,
        series_id: None,
        season_name: None,
        season_id: None,
        collection_type: None,
        media_sources: vec![],
        run_time_ticks: None,
        production_year: None,
        image_tags: Some(crate::emby::types::ImageTags {
            primary: Some("primary-tag".to_string()),
            thumb: None,
        }),
    };
    let proto: crate::transport_dto::emby::Item = item.into();
    assert!(proto.has_thumbnail);
}

#[test]
fn test_user_policy_to_proto() {
    let policy = crate::emby::types::UserPolicy {
        is_administrator: true,
        is_hidden: false,
        is_disabled: false,
        enable_all_folders: true,
    };
    let proto: crate::transport_dto::emby::UserPolicy = policy.into();
    assert!(proto.is_administrator);
    assert!(!proto.is_hidden);
    assert!(proto.enable_all_folders);
}

#[test]
fn test_validate_item_id_normal() {
    assert!(validate_item_id("12345").is_ok());
    assert!(validate_item_id("abc-def-ghi").is_ok());
    assert!(validate_item_id("a1b2c3d4e5f6").is_ok());
}

#[test]
fn test_validate_item_id_rejects_traversal() {
    assert!(validate_item_id("../etc/passwd").is_err());
    assert!(validate_item_id("..").is_err());
    assert!(validate_item_id("foo/bar").is_err());
    assert!(validate_item_id("foo\\bar").is_err());
}

#[test]
fn test_validate_item_id_rejects_empty() {
    assert!(validate_item_id("").is_err());
}

#[test]
fn test_validate_item_id_rejects_null_bytes() {
    assert!(validate_item_id("abc\0def").is_err());
}

#[test]
fn test_detection_candidates_preserve_host_path_first() -> TestResult {
    let client = EmbyClient::new("https://media.example.com/jellyfin")?;
    assert_eq!(
        client.detection_candidates()?,
        vec!["/jellyfin".to_string(), String::new(), "/emby".to_string()]
    );
    Ok(())
}

#[test]
fn test_api_prefix_hostname_does_not_affect_candidates() -> TestResult {
    let client = EmbyClient::new("https://jellyfin.home.local:8096")?;
    assert_eq!(
        client.detection_candidates()?,
        vec![String::new(), "/emby".to_string(), "/jellyfin".to_string()]
    );
    Ok(())
}

#[test]
fn test_endpoint_url_uses_root_for_emby_and_jellyfin_root_deployments() -> TestResult {
    let emby_client = EmbyClient::new("https://emby.example.com")?;
    assert_eq!(
        emby_client.endpoint_url_with_prefix("System/Info", "")?,
        "https://emby.example.com/System/Info"
    );

    let jellyfin_client = EmbyClient::new("https://jellyfin.example.com")?;
    assert_eq!(
        jellyfin_client.endpoint_url_with_prefix("System/Info", "")?,
        "https://jellyfin.example.com/System/Info"
    );
    Ok(())
}

#[test]
fn test_endpoint_url_uses_host_path_prefix() -> TestResult {
    let client = EmbyClient::new("https://media.example.com/custom-prefix")?;
    assert_eq!(
        client.endpoint_url_with_prefix("System/Info", "/custom-prefix")?,
        "https://media.example.com/custom-prefix/System/Info"
    );
    Ok(())
}
