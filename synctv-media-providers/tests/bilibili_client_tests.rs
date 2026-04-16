//! Bilibili client wiremock tests
//!
//! Tests for `parse_video_page`, `get_video_url`, `get_dash_video_url`,
//! and `get_live_streams` using wiremock mocked endpoints.
//!
//! The client supports transport and endpoint injection, so critical HTTP paths
//! can be verified against a local mock server.

#![allow(clippy::unwrap_used)]
use synctv_media_providers::bilibili::client::BilibiliEndpoints;
use synctv_media_providers::BilibiliClient;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// match_url additional coverage

#[test]
fn test_match_url_bvid_with_query_params() {
    let (media_type, id) =
        BilibiliClient::match_url("https://www.bilibili.com/video/BV1xx411c7mD?p=2&vd_source=abc")
            .unwrap();
    assert_eq!(media_type, "video");
    assert_eq!(id, "BV1xx411c7mD");
}

#[test]
fn test_match_url_bvid_mobile() {
    let (media_type, id) =
        BilibiliClient::match_url("https://m.bilibili.com/video/BV1xx411c7mD").unwrap();
    assert_eq!(media_type, "video");
    assert_eq!(id, "BV1xx411c7mD");
}

#[test]
fn test_match_url_live_room_direct() {
    let (media_type, id) =
        BilibiliClient::match_url("https://live.bilibili.com/live/99999").unwrap();
    assert_eq!(media_type, "live");
    assert_eq!(id, "99999");
}

#[test]
fn test_match_url_bangumi_ep_long_id() {
    let (media_type, id) =
        BilibiliClient::match_url("https://www.bilibili.com/bangumi/play/ep999999999").unwrap();
    assert_eq!(media_type, "bangumi");
    assert_eq!(id, "ep999999999");
}

#[test]
fn test_match_url_bangumi_ss_long_id() {
    let (media_type, id) =
        BilibiliClient::match_url("https://www.bilibili.com/bangumi/play/ss888888").unwrap();
    assert_eq!(media_type, "bangumi");
    assert_eq!(id, "ss888888");
}

// extract_bvid / extract_epid / is_short_link

#[test]
fn test_extract_bvid_from_url() {
    assert_eq!(
        BilibiliClient::extract_bvid("https://www.bilibili.com/video/BV1xx411c7mD?p=1"),
        Some("BV1xx411c7mD".to_string())
    );
    assert_eq!(BilibiliClient::extract_bvid("no bvid here"), None);
    assert_eq!(BilibiliClient::extract_bvid(""), None);
}

#[test]
fn test_extract_epid_from_url() {
    assert_eq!(
        BilibiliClient::extract_epid("https://www.bilibili.com/bangumi/play/ep123456"),
        Some("ep123456".to_string())
    );
    assert_eq!(BilibiliClient::extract_epid("no epid"), None);
}

#[test]
fn test_is_short_link_valid() {
    assert!(BilibiliClient::is_short_link("https://b23.tv/abcdef"));
    assert!(BilibiliClient::is_short_link("http://b23.tv/xyz"));
}

#[test]
fn test_is_short_link_subdomain() {
    // Subdomains of b23.tv should also match
    assert!(BilibiliClient::is_short_link("https://video.b23.tv/abc"));
}

#[test]
fn test_is_short_link_false_positives() {
    // These should NOT be treated as short links
    assert!(!BilibiliClient::is_short_link(
        "https://evil.com/b23.tv/fake"
    ));
    assert!(!BilibiliClient::is_short_link(
        "https://b23.tv.evil.com/fake"
    ));
    assert!(!BilibiliClient::is_short_link("not a url at all"));
    assert!(!BilibiliClient::is_short_link(""));
}

// BilibiliClient creation

#[test]
fn test_client_creation_no_cookies() {
    let client = BilibiliClient::new();
    assert!(client.is_ok(), "client construction should succeed");
}

#[test]
fn test_client_creation_with_cookies() {
    let mut cookies = std::collections::HashMap::new();
    cookies.insert("SESSDATA".to_string(), "abc123".to_string());
    cookies.insert("bili_jct".to_string(), "csrf_token".to_string());
    let client = BilibiliClient::with_cookies(cookies);
    assert!(
        client.is_ok(),
        "client construction with cookies should succeed"
    );
}

#[tokio::test]
async fn test_new_qr_code_uses_injected_endpoints() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/passport-login/web/qrcode/generate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 0,
            "message": "0",
            "data": {
                "url": "https://mock.local/qr",
                "qrcode_key": "qr-test-key"
            }
        })))
        .mount(&server)
        .await;

    let client = BilibiliClient::new_with_transport_defaults(
        reqwest::Client::new(),
        BilibiliEndpoints::for_test(server.uri()),
    )
    .unwrap();

    let (url, key) = client.new_qr_code().await.unwrap();
    assert_eq!(url, "https://mock.local/qr");
    assert_eq!(key, "qr-test-key");
}

// Quality type tests

#[test]
fn test_quality_to_qn() {
    use synctv_media_providers::bilibili::types::Quality;
    assert_eq!(Quality::P1080.to_qn(), 80);
    assert_eq!(Quality::P720.to_qn(), 64);
    assert_eq!(Quality::P480.to_qn(), 32);
    assert_eq!(Quality::P360.to_qn(), 16);
}

#[test]
fn test_quality_from_qn() {
    use synctv_media_providers::bilibili::types::Quality;
    assert_eq!(Quality::from_qn(80), Quality::P1080);
    assert_eq!(Quality::from_qn(64), Quality::P720);
    assert_eq!(Quality::from_qn(32), Quality::P480);
    assert_eq!(Quality::from_qn(16), Quality::P360);
    // Unknown quality should default to P360
    assert_eq!(Quality::from_qn(999), Quality::P360);
    assert_eq!(Quality::from_qn(0), Quality::P360);
}

#[test]
fn test_quality_as_str() {
    use synctv_media_providers::bilibili::types::Quality;
    assert_eq!(Quality::P1080.as_str(), "1080P");
    assert_eq!(Quality::P720.as_str(), "720P");
    assert_eq!(Quality::P480.as_str(), "480P");
    assert_eq!(Quality::P360.as_str(), "360P");
}

// VideoId / EpisodeId types

#[test]
fn test_video_id_bvid() {
    use synctv_media_providers::bilibili::types::VideoId;
    let vid = VideoId::Bvid("BV1xx411c7mD".to_string());
    assert_eq!(vid, VideoId::Bvid("BV1xx411c7mD".to_string()));
}

#[test]
fn test_video_id_aid() {
    use synctv_media_providers::bilibili::types::VideoId;
    let vid = VideoId::Aid(170_001);
    assert_eq!(vid, VideoId::Aid(170_001));
}

#[test]
fn test_episode_id() {
    use synctv_media_providers::bilibili::types::EpisodeId;
    let eid = EpisodeId("ep123456".to_string());
    assert_eq!(eid.0, "ep123456");
}

// Type deserialization tests (verify JSON response shapes)

#[test]
fn test_video_page_info_resp_deserialize() {
    use synctv_media_providers::bilibili::types::VideoPageInfoResp;
    let json = serde_json::json!({
        "code": 0,
        "message": "0",
        "ttl": 1,
        "data": {
            "title": "Test Video",
            "pic": "https://i0.hdslb.com/bfs/archive/test.jpg",
            "bvid": "BV1xx411c7mD",
            "aid": 170_001,
            "cid": 279_786,
            "owner": {
                "name": "TestUser",
                "face": "https://i0.hdslb.com/bfs/face/test.jpg",
                "mid": 12345
            },
            "pages": [
                {
                    "cid": 279_786,
                    "page": 1,
                    "part": "Part 1",
                    "duration": 300,
                    "dimension": {"width": 1920, "height": 1080, "rotate": 0},
                    "first_frame": ""
                }
            ]
        }
    });
    let resp: VideoPageInfoResp = serde_json::from_value(json).unwrap();
    assert_eq!(resp.code, 0);
    assert_eq!(resp.data.title, "Test Video");
    assert_eq!(resp.data.bvid, "BV1xx411c7mD");
    assert_eq!(resp.data.aid, 170_001);
    assert_eq!(resp.data.cid, 279_786);
    assert_eq!(resp.data.owner.name, "TestUser");
    assert_eq!(resp.data.pages.len(), 1);
    assert_eq!(resp.data.pages[0].part, "Part 1");
    assert_eq!(resp.data.pages[0].duration, 300);
}

#[test]
fn test_video_url_resp_deserialize() {
    use synctv_media_providers::bilibili::types::VideoUrlResp;
    let json = serde_json::json!({
        "code": 0,
        "message": "0",
        "ttl": 1,
        "data": {
            "accept_quality": [80, 64, 32, 16],
            "accept_description": ["1080P", "720P", "480P", "360P"],
            "quality": 80,
            "durl": [
                {
                    "url": "https://cdn.bilibili.com/video/test.flv",
                    "size": 12_345_678,
                    "length": 300_000
                }
            ]
        }
    });
    let resp: VideoUrlResp = serde_json::from_value(json).unwrap();
    assert_eq!(resp.code, 0);
    assert_eq!(resp.data.accept_quality, vec![80, 64, 32, 16]);
    assert_eq!(resp.data.quality, 80);
    assert_eq!(resp.data.durl.len(), 1);
    assert_eq!(resp.data.durl[0].size, 12_345_678);
}

#[test]
fn test_dash_video_resp_deserialize() {
    use synctv_media_providers::bilibili::types::DashVideoResp;
    let json = serde_json::json!({
        "code": 0,
        "message": "0",
        "ttl": 1,
        "data": {
            "dash": {
                "duration": 300.0,
                "minBufferTime": 1.5,
                "video": [
                    {
                        "id": 80,
                        "baseUrl": "https://cdn.bilibili.com/video/test_video.m4s",
                        "backupUrl": [],
                        "mimeType": "video/mp4",
                        "codecs": "avc1.640028",
                        "width": 1920,
                        "height": 1080,
                        "frameRate": "30",
                        "bandwidth": 2_000_000,
                        "sar": "1:1",
                        "startWithSap": 1,
                        "SegmentBase": {
                            "Initialization": "0-1000",
                            "indexRange": "1001-2000"
                        }
                    }
                ],
                "audio": [
                    {
                        "id": 30280,
                        "baseUrl": "https://cdn.bilibili.com/audio/test_audio.m4s",
                        "backupUrl": [],
                        "mimeType": "audio/mp4",
                        "codecs": "mp4a.40.2",
                        "bandwidth": 128_000,
                        "startWithSap": 1,
                        "SegmentBase": {
                            "Initialization": "0-500",
                            "indexRange": "501-1000"
                        }
                    }
                ]
            },
            "support_formats": [
                {"quality": 80, "new_description": "1080P 高清"}
            ]
        }
    });
    let resp: DashVideoResp = serde_json::from_value(json).unwrap();
    assert_eq!(resp.code, 0);
    assert!((resp.data.dash.duration - 300.0).abs() < f64::EPSILON);
    assert_eq!(resp.data.dash.video.len(), 1);
    assert_eq!(resp.data.dash.video[0].width, 1920);
    assert_eq!(resp.data.dash.video[0].height, 1080);
    assert_eq!(resp.data.dash.audio.len(), 1);
    assert_eq!(resp.data.dash.audio[0].codecs, "mp4a.40.2");
    assert_eq!(resp.data.support_formats.len(), 1);
    assert_eq!(resp.data.support_formats[0].quality, 80);
}

#[test]
fn test_live_room_play_info_resp_deserialize() {
    use synctv_media_providers::bilibili::types::RoomPlayInfoResp;
    let json = serde_json::json!({
        "code": 0,
        "message": "0",
        "data": {
            "playurl_info": {
                "playurl": {
                    "stream": [
                        {
                            "protocol_name": "http_stream",
                            "format": [
                                {
                                    "codec": [
                                        {
                                            "current_qn": 10000,
                                            "accept_qn": [10000, 400, 150],
                                            "base_url": "/live-bvc/test.flv",
                                            "url_info": [
                                                {
                                                    "host": "https://d1--cn-gotcha04.bilivideo.com",
                                                    "extra": "?expires=123&sign=abc"
                                                }
                                            ]
                                        }
                                    ]
                                }
                            ]
                        }
                    ]
                }
            }
        }
    });
    let resp: RoomPlayInfoResp = serde_json::from_value(json).unwrap();
    assert_eq!(resp.code, 0);
    let playurl_info = resp.data.playurl_info.unwrap();
    let playurl = playurl_info.playurl.unwrap();
    assert_eq!(playurl.stream.len(), 1);
    assert_eq!(playurl.stream[0].protocol_name, "http_stream");
    assert_eq!(playurl.stream[0].format[0].codec[0].current_qn, 10000);
    assert_eq!(
        playurl.stream[0].format[0].codec[0].url_info[0].host,
        "https://d1--cn-gotcha04.bilivideo.com"
    );
}

#[test]
fn test_season_info_resp_deserialize() {
    use synctv_media_providers::bilibili::types::SeasonInfoResp;
    let json = serde_json::json!({
        "code": 0,
        "message": "success",
        "result": {
            "title": "Test Anime",
            "cover": "https://i0.hdslb.com/bfs/bangumi/cover.jpg",
            "actors": "Actor1, Actor2",
            "episodes": [
                {
                    "title": "Episode 1",
                    "long_title": "The Beginning",
                    "bvid": "BV1test123",
                    "cid": 100_001,
                    "ep_id": 200_001,
                    "aid": 300_001,
                    "cover": "https://i0.hdslb.com/bfs/archive/ep1.jpg",
                    "duration": 1_440_000
                }
            ]
        }
    });
    let resp: SeasonInfoResp = serde_json::from_value(json).unwrap();
    assert_eq!(resp.code, 0);
    assert_eq!(resp.result.title, "Test Anime");
    assert_eq!(resp.result.episodes.len(), 1);
    assert_eq!(resp.result.episodes[0].ep_id, 200_001);
    assert_eq!(resp.result.episodes[0].long_title, "The Beginning");
}

#[test]
fn test_parse_live_page_resp_deserialize() {
    use synctv_media_providers::bilibili::types::ParseLivePageResp;
    let json = serde_json::json!({
        "code": 0,
        "message": "0",
        "data": {
            "title": "My Stream",
            "user_cover": "https://i0.hdslb.com/bfs/live/cover.jpg",
            "uid": 12345,
            "room_id": 67890,
            "live_status": 1
        }
    });
    let resp: ParseLivePageResp = serde_json::from_value(json).unwrap();
    assert_eq!(resp.code, 0);
    assert_eq!(resp.data.title, "My Stream");
    assert_eq!(resp.data.uid, 12345);
    assert_eq!(resp.data.room_id, 67890);
    assert_eq!(resp.data.live_status, 1);
}

#[test]
fn test_nav_resp_deserialize() {
    use synctv_media_providers::bilibili::types::NavResp;
    let json = serde_json::json!({
        "code": 0,
        "message": "0",
        "ttl": 1,
        "data": {
            "isLogin": true,
            "uname": "TestUser",
            "face": "https://i0.hdslb.com/bfs/face/test.jpg",
            "vipStatus": 1,
            "mid": 12345,
            "wbi_img": {
                "img_url": "https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png",
                "sub_url": "https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"
            }
        }
    });
    let resp: NavResp = serde_json::from_value(json).unwrap();
    assert_eq!(resp.code, 0);
    assert!(resp.data.is_login);
    assert_eq!(resp.data.uname, "TestUser");
    assert_eq!(resp.data.vip_status, 1);
    assert!(resp.data.wbi_img.is_some());
}

#[test]
fn test_nav_resp_deserialize_without_wbi_img() {
    use synctv_media_providers::bilibili::types::NavResp;
    let json = serde_json::json!({
        "code": 0,
        "message": "0",
        "ttl": 1,
        "data": {
            "isLogin": false,
            "uname": "",
            "face": "",
            "vipStatus": 0,
            "mid": 0
        }
    });
    let resp: NavResp = serde_json::from_value(json).unwrap();
    assert_eq!(resp.code, 0);
    assert!(!resp.data.is_login);
    assert!(resp.data.wbi_img.is_none());
}

#[test]
fn test_video_page_info_with_ugc_season() {
    use synctv_media_providers::bilibili::types::VideoPageInfoResp;
    let json = serde_json::json!({
        "code": 0,
        "message": "0",
        "ttl": 1,
        "data": {
            "title": "Series Video",
            "pic": "https://i0.hdslb.com/bfs/archive/test.jpg",
            "bvid": "BV1series",
            "aid": 170_002,
            "cid": 279_787,
            "owner": {
                "name": "SeriesCreator",
                "face": "https://i0.hdslb.com/bfs/face/test.jpg",
                "mid": 12345
            },
            "pages": [
                {
                    "cid": 279_787,
                    "page": 1,
                    "part": "Part 1",
                    "duration": 600,
                    "dimension": {"width": 1920, "height": 1080, "rotate": 0}
                }
            ],
            "ugc_season": {
                "title": "My UGC Season",
                "cover": "https://i0.hdslb.com/bfs/archive/season.jpg",
                "sections": [
                    {
                        "title": "Section 1",
                        "episodes": [
                            {
                                "title": "Ep 1",
                                "bvid": "BV1ep1",
                                "cid": 100,
                                "aid": 200,
                                "page": {
                                    "cid": 100,
                                    "part": "Episode 1",
                                    "duration": 300
                                }
                            }
                        ]
                    }
                ]
            }
        }
    });
    let resp: VideoPageInfoResp = serde_json::from_value(json).unwrap();
    assert!(resp.data.ugc_season.is_some());
    let ugc = resp.data.ugc_season.unwrap();
    assert_eq!(ugc.title, "My UGC Season");
    assert_eq!(ugc.sections.len(), 1);
    assert_eq!(ugc.sections[0].episodes.len(), 1);
    assert_eq!(ugc.sections[0].episodes[0].bvid, "BV1ep1");
}

#[test]
fn test_live_danmu_info_resp_deserialize() {
    use synctv_media_providers::bilibili::types::GetLiveDanmuInfoResp;
    let json = serde_json::json!({
        "code": 0,
        "message": "0",
        "data": {
            "token": "danmu_token_123",
            "host_list": [
                {
                    "host": "broadcastlv.chat.bilibili.com",
                    "port": 2243,
                    "ws_port": 2244,
                    "wss_port": 443
                }
            ]
        }
    });
    let resp: GetLiveDanmuInfoResp = serde_json::from_value(json).unwrap();
    assert_eq!(resp.code, 0);
    assert_eq!(resp.data.token, "danmu_token_123");
    assert_eq!(resp.data.host_list.len(), 1);
    assert_eq!(resp.data.host_list[0].wss_port, 443);
}

// Error response deserialization

#[test]
fn test_video_page_info_error_code() {
    use synctv_media_providers::bilibili::types::VideoPageInfoResp;
    let json = serde_json::json!({
        "code": -404,
        "message": "Video not found",
        "ttl": 1,
        "data": {
            "title": "",
            "pic": "",
            "bvid": "",
            "aid": 0,
            "cid": 0,
            "owner": {"name": "", "face": "", "mid": 0},
            "pages": []
        }
    });
    let resp: VideoPageInfoResp = serde_json::from_value(json).unwrap();
    assert_eq!(resp.code, -404);
    assert_eq!(resp.message, "Video not found");
}

#[test]
fn test_dash_pgc_resp_deserialize() {
    use synctv_media_providers::bilibili::types::DashPgcResp;
    let json = serde_json::json!({
        "code": 0,
        "message": "success",
        "result": {
            "dash": {
                "duration": 1440.0,
                "minBufferTime": 1.5,
                "video": [],
                "audio": []
            },
            "support_formats": []
        }
    });
    let resp: DashPgcResp = serde_json::from_value(json).unwrap();
    assert_eq!(resp.code, 0);
    assert!((resp.result.dash.duration - 1440.0).abs() < f64::EPSILON);
}

#[test]
fn test_pgc_url_resp_deserialize() {
    use synctv_media_providers::bilibili::types::PgcUrlResp;
    let json = serde_json::json!({
        "code": 0,
        "message": "success",
        "result": {
            "accept_quality": [80, 64],
            "accept_description": ["1080P", "720P"],
            "quality": 80,
            "durl": [
                {
                    "url": "https://cdn.bilibili.com/pgc/test.flv",
                    "size": 50_000_000,
                    "length": 1_440_000
                }
            ]
        }
    });
    let resp: PgcUrlResp = serde_json::from_value(json).unwrap();
    assert_eq!(resp.code, 0);
    assert_eq!(resp.result.quality, 80);
    assert_eq!(resp.result.durl.len(), 1);
}

// Danmaku heartbeat packet tests

/// Test that build_heartbeat_packet produces correct binary format
/// Header format: packet_length (4) + header_length (2) + version (2) + operation (4) + sequence (4)
#[test]
fn test_heartbeat_packet_format() {
    use synctv_media_providers::bilibili::client::build_heartbeat_packet;

    let packet = build_heartbeat_packet();

    // Heartbeat packet is exactly 16 bytes (header only, no body)
    assert_eq!(packet.len(), 16);

    // Packet length (big-endian u32) = 16
    assert_eq!(&packet[0..4], &[0, 0, 0, 16]);

    // Header length (big-endian u16) = 16
    assert_eq!(&packet[4..6], &[0, 16]);

    // Protocol version (big-endian u16) = 1
    assert_eq!(&packet[6..8], &[0, 1]);

    // Operation (big-endian u32) = 2 (heartbeat)
    assert_eq!(&packet[8..12], &[0, 0, 0, 2]);

    // Sequence (big-endian u32) = 1
    assert_eq!(&packet[12..16], &[0, 0, 0, 1]);
}

/// Test that build_auth_packet produces correct format
#[test]
fn test_auth_packet_format() {
    use synctv_media_providers::bilibili::client::build_auth_packet;

    let packet = build_auth_packet(12345, "test_token");

    // Minimum size is 16 byte header + some JSON body
    assert!(packet.len() > 16);

    // Header length (big-endian u16) = 16
    assert_eq!(&packet[4..6], &[0, 16]);

    // Protocol version (big-endian u16) = 1
    assert_eq!(&packet[6..8], &[0, 1]);

    // Operation (big-endian u32) = 7 (auth)
    assert_eq!(&packet[8..12], &[0, 0, 0, 7]);

    // Sequence (big-endian u32) = 1
    assert_eq!(&packet[12..16], &[0, 0, 0, 1]);

    // Body should contain JSON with roomid
    let body = &packet[16..];
    let json: serde_json::Value = serde_json::from_slice(body).unwrap();
    assert_eq!(json["roomid"], 12345);
    assert_eq!(json["key"], "test_token");
}

// DanmakuMessage tests

#[test]
fn test_danmaku_message_debug() {
    use synctv_media_providers::bilibili::DanmakuMessage;

    let chat = DanmakuMessage::Chat {
        user: "test_user".to_string(),
        message: "hello".to_string(),
        timestamp: 12345,
    };
    let debug_str = format!("{chat:?}");
    assert!(debug_str.contains("test_user"));
    assert!(debug_str.contains("hello"));
}

#[test]
fn test_danmaku_message_heartbeat() {
    use synctv_media_providers::bilibili::DanmakuMessage;

    let heartbeat = DanmakuMessage::Heartbeat { online_count: 1000 };
    if let DanmakuMessage::Heartbeat { online_count } = heartbeat {
        assert_eq!(online_count, 1000);
    } else {
        panic!("Expected Heartbeat variant");
    }
}

// HeartbeatConfig tests

#[test]
fn test_heartbeat_config_default() {
    use std::time::Duration;
    use synctv_media_providers::bilibili::HeartbeatConfig;

    let config = HeartbeatConfig::default();
    assert_eq!(config.interval, Duration::from_secs(30));
}

#[test]
fn test_heartbeat_config_custom() {
    use std::time::Duration;
    use synctv_media_providers::bilibili::HeartbeatConfig;

    let config = HeartbeatConfig {
        interval: Duration::from_secs(10),
    };
    assert_eq!(config.interval, Duration::from_secs(10));
}

// ReconnectConfig tests

#[test]
fn test_reconnect_config_default() {
    use std::time::Duration;
    use synctv_media_providers::bilibili::ReconnectConfig;

    let config = ReconnectConfig::default();
    assert_eq!(config.max_retries, 5);
    assert_eq!(config.initial_delay, Duration::from_secs(1));
    assert_eq!(config.max_delay, Duration::from_secs(30));
    assert!((config.backoff_multiplier - 2.0).abs() < f64::EPSILON);
}

#[test]
fn test_reconnect_config_custom() {
    use std::time::Duration;
    use synctv_media_providers::bilibili::ReconnectConfig;

    let config = ReconnectConfig {
        max_retries: 10,
        initial_delay: Duration::from_millis(500),
        max_delay: Duration::from_mins(1),
        backoff_multiplier: 1.5,
    };
    assert_eq!(config.max_retries, 10);
    assert_eq!(config.initial_delay, Duration::from_millis(500));
    assert_eq!(config.max_delay, Duration::from_mins(1));
    assert!((config.backoff_multiplier - 1.5).abs() < f64::EPSILON);
}

#[test]
fn test_reconnect_config_delay_calculation() {
    use std::time::Duration;
    use synctv_media_providers::bilibili::ReconnectConfig;

    let config = ReconnectConfig {
        max_retries: 5,
        initial_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(30),
        backoff_multiplier: 2.0,
    };

    // First retry (retry_count = 0): 1 * 2^0 = 1s
    let delay0 = config.delay_for_retry(0);
    assert_eq!(delay0, Duration::from_secs(1));

    // Second retry (retry_count = 1): 1 * 2^1 = 2s
    let delay1 = config.delay_for_retry(1);
    assert_eq!(delay1, Duration::from_secs(2));

    // Third retry (retry_count = 2): 1 * 2^2 = 4s
    let delay2 = config.delay_for_retry(2);
    assert_eq!(delay2, Duration::from_secs(4));

    // Fourth retry (retry_count = 3): 1 * 2^3 = 8s
    let delay3 = config.delay_for_retry(3);
    assert_eq!(delay3, Duration::from_secs(8));

    // Fifth retry (retry_count = 4): 1 * 2^4 = 16s
    let delay4 = config.delay_for_retry(4);
    assert_eq!(delay4, Duration::from_secs(16));

    // Sixth retry (retry_count = 5): 1 * 2^5 = 32s -> capped at 30s
    let delay5 = config.delay_for_retry(5);
    assert_eq!(delay5, Duration::from_secs(30));
}

#[test]
fn test_reconnect_config_delay_never_exceeds_max() {
    use std::time::Duration;
    use synctv_media_providers::bilibili::ReconnectConfig;

    let config = ReconnectConfig {
        max_retries: 100,
        initial_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(10),
        backoff_multiplier: 2.0,
    };

    // Even with large retry counts, delay should be capped
    for i in 0..50 {
        let delay = config.delay_for_retry(i);
        assert!(
            delay <= Duration::from_secs(10),
            "Delay {delay:?} exceeds max for retry {i}"
        );
    }
}

// ReconnectResult tests

#[test]
fn test_reconnect_result_messages() {
    use synctv_media_providers::bilibili::{DanmakuMessage, ReconnectResult};

    let messages = vec![DanmakuMessage::Chat {
        user: "test".to_string(),
        message: "hello".to_string(),
        timestamp: 12345,
    }];

    let result = ReconnectResult::Messages(messages);
    if let ReconnectResult::Messages(msgs) = result {
        assert_eq!(msgs.len(), 1);
    } else {
        panic!("Expected Messages variant");
    }
}

#[test]
fn test_reconnect_result_reconnected() {
    use synctv_media_providers::bilibili::ReconnectResult;

    let result = ReconnectResult::Reconnected { attempts: 3 };
    if let ReconnectResult::Reconnected { attempts } = result {
        assert_eq!(attempts, 3);
    } else {
        panic!("Expected Reconnected variant");
    }
}

#[test]
fn test_reconnect_result_failed() {
    use synctv_media_providers::bilibili::{BilibiliError, ReconnectResult};

    let result = ReconnectResult::Failed {
        attempts: 5,
        error: BilibiliError::Parse("test error".to_string()),
    };

    if let ReconnectResult::Failed { attempts, error: e } = result {
        assert_eq!(attempts, 5);
        match e {
            BilibiliError::Parse(msg) => assert_eq!(msg, "test error"),
            _ => panic!("Expected Parse error"),
        }
    } else {
        panic!("Expected Failed variant");
    }
}

#[test]
fn test_reconnect_result_debug() {
    use synctv_media_providers::bilibili::{DanmakuMessage, ReconnectResult};

    let messages = vec![DanmakuMessage::Heartbeat { online_count: 100 }];
    let result = ReconnectResult::Messages(messages);

    let debug_str = format!("{result:?}");
    assert!(debug_str.contains("Messages"));
}
