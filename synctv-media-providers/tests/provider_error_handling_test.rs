//! Provider Error Handling and Type Conversion Tests
//!
//! This test suite covers:
//! 1. Provider error variants and display formatting
//! 2. Retryable error classification
//! 3. Error conversions (From impls)
//! 4. SSRF validation edge cases
//! 5. Alist type deserialization and proto conversion
//! 6. Emby type deserialization and proto conversion
//! 7. Bilibili type deserialization
//! 8. gRPC validation layer

#![allow(clippy::unwrap_used)]
use synctv_media_providers::error::*;

// === ProviderClientError Display Tests ===

#[test]
fn test_error_display_not_implemented() {
    let err = ProviderClientError::NotImplemented("subtitle parsing".to_string());
    assert_eq!(err.to_string(), "Not implemented: subtitle parsing");
}

#[test]
fn test_error_display_invalid_header() {
    let err = ProviderClientError::InvalidHeader("bad value".to_string());
    assert_eq!(err.to_string(), "Invalid header value: bad value");
}

#[test]
fn test_error_display_http_with_retry_after() {
    let err = ProviderClientError::Http {
        status: reqwest::StatusCode::TOO_MANY_REQUESTS,
        url: "https://api.bilibili.com/test".to_string(),
        retry_after_secs: Some(30),
        body: String::new(),
    };
    let msg = err.to_string();
    assert!(msg.contains("429"));
    assert!(msg.contains("https://api.bilibili.com/test"));
}

// === Retryable Error Classification ===

#[test]
fn test_is_retryable_bad_gateway() {
    let err = ProviderClientError::Http {
        status: reqwest::StatusCode::BAD_GATEWAY,
        url: "https://example.com".to_string(),
        retry_after_secs: None,
        body: String::new(),
    };
    assert!(err.is_retryable());
}

#[test]
fn test_is_retryable_service_unavailable() {
    let err = ProviderClientError::Http {
        status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
        url: "https://example.com".to_string(),
        retry_after_secs: None,
        body: String::new(),
    };
    assert!(err.is_retryable());
}

#[test]
fn test_is_retryable_gateway_timeout() {
    let err = ProviderClientError::Http {
        status: reqwest::StatusCode::GATEWAY_TIMEOUT,
        url: "https://example.com".to_string(),
        retry_after_secs: None,
        body: String::new(),
    };
    assert!(err.is_retryable());
}

#[test]
fn test_is_not_retryable_bad_request() {
    let err = ProviderClientError::Http {
        status: reqwest::StatusCode::BAD_REQUEST,
        url: "https://example.com".to_string(),
        retry_after_secs: None,
        body: String::new(),
    };
    assert!(!err.is_retryable());
}

#[test]
fn test_is_not_retryable_unauthorized() {
    let err = ProviderClientError::Http {
        status: reqwest::StatusCode::UNAUTHORIZED,
        url: "https://example.com".to_string(),
        retry_after_secs: None,
        body: String::new(),
    };
    assert!(!err.is_retryable());
}

#[test]
fn test_is_not_retryable_invalid_config() {
    let err = ProviderClientError::InvalidConfig("missing url".to_string());
    assert!(!err.is_retryable());
}

#[test]
fn test_is_not_retryable_response_too_large() {
    let err = ProviderClientError::ResponseTooLarge { size: 99_999_999 };
    assert!(!err.is_retryable());
}

#[test]
fn test_is_not_retryable_not_implemented() {
    let err = ProviderClientError::NotImplemented("feature".to_string());
    assert!(!err.is_retryable());
}

#[test]
fn test_is_not_retryable_invalid_header() {
    let err = ProviderClientError::InvalidHeader("bad".to_string());
    assert!(!err.is_retryable());
}

// === Error From impls ===

#[test]
fn test_error_from_serde_json_invalid_type() {
    // Test with a type mismatch error
    let json_err = serde_json::from_str::<Vec<String>>("123").unwrap_err();
    let err: ProviderClientError = json_err.into();
    assert!(matches!(err, ProviderClientError::Parse(_)));
    assert!(!err.is_retryable());
}

#[test]
fn test_error_from_serde_json_eof() {
    let json_err = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
    let err: ProviderClientError = json_err.into();
    assert!(matches!(err, ProviderClientError::Parse(_)));
}

// === Provider Backoff Configuration ===

#[test]
fn test_provider_backoff_creation() {
    let builder = provider_backoff();
    // Verify it creates without panicking
    let _ = builder;
}

// === MAX_RESPONSE_SIZE ===

#[test]
fn test_max_response_size_value() {
    assert_eq!(MAX_RESPONSE_SIZE, 16 * 1024 * 1024);
    assert_eq!(MAX_RESPONSE_SIZE, 16_777_216);
}

// === SSRF IP Validation Tests ===

mod ssrf_tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use synctv_common::ssrf::is_ip_blocked;

    #[test]
    fn test_blocked_ipv4_172_range_boundary() {
        assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(172, 16, 0, 0))));
        assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255))));
        assert!(!is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(172, 15, 0, 1))));
        assert!(!is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(172, 32, 0, 1))));
    }

    #[test]
    fn test_blocked_ipv4_cgnat_boundary() {
        assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(100, 64, 0, 0))));
        assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(
            100, 127, 255, 255
        ))));
        assert!(!is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(
            100, 63, 255, 255
        ))));
        assert!(!is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(100, 128, 0, 0))));
    }

    #[test]
    fn test_blocked_ipv4_multicast_range() {
        assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(224, 0, 0, 0))));
        assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(
            239, 255, 255, 255
        ))));
        assert!(!is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(
            223, 255, 255, 255
        ))));
    }

    #[test]
    fn test_blocked_ipv4_reserved_range() {
        assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(240, 0, 0, 0))));
        assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(250, 1, 2, 3))));
        assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::BROADCAST)));
    }

    #[test]
    fn test_blocked_ipv6_unique_local() {
        assert!(is_ip_blocked(&IpAddr::V6(Ipv6Addr::new(
            0xfc00, 0, 0, 0, 0, 0, 0, 1
        ))));
        assert!(is_ip_blocked(&IpAddr::V6(Ipv6Addr::new(
            0xfd00, 0, 0, 0, 0, 0, 0, 1
        ))));
    }

    #[test]
    fn test_blocked_ipv6_link_local() {
        assert!(is_ip_blocked(&IpAddr::V6(Ipv6Addr::new(
            0xfe80, 0, 0, 0, 0, 0, 0, 1
        ))));
        assert!(is_ip_blocked(&IpAddr::V6(Ipv6Addr::new(
            0xfebf, 0, 0, 0, 0, 0, 0, 1
        ))));
    }

    #[test]
    fn test_blocked_ipv6_multicast() {
        assert!(is_ip_blocked(&IpAddr::V6(Ipv6Addr::new(
            0xff00, 0, 0, 0, 0, 0, 0, 1
        ))));
        assert!(is_ip_blocked(&IpAddr::V6(Ipv6Addr::new(
            0xff02, 0, 0, 0, 0, 0, 0, 1
        ))));
    }

    #[test]
    fn test_allowed_ipv6_public() {
        // Note: 2001:db8::/32 is "Documentation and Reserved Purposes" per IANA,
        // so it should be blocked. Use actual public IPv6 addresses instead.
        // Cloudflare public DNS (2606:4700:4700::1111)
        assert!(!is_ip_blocked(&IpAddr::V6(Ipv6Addr::new(
            0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111
        ))));
        // Google public DNS (2001:4860:4860::8888)
        assert!(!is_ip_blocked(&IpAddr::V6(Ipv6Addr::new(
            0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888
        ))));
    }

    #[test]
    fn test_is_ip_blocked_dispatches_correctly() {
        assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(is_ip_blocked(&IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(!is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }
}

// === Alist Type Deserialization Tests ===

mod alist_type_tests {
    use synctv_media_providers::alist::types::*;

    #[test]
    fn test_alist_resp_deserialize_success() {
        let json = r#"{
            "code": 200,
            "message": "success",
            "data": {"token": "abc123"}
        }"#;
        let resp: AlistResp<LoginData> = serde_json::from_str(json).unwrap();
        assert_eq!(resp.code, 200);
        assert_eq!(resp.message, "success");
        assert_eq!(resp.data.unwrap().token, "abc123");
    }

    #[test]
    fn test_alist_resp_deserialize_error() {
        let json = r#"{
            "code": 401,
            "message": "unauthorized",
            "data": null
        }"#;
        let resp: AlistResp<LoginData> = serde_json::from_str(json).unwrap();
        assert_eq!(resp.code, 401);
        assert!(resp.data.is_none());
    }

    #[test]
    fn test_alist_fs_get_resp_deserialize() {
        let json = r#"{
            "name": "video.mp4",
            "size": 1024000,
            "is_dir": false,
            "modified": 1700000000,
            "created": 1699000000,
            "sign": "abc",
            "thumb": "",
            "type": 6,
            "hashinfo": "sha256:abc",
            "raw_url": "https://cdn.example.com/video.mp4",
            "readme": "",
            "provider": "local",
            "related": []
        }"#;
        let resp: HttpFsGetResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.name, "video.mp4");
        assert_eq!(resp.size, 1_024_000);
        assert!(!resp.is_dir);
        assert_eq!(resp.raw_url, "https://cdn.example.com/video.mp4");
    }

    #[test]
    fn test_alist_fs_get_resp_with_related() {
        let json = r#"{
            "name": "video.mp4",
            "size": 1024000,
            "is_dir": false,
            "related": [
                {"name": "subtitle.srt", "size": 5000, "is_dir": false}
            ]
        }"#;
        let resp: HttpFsGetResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.related.len(), 1);
        assert_eq!(resp.related[0].name, "subtitle.srt");
    }

    #[test]
    fn test_alist_fs_list_resp_deserialize() {
        let json = r#"{
            "content": [
                {"name": "file1.mp4", "size": 100, "is_dir": false},
                {"name": "folder1", "size": 0, "is_dir": true}
            ],
            "total": 2,
            "readme": "",
            "write": false,
            "provider": "local"
        }"#;
        let resp: HttpFsListResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.total, 2);
        assert_eq!(resp.content.len(), 2);
        assert!(!resp.content[0].is_dir);
        assert!(resp.content[1].is_dir);
    }

    #[test]
    fn test_alist_me_resp_deserialize() {
        let json = r#"{
            "id": 1,
            "username": "admin",
            "base_path": "/",
            "role": 2,
            "disabled": false,
            "permission": 0,
            "sso_id": "",
            "otp": false
        }"#;
        let resp: HttpMeResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, 1);
        assert_eq!(resp.username, "admin");
        assert!(!resp.disabled);
    }

    #[test]
    fn test_alist_search_resp_deserialize() {
        let json = r#"{
            "content": [
                {"parent": "/videos", "name": "movie.mkv", "is_dir": false, "size": 2000000, "type": 6}
            ],
            "total": 1
        }"#;
        let resp: HttpFsSearchResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.total, 1);
        assert_eq!(resp.content[0].parent, "/videos");
        assert_eq!(resp.content[0].name, "movie.mkv");
    }

    #[test]
    fn test_alist_fs_other_resp_with_preview() {
        let json = r#"{
            "drive_id": "d1",
            "file_id": "f1",
            "video_preview_play_info": {
                "category": "live_transcoding",
                "live_transcoding_subtitle_task_list": [],
                "live_transcoding_task_list": [
                    {"stage": "finished", "status": "finished", "template_height": 720, "template_id": "720p", "template_name": "HD", "template_width": 1280, "url": "https://cdn.example.com/720p.m3u8"}
                ],
                "meta": {"duration": 120.5, "height": 1080, "width": 1920}
            }
        }"#;
        let resp: HttpFsOtherResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.drive_id, "d1");
        let preview = resp.video_preview_play_info.unwrap();
        assert_eq!(preview.live_transcoding_task_list.len(), 1);
        assert_eq!(preview.live_transcoding_task_list[0].template_height, 720);
        let meta = preview.meta.unwrap();
        assert_eq!(meta.width, 1920);
        assert_eq!(meta.height, 1080);
    }

    #[test]
    fn test_alist_fs_other_resp_without_preview() {
        let json = r#"{
            "drive_id": "",
            "file_id": "",
            "video_preview_play_info": null
        }"#;
        let resp: HttpFsOtherResp = serde_json::from_str(json).unwrap();
        assert!(resp.video_preview_play_info.is_none());
    }
}

// === Emby Type Deserialization Tests ===

mod emby_type_tests {
    use synctv_media_providers::emby::types::*;

    #[test]
    fn test_emby_auth_response_deserialize() {
        let json = r#"{
            "AccessToken": "abc123token",
            "User": {"Id": "user1", "Name": "admin"}
        }"#;
        let resp: AuthResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.access_token, "abc123token");
        assert_eq!(resp.user.id, "user1");
        assert_eq!(resp.user.name, "admin");
    }

    #[test]
    fn test_emby_user_info_deserialize() {
        let json = r#"{
            "Id": "u1",
            "Name": "TestUser",
            "ServerId": "s1",
            "Policy": {
                "IsAdministrator": true,
                "IsHidden": false,
                "IsDisabled": false,
                "EnableAllFolders": true
            }
        }"#;
        let resp: UserInfo = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, "u1");
        assert_eq!(resp.name, "TestUser");
        let policy = resp.policy.unwrap();
        assert!(policy.is_administrator);
        assert!(!policy.is_hidden);
    }

    #[test]
    fn test_emby_user_info_without_policy() {
        let json = r#"{
            "Id": "u1",
            "Name": "TestUser",
            "ServerId": "s1"
        }"#;
        let resp: UserInfo = serde_json::from_str(json).unwrap();
        assert!(resp.policy.is_none());
    }

    #[test]
    fn test_emby_item_deserialize() {
        let json = r#"{
            "Id": "item1",
            "Name": "Test Movie",
            "Type": "Movie",
            "IsFolder": false,
            "MediaSources": [{
                "Id": "src1",
                "Name": "1080p",
                "Path": "/path/to/movie.mkv",
                "Container": "mkv",
                "Protocol": "File",
                "DefaultSubtitleStreamIndex": -1,
                "DefaultAudioStreamIndex": 1,
                "MediaStreams": [{
                    "Codec": "h264",
                    "Language": "eng",
                    "Type": "Video",
                    "Title": "H.264 1080p",
                    "DisplayTitle": "1080p H.264",
                    "DisplayLanguage": "",
                    "IsDefault": true,
                    "Index": 0,
                    "Protocol": "",
                    "DeliveryUrl": ""
                }],
                "DirectStreamUrl": "/direct/play/item1",
                "TranscodingUrl": "/transcode/item1",
                "SupportsDirectPlay": true,
                "SupportsTranscoding": true
            }]
        }"#;
        let item: Item = serde_json::from_str(json).unwrap();
        assert_eq!(item.id, "item1");
        assert_eq!(item.name, "Test Movie");
        assert_eq!(item.item_type, "Movie");
        assert!(!item.is_folder);
        assert_eq!(item.media_sources.len(), 1);
        assert_eq!(item.media_sources[0].container, "mkv");
        assert_eq!(item.media_sources[0].media_streams.len(), 1);
        assert_eq!(item.media_sources[0].media_streams[0].codec, "h264");
    }

    #[test]
    fn test_emby_items_response_deserialize() {
        let json = r#"{
            "Items": [
                {"Id": "i1", "Name": "Movie 1", "Type": "Movie", "MediaSources": []},
                {"Id": "i2", "Name": "Movie 2", "Type": "Movie", "MediaSources": []}
            ],
            "TotalRecordCount": 2
        }"#;
        let resp: ItemsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.total_record_count, 2);
        assert_eq!(resp.items.len(), 2);
    }

    #[test]
    fn test_emby_system_info_deserialize() {
        let json = r#"{
            "ServerName": "My Emby",
            "Version": "4.7.0",
            "OperatingSystem": "Linux",
            "Id": "server1",
            "HttpServerPortNumber": 8096
        }"#;
        let info: SystemInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.server_name, "My Emby");
        assert_eq!(info.version, "4.7.0");
        assert_eq!(info.http_server_port_number, 8096);
    }

    #[test]
    fn test_emby_playback_info_response_deserialize() {
        let json = r#"{
            "PlaySessionId": "session123",
            "MediaSources": [{
                "Id": "src1",
                "Name": "main",
                "Path": "/videos/test.mkv",
                "Container": "mkv",
                "Protocol": "File",
                "DefaultSubtitleStreamIndex": 0,
                "DefaultAudioStreamIndex": 1,
                "MediaStreams": [],
                "DirectStreamUrl": "",
                "TranscodingUrl": "/transcoding/123",
                "SupportsDirectPlay": true,
                "SupportsTranscoding": true
            }]
        }"#;
        let resp: PlaybackInfoResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.play_session_id, "session123");
        assert_eq!(resp.media_sources.len(), 1);
    }

    #[test]
    fn test_emby_default_device_profile() {
        let profile = default_device_profile();
        assert!(profile.is_object());
        let obj = profile.as_object().unwrap();
        assert!(obj.contains_key("DirectPlayProfiles"));
        assert!(obj.contains_key("TranscodingProfiles"));
        assert!(obj.contains_key("SubtitleProfiles"));

        // Verify direct play profiles contain expected containers
        let direct_play = obj["DirectPlayProfiles"].as_array().unwrap();
        assert!(!direct_play.is_empty());

        let containers: Vec<&str> = direct_play
            .iter()
            .filter_map(|p| p.get("Container").and_then(|c| c.as_str()))
            .collect();
        assert!(containers.contains(&"mp4,m4v"));
        assert!(containers.contains(&"mkv"));
        assert!(containers.contains(&"webm"));
    }

    #[test]
    fn test_emby_item_with_optional_fields() {
        let json = r#"{
            "Id": "item1",
            "Name": "Episode 1",
            "Type": "Episode",
            "IsFolder": false,
            "ParentId": "season1",
            "SeriesName": "Test Show",
            "SeriesId": "series1",
            "SeasonName": "Season 1",
            "SeasonId": "season1",
            "CollectionType": "tvshows",
            "MediaSources": [],
            "RunTimeTicks": 18000000000,
            "ProductionYear": 2024
        }"#;
        let item: Item = serde_json::from_str(json).unwrap();
        assert_eq!(item.parent_id, Some("season1".to_string()));
        assert_eq!(item.series_name, Some("Test Show".to_string()));
        assert_eq!(item.run_time_ticks, Some(18_000_000_000));
        assert_eq!(item.production_year, Some(2024));
    }
}

// === Bilibili Type Deserialization Tests ===

mod bilibili_type_tests {
    use synctv_media_providers::bilibili::types::*;

    #[test]
    fn test_season_info_resp_deserialize() {
        let json = r#"{
            "result": {
                "title": "Test Anime",
                "cover": "https://example.com/cover.jpg",
                "actors": "Actor 1, Actor 2",
                "episodes": [
                    {
                        "title": "EP1",
                        "long_title": "The Beginning",
                        "bvid": "BV1test",
                        "cid": 123,
                        "ep_id": 456,
                        "aid": 789,
                        "cover": "https://example.com/ep1.jpg",
                        "duration": 1440000
                    }
                ]
            },
            "message": "0",
            "code": 0
        }"#;
        let resp: SeasonInfoResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.result.title, "Test Anime");
        assert_eq!(resp.result.episodes.len(), 1);
        assert_eq!(resp.result.episodes[0].ep_id, 456);
    }

    #[test]
    fn test_live_page_data_deserialize() {
        let json = r#"{
            "data": {
                "title": "Live Stream",
                "user_cover": "https://example.com/cover.jpg",
                "uid": 12345,
                "room_id": 67890,
                "live_status": 1
            },
            "message": "ok",
            "code": 0
        }"#;
        let resp: ParseLivePageResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.title, "Live Stream");
        assert_eq!(resp.data.room_id, 67890);
        assert_eq!(resp.data.live_status, 1);
    }

    #[test]
    fn test_live_stream_resp_deserialize() {
        let json = r#"{
            "data": {
                "accept_quality": ["10000", "400"],
                "quality_description": [
                    {"desc": "Source", "qn": 10000},
                    {"desc": "720p", "qn": 400}
                ],
                "durl": [{"url": "https://live.bilibili.com/stream/123.flv", "order": 1}],
                "current_quality": 10000
            },
            "message": "ok",
            "code": 0,
            "ttl": 1
        }"#;
        let resp: GetLiveStreamResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.durl.len(), 1);
        assert_eq!(resp.data.current_quality, 10000);
        assert_eq!(resp.data.quality_description.len(), 2);
    }

    #[test]
    fn test_dash_video_resp_deserialize() {
        let json = r#"{
            "data": {
                "dash": {
                    "duration": 120.5,
                    "minBufferTime": 1.5,
                    "video": [{
                        "id": 80,
                        "baseUrl": "https://cdn.bilibili.com/video.m4s",
                        "backupUrl": [],
                        "mimeType": "video/mp4",
                        "codecs": "avc1.640032",
                        "width": 1920,
                        "height": 1080,
                        "frameRate": "30",
                        "bandwidth": 5000000,
                        "sar": "1:1",
                        "startWithSap": 1,
                        "SegmentBase": {"Initialization": "0-999", "indexRange": "1000-1999"}
                    }],
                    "audio": [{
                        "id": 30280,
                        "baseUrl": "https://cdn.bilibili.com/audio.m4s",
                        "backupUrl": [],
                        "mimeType": "audio/mp4",
                        "codecs": "mp4a.40.2",
                        "bandwidth": 128000,
                        "startWithSap": 1,
                        "SegmentBase": {"Initialization": "0-499", "indexRange": "500-999"}
                    }]
                },
                "support_formats": [{"quality": 80, "new_description": "1080P"}]
            },
            "message": "0",
            "code": 0,
            "ttl": 1
        }"#;
        let resp: DashVideoResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.dash.video.len(), 1);
        assert_eq!(resp.data.dash.audio.len(), 1);
        assert_eq!(resp.data.dash.video[0].width, 1920);
        assert_eq!(resp.data.dash.video[0].height, 1080);
        assert_eq!(resp.data.dash.duration, 120.5);
    }

    #[test]
    fn test_room_play_info_resp_deserialize() {
        let json = r#"{
            "code": 0,
            "message": "ok",
            "data": {
                "playurl_info": {
                    "playurl": {
                        "stream": [{
                            "protocol_name": "http_stream",
                            "format": [{
                                "codec": [{
                                    "current_qn": 10000,
                                    "accept_qn": [10000, 400],
                                    "base_url": "/live-bvc/stream",
                                    "url_info": [{"host": "https://d1.bilivideo.com", "extra": "?key=abc"}]
                                }]
                            }]
                        }]
                    }
                }
            }
        }"#;
        let resp: RoomPlayInfoResp = serde_json::from_str(json).unwrap();
        let playurl_info = resp.data.playurl_info.unwrap();
        let playurl = playurl_info.playurl.unwrap();
        assert_eq!(playurl.stream.len(), 1);
        assert_eq!(playurl.stream[0].protocol_name, "http_stream");
    }

    #[test]
    fn test_room_play_info_resp_no_playurl() {
        let json = r#"{
            "code": 0,
            "message": "ok",
            "data": {
                "playurl_info": null
            }
        }"#;
        let resp: RoomPlayInfoResp = serde_json::from_str(json).unwrap();
        assert!(resp.data.playurl_info.is_none());
    }

    #[test]
    fn test_player_v2_info_resp_deserialize() {
        let json = r#"{
            "data": {
                "subtitle": {
                    "subtitles": [
                        {"lan": "zh-Hans", "lan_doc": "Chinese (Simplified)", "subtitle_url": "https://example.com/sub.json", "id": 1}
                    ]
                }
            },
            "message": "0",
            "code": 0,
            "ttl": 1
        }"#;
        let resp: PlayerV2InfoResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.subtitle.subtitles.len(), 1);
        assert_eq!(resp.data.subtitle.subtitles[0].lan, "zh-Hans");
    }

    #[test]
    fn test_live_danmu_info_deserialize() {
        let json = r#"{
            "data": {
                "token": "secret_token",
                "host_list": [
                    {"host": "broadcastlv.chat.bilibili.com", "port": 2243, "ws_port": 2244, "wss_port": 443}
                ]
            },
            "message": "ok",
            "code": 0
        }"#;
        let resp: GetLiveDanmuInfoResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.token, "secret_token");
        assert_eq!(resp.data.host_list.len(), 1);
        assert_eq!(resp.data.host_list[0].wss_port, 443);
    }

    #[test]
    fn test_video_id_equality() {
        assert_eq!(
            VideoId::Bvid("BV1test".to_string()),
            VideoId::Bvid("BV1test".to_string())
        );
        assert_ne!(
            VideoId::Bvid("BV1a".to_string()),
            VideoId::Bvid("BV1b".to_string())
        );
        assert_eq!(VideoId::Aid(123), VideoId::Aid(123));
        assert_ne!(VideoId::Aid(1), VideoId::Aid(2));
        assert_ne!(VideoId::Bvid("test".to_string()), VideoId::Aid(1));
    }

    #[test]
    fn test_episode_id() {
        let ep = EpisodeId("ep12345".to_string());
        assert_eq!(ep.0, "ep12345");
        let ep2 = ep.clone();
        assert_eq!(ep, ep2);
    }

    #[test]
    fn test_pgc_url_resp_deserialize() {
        let json = r#"{
            "result": {
                "accept_quality": [80, 64],
                "accept_description": ["1080P", "720P"],
                "quality": 80,
                "durl": [{"url": "https://cdn.bilibili.com/pgc.flv", "size": 500000, "length": 60}]
            },
            "message": "ok",
            "code": 0
        }"#;
        let resp: PgcUrlResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.result.quality, 80);
        assert_eq!(resp.result.durl.len(), 1);
    }
}

// === gRPC Validation Tests ===

mod grpc_validation_tests {
    use synctv_media_providers::grpc::validation::*;

    #[test]
    fn test_validate_required_whitespace() {
        assert!(validate_required("field", " ").is_err());
        assert!(validate_required("field", "\t").is_err());
    }

    #[test]
    fn test_validate_host_cloud_metadata() {
        assert!(validate_host("http://169.254.169.254").is_ok());
        assert!(validate_host("http://metadata.google.internal").is_ok());
        assert!(validate_provider_grpc_host("http://169.254.169.254").is_ok());
        assert!(validate_provider_grpc_host("http://metadata.google.internal").is_ok());
    }

    #[test]
    fn test_validate_host_ipv4_mapped_ipv6() {
        assert!(validate_host("http://[::ffff:127.0.0.1]").is_ok());
        assert!(validate_provider_grpc_host("http://[::ffff:127.0.0.1]").is_ok());
    }

    #[test]
    fn test_validate_host_kubernetes_service() {
        assert!(validate_host("http://localhost").is_ok());
        assert!(validate_provider_grpc_host("http://localhost").is_ok());
    }
}

// === Error Type Alias Tests ===

#[test]
fn test_alist_error_is_provider_error() {
    use synctv_media_providers::AlistError;
    let err = AlistError::Auth("token expired".to_string());
    assert_eq!(err.to_string(), "Authentication failed: token expired");
    assert!(!err.is_retryable());
}

#[test]
fn test_bilibili_error_is_provider_error() {
    use synctv_media_providers::BilibiliError;
    let err = BilibiliError::Network("dns failed".to_string());
    assert_eq!(err.to_string(), "Network error: dns failed");
    assert!(err.is_retryable());
}

#[test]
fn test_emby_error_is_provider_error() {
    use synctv_media_providers::EmbyError;
    let err = EmbyError::Api {
        code: 500,
        message: "internal error".to_string(),
    };
    assert_eq!(err.to_string(), "API error (code 500): internal error");
    assert!(!err.is_retryable());
}

// === PROVIDER_USER_AGENT constant ===

#[test]
fn test_provider_user_agent_is_browser_like() {
    assert!(PROVIDER_USER_AGENT.contains("Mozilla"));
    assert!(PROVIDER_USER_AGENT.contains("Chrome"));
}
