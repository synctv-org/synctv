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
use synctv_media_providers::*;

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
fn test_is_not_retryable_invalid_header() {
    let err = ProviderClientError::InvalidHeader("bad".to_string());
    assert!(!err.is_retryable());
}

mod ssrf_tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use synctv_common::ssrf::SsrfGuard;

    fn is_ip_blocked(ip: &IpAddr) -> bool {
        SsrfGuard::strict_policy().is_ip_blocked(ip)
    }

    #[test]
    fn test_disabled_policy_does_not_block_ips() {
        assert!(!SsrfGuard::disabled().is_ip_blocked(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(!SsrfGuard::disabled().is_ip_blocked(&IpAddr::V6(Ipv6Addr::LOCALHOST)));
    }

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

mod alist_type_tests {
    use synctv_media_providers::alist::*;

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
    fn test_alist_fs_get_resp_with_related() {
        let json = r#"{
            "name": "video.mp4",
            "size": 1024000,
            "is_dir": false,
            "related": [
                {
                    "name": "subtitle.srt",
                    "size": 5000,
                    "is_dir": false,
                    "raw_url": "https://cdn.example.com/subtitle.srt",
                    "provider": "AliyundriveOpen"
                }
            ]
        }"#;
        let resp: HttpFsGetResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.related.len(), 1);
        assert_eq!(resp.related[0].name, "subtitle.srt");
        assert_eq!(
            resp.related[0].raw_url,
            "https://cdn.example.com/subtitle.srt"
        );
        assert_eq!(resp.related[0].provider, "AliyundriveOpen");
    }

    #[test]
    fn test_alist_fs_other_resp_with_preview() {
        let json = r#"{
            "drive_id": "d1",
            "file_id": "f1",
            "provider": "AliyundriveOpen",
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
        assert_eq!(resp.provider, "AliyundriveOpen");
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

mod emby_type_tests {
    use synctv_media_providers::emby::*;
    use synctv_media_providers::grpc::emby::{
        DirectPlayProfileHint, PlaybackInfoDeviceProfile, SubtitleDeliveryMethod,
        SubtitleProfileHint,
    };

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
    fn test_emby_device_profile_preserves_explicit_empty_subtitle_profiles() {
        let profile = PlaybackInfoDeviceProfile {
            direct_play_profiles: Vec::new(),
            transcoding_container: String::new(),
            transcoding_protocol: String::new(),
            transcoding_video_codec: String::new(),
            transcoding_audio_codec: String::new(),
            subtitle_profiles: Vec::new(),
        };

        let value = device_profile_from_playback_client_profile(Some(&profile));
        let subtitles = value["SubtitleProfiles"].as_array().unwrap();

        assert!(
            subtitles.is_empty(),
            "an explicitly empty subtitle profile means subtitles are unsupported"
        );
    }

    #[test]
    fn test_emby_device_profile_maps_custom_playback_profile() {
        let profile = PlaybackInfoDeviceProfile {
            direct_play_profiles: vec![
                DirectPlayProfileHint {
                    container: "mp4,m4v".to_string(),
                    video_codecs: vec!["h264".to_string(), "hevc".to_string()],
                    audio_codecs: vec!["aac".to_string(), "eac3".to_string()],
                },
                DirectPlayProfileHint {
                    container: "ignored-empty-codecs".to_string(),
                    video_codecs: Vec::new(),
                    audio_codecs: vec!["aac".to_string()],
                },
            ],
            transcoding_container: "mp4".to_string(),
            transcoding_protocol: "dash".to_string(),
            transcoding_video_codec: "hevc".to_string(),
            transcoding_audio_codec: "eac3".to_string(),
            subtitle_profiles: vec![
                SubtitleProfileHint {
                    format: "vtt".to_string(),
                    method: SubtitleDeliveryMethod::Hls as i32,
                },
                SubtitleProfileHint {
                    format: "ass".to_string(),
                    method: SubtitleDeliveryMethod::Embed as i32,
                },
                SubtitleProfileHint {
                    format: "ignored".to_string(),
                    method: SubtitleDeliveryMethod::Unspecified as i32,
                },
            ],
        };

        let value = device_profile_from_playback_client_profile(Some(&profile));

        let direct_play = value["DirectPlayProfiles"].as_array().unwrap();
        assert_eq!(direct_play.len(), 1);
        assert_eq!(direct_play[0]["Container"], "mp4,m4v");
        assert_eq!(direct_play[0]["VideoCodec"], "h264,hevc");
        assert_eq!(direct_play[0]["AudioCodec"], "aac,eac3");
        assert_eq!(direct_play[0]["Type"], "Video");

        let transcoding = value["TranscodingProfiles"].as_array().unwrap();
        assert_eq!(transcoding.len(), 1);
        assert_eq!(transcoding[0]["Container"], "mp4");
        assert_eq!(transcoding[0]["Protocol"], "dash");
        assert_eq!(transcoding[0]["VideoCodec"], "hevc");
        assert_eq!(transcoding[0]["AudioCodec"], "eac3");
        assert_eq!(transcoding[0]["Context"], "Streaming");

        let subtitles = value["SubtitleProfiles"].as_array().unwrap();
        assert_eq!(subtitles.len(), 2);
        assert_eq!(subtitles[0]["Format"], "vtt");
        assert_eq!(subtitles[0]["Method"], "Hls");
        assert_eq!(subtitles[1]["Format"], "ass");
        assert_eq!(subtitles[1]["Method"], "Embed");
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

mod bilibili_type_tests {
    use synctv_media_providers::bilibili::*;

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
}

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

#[test]
fn test_provider_user_agent_is_browser_like() {
    assert!(PROVIDER_USER_AGENT.contains("Mozilla"));
    assert!(PROVIDER_USER_AGENT.contains("Chrome"));
}
