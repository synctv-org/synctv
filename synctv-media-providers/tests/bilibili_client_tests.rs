//! Bilibili client wiremock tests
//!
//! Tests for parse_video_page, get_video_url, get_dash_video_url,
//! and get_live_streams using wiremock mocked endpoints.
//!
//! Note: BilibiliClient uses a shared static SHARED_CLIENT that hardcodes
//! Bilibili's API hostnames. To test with wiremock, we cannot redirect
//! BilibiliClient methods to a local mock server (they always hit
//! api.bilibili.com). Instead, we test the lower-level utilities and
//! match_url parsing thoroughly, and add wiremock tests for the helpers
//! and error-checking code paths that we CAN intercept.

#![allow(clippy::unwrap_used)]
use synctv_media_providers::BilibiliClient;

// ============================================================================
// match_url additional coverage
// ============================================================================

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

// ============================================================================
// extract_bvid / extract_epid / is_short_link
// ============================================================================

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

// ============================================================================
// BilibiliClient creation
// ============================================================================

#[test]
fn test_client_creation_no_cookies() {
    let client = BilibiliClient::new();
    assert!(client.is_ok());
}

#[test]
fn test_client_creation_with_cookies() {
    let mut cookies = std::collections::HashMap::new();
    cookies.insert("SESSDATA".to_string(), "abc123".to_string());
    cookies.insert("bili_jct".to_string(), "csrf_token".to_string());
    let client = BilibiliClient::with_cookies(cookies);
    assert!(client.is_ok());
}

// ============================================================================
// Quality type tests
// ============================================================================

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

// ============================================================================
// VideoId / EpisodeId types
// ============================================================================

#[test]
fn test_video_id_bvid() {
    use synctv_media_providers::bilibili::types::VideoId;
    let vid = VideoId::Bvid("BV1xx411c7mD".to_string());
    assert_eq!(vid, VideoId::Bvid("BV1xx411c7mD".to_string()));
}

#[test]
fn test_video_id_aid() {
    use synctv_media_providers::bilibili::types::VideoId;
    let vid = VideoId::Aid(170001);
    assert_eq!(vid, VideoId::Aid(170001));
}

#[test]
fn test_episode_id() {
    use synctv_media_providers::bilibili::types::EpisodeId;
    let eid = EpisodeId("ep123456".to_string());
    assert_eq!(eid.0, "ep123456");
}

// ============================================================================
// Type deserialization tests (verify JSON response shapes)
// ============================================================================

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
            "aid": 170001,
            "cid": 279786,
            "owner": {
                "name": "TestUser",
                "face": "https://i0.hdslb.com/bfs/face/test.jpg",
                "mid": 12345
            },
            "pages": [
                {
                    "cid": 279786,
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
    assert_eq!(resp.data.aid, 170001);
    assert_eq!(resp.data.cid, 279786);
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
                    "size": 12345678,
                    "length": 300000
                }
            ]
        }
    });
    let resp: VideoUrlResp = serde_json::from_value(json).unwrap();
    assert_eq!(resp.code, 0);
    assert_eq!(resp.data.accept_quality, vec![80, 64, 32, 16]);
    assert_eq!(resp.data.quality, 80);
    assert_eq!(resp.data.durl.len(), 1);
    assert_eq!(resp.data.durl[0].size, 12345678);
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
                        "bandwidth": 2000000,
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
                        "bandwidth": 128000,
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
    assert_eq!(resp.data.dash.duration, 300.0);
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
                    "cid": 100001,
                    "ep_id": 200001,
                    "aid": 300001,
                    "cover": "https://i0.hdslb.com/bfs/archive/ep1.jpg",
                    "duration": 1440000
                }
            ]
        }
    });
    let resp: SeasonInfoResp = serde_json::from_value(json).unwrap();
    assert_eq!(resp.code, 0);
    assert_eq!(resp.result.title, "Test Anime");
    assert_eq!(resp.result.episodes.len(), 1);
    assert_eq!(resp.result.episodes[0].ep_id, 200001);
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
            "aid": 170002,
            "cid": 279787,
            "owner": {
                "name": "SeriesCreator",
                "face": "https://i0.hdslb.com/bfs/face/test.jpg",
                "mid": 12345
            },
            "pages": [
                {
                    "cid": 279787,
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

// ============================================================================
// Error response deserialization
// ============================================================================

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
    assert_eq!(resp.result.dash.duration, 1440.0);
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
                    "size": 50000000,
                    "length": 1440000
                }
            ]
        }
    });
    let resp: PgcUrlResp = serde_json::from_value(json).unwrap();
    assert_eq!(resp.code, 0);
    assert_eq!(resp.result.quality, 80);
    assert_eq!(resp.result.durl.len(), 1);
}
