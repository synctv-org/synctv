use super::*;
use crate::bilibili::Quality;
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn missing(message: &'static str) -> Box<dyn std::error::Error + Send + Sync> {
    anyhow::anyhow!(message).into()
}

fn signed_value<'a>(
    signed: &'a [(String, String)],
    key: &'static str,
) -> std::result::Result<&'a str, Box<dyn std::error::Error + Send + Sync>> {
    signed
        .iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.as_str())
        .ok_or_else(|| missing(key))
}

fn test_endpoints(base_url: impl AsRef<str>) -> BilibiliEndpoints {
    let base = base_url.as_ref().trim_end_matches('/').to_string();
    BilibiliEndpoints {
        web_base: base.clone(),
        api_base: base.clone(),
        passport_base: base.clone(),
        live_api_base: base,
    }
}

fn test_http_client() -> reqwest::Client {
    crate::install_process_crypto_provider();
    reqwest::Client::new()
}

#[test]
fn live_stream_expiry_uses_the_explicit_expires_query_parameter() {
    assert_eq!(
        parse_live_stream_expires_at(
            "https://cdn.example/live.m3u8?token=abc&expires=1785945600&deadline=1",
        ),
        Some(1_785_945_600)
    );
    assert_eq!(
        parse_live_stream_expires_at(
            "https://cdn.example/live.m3u8?deadline=1785945600&wsTime=6a123456",
        ),
        None
    );
    assert_eq!(
        parse_live_stream_expires_at("https://cdn.example/live.m3u8?expires=invalid"),
        None
    );
    assert_eq!(
        parse_live_stream_expires_at("https://cdn.example/live.m3u8?expires=0"),
        None
    );
    assert_eq!(parse_live_stream_expires_at("not a URL"), None);
}

fn nav_response_with_wbi_keys(img_key: &str, sub_key: &str) -> serde_json::Value {
    json!({
        "data": {
            "isLogin": false,
            "wbi_img": {
                "img_url": format!("https://i0.hdslb.com/bfs/wbi/{img_key}.png"),
                "sub_url": format!("https://i0.hdslb.com/bfs/wbi/{sub_key}.png")
            }
        },
        "message": "0",
        "code": 0,
        "ttl": 1
    })
}

#[test]
fn test_extract_bvid_various_formats() {
    // Standard video URL
    assert_eq!(
        BilibiliClient::extract_bvid("https://www.bilibili.com/video/BV1xx411c7XZ"),
        Some("BV1xx411c7XZ".to_string())
    );
    // With query params
    assert_eq!(
        BilibiliClient::extract_bvid("https://www.bilibili.com/video/BV1xx411c7XZ?p=2"),
        Some("BV1xx411c7XZ".to_string())
    );
    // Mobile URL
    assert_eq!(
        BilibiliClient::extract_bvid("https://m.bilibili.com/video/BV1xx411c7XZ"),
        Some("BV1xx411c7XZ".to_string())
    );
    // Just the BV id
    assert_eq!(
        BilibiliClient::extract_bvid("BV1xx411c7XZ"),
        Some("BV1xx411c7XZ".to_string())
    );
}

#[test]
fn test_extract_bvid_invalid() {
    assert_eq!(
        BilibiliClient::extract_bvid("https://www.bilibili.com/video/av12345"),
        None
    );
    assert_eq!(BilibiliClient::extract_bvid("not-a-url"), None);
    assert_eq!(BilibiliClient::extract_bvid(""), None);
}

#[test]
fn test_extract_epid_various_formats() {
    assert_eq!(
        BilibiliClient::extract_epid("https://www.bilibili.com/bangumi/play/ep12345"),
        Some("ep12345".to_string())
    );
    assert_eq!(
        BilibiliClient::extract_epid("https://www.bilibili.com/bangumi/play/ep99999?from=search"),
        Some("ep99999".to_string())
    );
}

#[test]
fn test_extract_epid_invalid() {
    assert_eq!(
        BilibiliClient::extract_epid("https://www.bilibili.com/video/BV123"),
        None
    );
    assert_eq!(BilibiliClient::extract_epid(""), None);
}

#[test]
fn test_is_short_link_variations() {
    assert!(BilibiliClient::is_short_link("https://b23.tv/abc123"));
    assert!(BilibiliClient::is_short_link("http://b23.tv/xyz"));
    assert!(BilibiliClient::is_short_link(
        "https://b23.tv/episode/12345"
    ));
    assert!(!BilibiliClient::is_short_link(
        "https://www.bilibili.com/video/BV123"
    ));
    assert!(!BilibiliClient::is_short_link(""));
    // These must NOT match: "b23.tv" appearing in path or as subdomain of another host
    assert!(!BilibiliClient::is_short_link(
        "https://evil.com/b23.tv/abc"
    ));
    assert!(!BilibiliClient::is_short_link(
        "https://b23.tv.evil.com/abc"
    ));
}

#[test]
fn test_match_url_video() -> TestResult {
    let matched = BilibiliClient::match_url("https://www.bilibili.com/video/BV1xx411c7XZ")?;
    assert!(matches!(
        matched.resource,
        BilibiliResource::Video { ref bvid, .. } if bvid == "BV1xx411c7XZ"
    ));
    Ok(())
}

#[test]
fn test_match_url_bangumi_ep() -> TestResult {
    let matched = BilibiliClient::match_url("https://www.bilibili.com/bangumi/play/ep12345")?;
    assert_eq!(
        matched.resource,
        BilibiliResource::PgcEpisode { episode_id: 12345 }
    );
    Ok(())
}

#[test]
fn test_match_url_bangumi_ss() -> TestResult {
    let matched = BilibiliClient::match_url("https://www.bilibili.com/bangumi/play/ss67890")?;
    assert_eq!(
        matched.resource,
        BilibiliResource::PgcSeason { season_id: 67890 }
    );
    Ok(())
}

#[test]
fn test_match_url_live() -> TestResult {
    let matched = BilibiliClient::match_url("https://live.bilibili.com/live/12345")?;
    assert_eq!(matched.resource, BilibiliResource::Live { room_id: 12345 });

    let matched = BilibiliClient::match_url("https://live.bilibili.com/76?live_from=85002")?;
    assert_eq!(matched.resource, BilibiliResource::Live { room_id: 76 });

    let matched = BilibiliClient::match_url("https://live.bilibili.com/21452505#main")?;
    assert_eq!(
        matched.resource,
        BilibiliResource::Live {
            room_id: 21_452_505
        }
    );

    let matched = BilibiliClient::match_url("https://live.bilibili.com/")?;
    assert_eq!(matched.resource, BilibiliResource::LiveRecommended);

    let matched = BilibiliClient::match_url(
        "https://live.bilibili.com/p/eden/area-tags?parentAreaId=2&areaId=86",
    )?;
    assert_eq!(
        matched.resource,
        BilibiliResource::LiveArea {
            parent_area_id: 2,
            area_id: 86,
        }
    );
    Ok(())
}

#[test]
fn test_match_url_unknown() {
    let result = BilibiliClient::match_url("https://example.com/unknown");
    assert!(result.is_err());
}

#[test]
fn test_quality_all_variants() {
    assert_eq!(Quality::P1080.to_qn(), 80);
    assert_eq!(Quality::P720.to_qn(), 64);
    assert_eq!(Quality::P480.to_qn(), 32);
    assert_eq!(Quality::P360.to_qn(), 16);
}

#[test]
fn test_quality_from_qn_all() {
    assert_eq!(Quality::from_qn(80), Quality::P1080);
    assert_eq!(Quality::from_qn(64), Quality::P720);
    assert_eq!(Quality::from_qn(32), Quality::P480);
    assert_eq!(Quality::from_qn(16), Quality::P360);
}

#[test]
fn test_quality_from_qn_unknown_defaults() {
    assert_eq!(Quality::from_qn(0), Quality::P360);
    assert_eq!(Quality::from_qn(999), Quality::P360);
}

#[test]
fn test_quality_as_str_all() {
    assert_eq!(Quality::P1080.as_str(), "1080P");
    assert_eq!(Quality::P720.as_str(), "720P");
    assert_eq!(Quality::P480.as_str(), "480P");
    assert_eq!(Quality::P360.as_str(), "360P");
}

#[test]
fn test_quality_roundtrip() {
    for q in [Quality::P1080, Quality::P720, Quality::P480, Quality::P360] {
        assert_eq!(Quality::from_qn(q.to_qn()), q);
    }
}

#[test]
fn test_client_creation_no_cookies() -> TestResult {
    let client = BilibiliClient::new()?;
    assert!(client.cookies.is_none());
    Ok(())
}

#[test]
fn test_client_creation_with_cookies() -> TestResult {
    let mut cookies = HashMap::new();
    cookies.insert("SESSDATA".to_string(), "abc123".to_string());
    let client = BilibiliClient::with_cookies(cookies.clone())?;
    assert!(client.cookies.is_some());
    assert_eq!(
        client
            .cookies
            .as_ref()
            .ok_or_else(|| missing("client cookies should be present"))?
            .get("SESSDATA"),
        Some(&"abc123".to_string())
    );
    Ok(())
}

#[test]
fn test_live_danmaku_websocket_config_limits_incoming_sizes() {
    let config = live_danmaku_websocket_config();
    assert_eq!(
        config.max_message_size,
        Some(LIVE_DANMAKU_WS_MAX_MESSAGE_SIZE)
    );
    assert_eq!(config.max_frame_size, Some(LIVE_DANMAKU_WS_MAX_FRAME_SIZE));
}

#[test]
fn test_video_page_info_deserialize() -> TestResult {
    let json = r#"{
            "data": {
                "title": "Test Video",
                "pic": "https://example.com/pic.jpg",
                "bvid": "BV1xx411c7XZ",
                "aid": 12345,
                "cid": 67890,
                "owner": {"name": "TestUser", "face": "https://example.com/face.jpg", "mid": 111},
                "pages": [{"cid": 67890, "page": 1, "part": "P1", "duration": 120, "dimension": {"width": 1920, "height": 1080, "rotate": 0}}]
            },
            "message": "0",
            "code": 0,
            "ttl": 1
        }"#;
    let resp: types::VideoPageInfoResp = serde_json::from_str(json)?;
    let data = resp
        .data
        .ok_or_else(|| missing("video page data should deserialize"))?;
    assert_eq!(data.title, "Test Video");
    assert_eq!(data.bvid, "BV1xx411c7XZ");
    assert_eq!(data.aid, 12345);
    let pages = data.pages.expect("video pages should deserialize");
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].duration, 120);
    assert_eq!(resp.code, 0);
    Ok(())
}

#[test]
fn test_nav_resp_deserialize() -> TestResult {
    let json = r#"{
            "data": {"isLogin": true, "uname": "TestUser", "face": "https://example.com/face.jpg", "vipStatus": 1, "mid": 12345},
            "message": "0",
            "code": 0,
            "ttl": 1
        }"#;
    let resp: types::NavResp = serde_json::from_str(json)?;
    assert!(resp.data.is_login);
    assert_eq!(resp.data.uname, "TestUser");
    assert_eq!(resp.data.mid, 12345);
    Ok(())
}

#[test]
fn test_video_url_resp_deserialize() -> TestResult {
    let json = r#"{
            "data": {
                "accept_quality": [80, 64, 32],
                "accept_description": ["1080P", "720P", "480P"],
                "quality": 80,
                "durl": [{
                    "url": "https://cdn.bilibili.com/video.flv",
                    "backup_url": ["https://backup.bilibili.com/video.flv"],
                    "size": 1000000,
                    "length": 120
                }]
            },
            "message": "0",
            "code": 0,
            "ttl": 1
        }"#;
    let resp: types::VideoUrlResp = serde_json::from_str(json)?;
    assert_eq!(resp.data.quality, 80);
    let durls = resp.data.durl.expect("DURL entries should deserialize");
    assert_eq!(durls.len(), 1);
    assert_eq!(resp.data.accept_quality, Some(vec![80, 64, 32]));
    let segments = video_segments_from_durls(&durls);
    assert_eq!(segments.len(), 1);
    assert_eq!(
        segments[0].backup_urls,
        vec!["https://backup.bilibili.com/video.flv"]
    );
    Ok(())
}

#[test]
fn test_qrcode_resp_deserialize() -> TestResult {
    let json = r#"{
            "data": {"url": "https://passport.bilibili.com/qrcode", "qrcode_key": "abc123"},
            "message": "0",
            "code": 0,
            "ttl": 180
        }"#;
    let resp: types::QrcodeResp = serde_json::from_str(json)?;
    assert_eq!(resp.data.qrcode_key, "abc123");
    assert_eq!(resp.ttl, 180);
    Ok(())
}

#[test]
fn test_extract_key_from_url() {
    assert_eq!(
        extract_key_from_url("https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png"),
        Some("7cd084941338484aae1ad9425b84077c".to_string())
    );
    assert_eq!(
        extract_key_from_url("https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"),
        Some("4932caff0ff746eab6f01bf08b70ac45".to_string())
    );
    assert_eq!(extract_key_from_url(""), None);
    assert_eq!(extract_key_from_url("no-slash"), None);
}

#[test]
fn test_gen_mixin_key() {
    // Use known img_key and sub_key values to test the mixin key generation.
    let img_key = "7cd084941338484aae1ad9425b84077c";
    let sub_key = "4932caff0ff746eab6f01bf08b70ac45";
    let mixin = gen_mixin_key(img_key, sub_key);
    // The mixin key should be exactly 32 characters
    assert_eq!(mixin.len(), 32);
    // Verify the key is deterministic
    assert_eq!(mixin, gen_mixin_key(img_key, sub_key));
    // Verify first few characters from the known encoding table:
    // MIXIN_KEY_ENC_TAB[0] = 46 → combined[46] (sub_key[14] = 'f')
    let combined = format!("{img_key}{sub_key}");
    let combined_bytes: Vec<u8> = combined.bytes().collect();
    assert_eq!(mixin.as_bytes()[0], combined_bytes[46]);
}

#[test]
fn test_gen_mixin_key_empty() {
    let mixin = gen_mixin_key("", "");
    assert!(mixin.is_empty());
}

#[test]
fn test_wbi_sign_produces_w_rid_and_wts() -> TestResult {
    let params = vec![
        ("bvid", "BV1xx411c7XZ".to_string()),
        ("cid", "12345".to_string()),
        ("fnval", "4048".to_string()),
    ];
    let mixin_key = "ea1db124af3c7062474693fa704f4ff8";
    let signed = wbi_sign(&params, mixin_key);

    let keys: Vec<&str> = signed.iter().map(|(k, _)| k.as_str()).collect();
    assert!(
        keys.contains(&"w_rid"),
        "signed params should contain w_rid"
    );
    assert!(keys.contains(&"wts"), "signed params should contain wts");
    assert!(keys.contains(&"bvid"), "signed params should contain bvid");
    assert!(keys.contains(&"cid"), "signed params should contain cid");
    assert!(
        keys.contains(&"fnval"),
        "signed params should contain fnval"
    );

    let w_rid = signed_value(&signed, "w_rid")?;
    assert_eq!(w_rid.len(), 32);
    assert!(
        w_rid.chars().all(|c| c.is_ascii_hexdigit()),
        "w_rid should be hex"
    );
    Ok(())
}

#[test]
fn test_wbi_sign_filters_special_chars() -> TestResult {
    let params = vec![("key", "hello!'()*world".to_string())];
    let mixin_key = "testkey12345678901234567890123456";
    let signed = wbi_sign(&params, mixin_key);

    let val = signed_value(&signed, "key")?;
    assert_eq!(val, "helloworld");
    Ok(())
}

#[test]
fn test_wbi_sign_url_encodes_values_for_hash() -> TestResult {
    // Values with spaces and Chinese characters should be URL-encoded
    // before hashing, matching Go's url.Values.Encode() behavior.
    let params = vec![
        ("keyword", "hello world".to_string()),
        ("name", "hello".to_string()),
    ];
    let mixin_key = "ea1db124af3c7062474693fa704f4ff8";
    let signed = wbi_sign(&params, mixin_key);

    let w_rid = signed_value(&signed, "w_rid")?;
    let wts = signed_value(&signed, "wts")?;

    // Reconstruct the expected hash using URL-encoded query string
    let mut expected_params: Vec<(&str, &str)> =
        vec![("keyword", "hello world"), ("name", "hello")];
    expected_params.push(("wts", wts));
    expected_params.sort_by(|a, b| a.0.cmp(b.0));

    let expected_query: String = {
        let mut ser = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in &expected_params {
            ser.append_pair(k, v);
        }
        ser.finish()
    };

    let mut hasher = Md5::new();
    hasher.update(format!("{expected_query}{mixin_key}").as_bytes());
    let expected_hash = hex::encode(hasher.finalize());

    assert_eq!(
        w_rid, expected_hash,
        "w_rid should match hash of URL-encoded query string"
    );
    // Verify the URL-encoded form includes %20 or + for space, not raw space
    assert!(
        expected_query.contains('+') || expected_query.contains("%20"),
        "query string should URL-encode spaces"
    );
    Ok(())
}

#[test]
fn test_wbi_sign_sorted_params() {
    let params = vec![
        ("z_param", "z".to_string()),
        ("a_param", "a".to_string()),
        ("m_param", "m".to_string()),
    ];
    let mixin_key = "testkey12345678901234567890123456";
    let signed = wbi_sign(&params, mixin_key);

    // Params before w_rid should be sorted alphabetically
    let keys_before_wrid: Vec<&str> = signed
        .iter()
        .filter(|(k, _)| k != "w_rid")
        .map(|(k, _)| k.as_str())
        .collect();
    // a_param, m_param, wts, z_param (alphabetically sorted)
    let mut sorted = keys_before_wrid.clone();
    sorted.sort_unstable();
    assert_eq!(
        keys_before_wrid, sorted,
        "params should be sorted alphabetically"
    );
}

#[test]
fn test_wbi_sign_deterministic_for_same_timestamp() -> TestResult {
    // The same params + mixin_key should produce consistent signing
    // (modulo the wts which depends on system time)
    let params = vec![("bvid", "BV1test".to_string()), ("cid", "999".to_string())];
    let mixin_key = "ea1db124af3c7062474693fa704f4ff8";
    let signed1 = wbi_sign(&params, mixin_key);
    let signed2 = wbi_sign(&params, mixin_key);

    // The wts values should be very close (same second)
    let wts1 = signed_value(&signed1, "wts")?;
    let wts2 = signed_value(&signed2, "wts")?;
    assert_eq!(wts1, wts2, "wts should be same within the same second");

    let w_rid1 = signed_value(&signed1, "w_rid")?;
    let w_rid2 = signed_value(&signed2, "w_rid")?;
    assert_eq!(
        w_rid1, w_rid2,
        "w_rid should be deterministic for same inputs"
    );
    Ok(())
}

#[test]
fn test_nav_resp_with_wbi_img_deserialize() -> TestResult {
    let json = r#"{
            "data": {
                "isLogin": true,
                "uname": "TestUser",
                "face": "https://example.com/face.jpg",
                "vipStatus": 1,
                "mid": 12345,
                "wbi_img": {
                    "img_url": "https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png",
                    "sub_url": "https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"
                }
            },
            "message": "0",
            "code": 0,
            "ttl": 1
        }"#;
    let resp: types::NavResp = serde_json::from_str(json)?;
    assert!(resp.data.wbi_img.is_some());
    let wbi_img = resp
        .data
        .wbi_img
        .ok_or_else(|| missing("wbi_img should deserialize"))?;
    assert!(wbi_img.img_url.contains("7cd084941338484aae1ad9425b84077c"));
    assert!(wbi_img.sub_url.contains("4932caff0ff746eab6f01bf08b70ac45"));
    Ok(())
}

#[test]
fn test_nav_resp_without_wbi_img_deserialize() -> TestResult {
    let json = r#"{
            "data": {
                "isLogin": false
            },
            "message": "not logged in",
            "code": -101,
            "ttl": 1
        }"#;
    let resp: types::NavResp = serde_json::from_str(json)?;
    assert_eq!(resp.code, -101);
    assert_eq!(resp.data.uname, "");
    assert_eq!(resp.data.face, "");
    assert_eq!(resp.data.vip_status, 0);
    assert_eq!(resp.data.mid, 0);
    assert!(resp.data.wbi_img.is_none());
    Ok(())
}

#[test]
fn test_is_wbi_stale_error_minus_352() {
    let err = BilibiliError::Api {
        code: -352,
        message: "signature error".to_string(),
    };
    assert!(BilibiliClient::is_wbi_stale_error(&err));
}

#[test]
fn test_is_wbi_stale_error_minus_401() {
    let err = BilibiliError::Api {
        code: -401,
        message: "unauthorized".to_string(),
    };
    assert!(BilibiliClient::is_wbi_stale_error(&err));
}

#[test]
fn test_is_wbi_stale_error_other_codes() {
    let err = BilibiliError::Api {
        code: -101,
        message: "not logged in".to_string(),
    };
    assert!(!BilibiliClient::is_wbi_stale_error(&err));

    let err = BilibiliError::Api {
        code: 0,
        message: "success".to_string(),
    };
    assert!(!BilibiliClient::is_wbi_stale_error(&err));

    let err = BilibiliError::Network("timeout".to_string());
    assert!(!BilibiliClient::is_wbi_stale_error(&err));

    let err = BilibiliError::Parse("bad json".to_string());
    assert!(!BilibiliClient::is_wbi_stale_error(&err));
}

#[test]
fn test_bilibili_api_error_uses_local_english_message() {
    let err = bilibili_api_error(-101, "nav");
    match err {
        BilibiliError::Api { code, message } => {
            assert_eq!(code, -101);
            assert_eq!(message, "Bilibili authentication is required");
        }
        other => panic!("unexpected error variant: {other}"),
    }

    let err = bilibili_api_error(12345, "video URL");
    match err {
        BilibiliError::Api { code, message } => {
            assert_eq!(code, 12345);
            assert_eq!(message, "Bilibili video URL API returned code 12345");
        }
        other => panic!("unexpected error variant: {other}"),
    }
}

#[test]
fn test_build_cookie_header_empty_returns_none() -> TestResult {
    let client = BilibiliClient::new()?;
    assert!(client.build_cookie_header().is_none());
    Ok(())
}

#[test]
fn test_build_cookie_header_multiple_joined() -> TestResult {
    let mut cookies = HashMap::new();
    cookies.insert("SESSDATA".to_string(), "abc123".to_string());
    cookies.insert("bili_jct".to_string(), "token456".to_string());
    let client = BilibiliClient::with_cookies(cookies)?;

    let header = client
        .build_cookie_header()
        .ok_or_else(|| missing("cookie header should be present"))?;
    assert!(header.contains("SESSDATA=abc123"));
    assert!(header.contains("bili_jct=token456"));
    assert!(header.contains("; "));
    Ok(())
}

#[test]
fn test_build_cookie_header_sanitizes_crlf() -> TestResult {
    let mut cookies = HashMap::new();
    cookies.insert("evil\r\nkey".to_string(), "evil\r\nvalue".to_string());
    let client = BilibiliClient::with_cookies(cookies)?;

    let header = client
        .build_cookie_header()
        .ok_or_else(|| missing("cookie header should be present"))?;
    assert!(!header.contains('\r'));
    assert!(!header.contains('\n'));
    assert!(header.contains("evilkey=evilvalue"));
    Ok(())
}

#[tokio::test]
async fn test_wbi_state_is_isolated_per_client_instance() -> TestResult {
    let client_a = BilibiliClient::new()?;
    let client_b = BilibiliClient::new()?;

    let state_a = client_a.shared_wbi_state();
    let state_b = client_b.shared_wbi_state();

    state_a.reset_for_tests();
    state_b.reset_for_tests();

    state_a.set_wbi_key("key-a".to_string());
    state_b.set_wbi_key("key-b".to_string());

    assert_eq!(state_a.get_valid_wbi_key().as_deref(), Some("key-a"));
    assert_eq!(state_b.get_valid_wbi_key().as_deref(), Some("key-b"));

    state_a.record_failure_for_tests();
    state_a.record_failure_for_tests();
    state_a.record_failure_for_tests();

    assert!(state_a.has_exceeded_max_failures_for_tests());
    assert!(!state_b.has_exceeded_max_failures_for_tests());
    assert_eq!(state_a.api_call_count(), 0);
    assert_eq!(state_b.api_call_count(), 0);
    Ok(())
}

#[tokio::test]
async fn test_force_wbi_refresh_reuses_key_written_while_waiting() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(nav_response_with_wbi_keys(
                "7cd084941338484aae1ad9425b84077c",
                "4932caff0ff746eab6f01bf08b70ac45",
            )),
        )
        .expect(0)
        .mount(&server)
        .await;

    let state = Arc::new(WbiState::default());
    state.set_wbi_key("old-key".to_string());
    let refresh_guard = state.acquire_refresh_for_tests().await;
    let client = BilibiliClient::new_with_transport(
        test_http_client(),
        test_http_client(),
        test_endpoints(server.uri()),
        state.clone(),
        SsrfGuard::strict_policy(),
    );

    let waiter_client = Arc::new(client);
    let waiter = {
        let waiter_client = waiter_client.clone();
        tokio::spawn(async move { waiter_client.get_wbi_mixin_key_for_tests(true).await })
    };
    tokio::task::yield_now().await;

    state.set_wbi_key("fresh-key".to_string());
    drop(refresh_guard);

    let key = waiter.await??;
    assert_eq!(key, "fresh-key");
    assert_eq!(state.get_valid_wbi_key().as_deref(), Some("fresh-key"));
    assert_eq!(state.api_call_count(), 0);
    Ok(())
}

#[tokio::test]
async fn test_wbi_refresh_rechecks_failure_breaker_after_waiting() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(nav_response_with_wbi_keys(
                "7cd084941338484aae1ad9425b84077c",
                "4932caff0ff746eab6f01bf08b70ac45",
            )),
        )
        .expect(0)
        .mount(&server)
        .await;

    let state = Arc::new(WbiState::default());
    let refresh_guard = state.acquire_refresh_for_tests().await;
    let client = Arc::new(BilibiliClient::new_with_transport(
        test_http_client(),
        test_http_client(),
        test_endpoints(server.uri()),
        state.clone(),
        SsrfGuard::strict_policy(),
    ));

    let waiter = {
        let client = client.clone();
        tokio::spawn(async move { client.get_wbi_mixin_key_for_tests(false).await })
    };
    tokio::task::yield_now().await;

    state.record_failure_for_tests();
    state.record_failure_for_tests();
    state.record_failure_for_tests();
    drop(refresh_guard);

    let err = waiter
        .await?
        .expect_err("waiter should observe the breaker after acquiring refresh lock");
    assert!(
        err.to_string().contains("too many consecutive failures"),
        "unexpected error: {err}"
    );
    assert_eq!(state.api_call_count(), 0);
    Ok(())
}

#[test]
fn test_parse_danmaku_gift_with_huge_count_no_panic() -> TestResult {
    let json = serde_json::json!({
        "cmd": "SEND_GIFT",
        "data": {
            "uname": "TestUser",
            "giftName": "TestGift",
            "num": u64::from(u32::MAX) + 1  // exceeds u32
        }
    });

    let DanmakuMessage::Gift { count, .. } = parse_danmaku_cmd("SEND_GIFT", &json) else {
        return Err(missing("expected Gift message variant"));
    };
    assert_eq!(count, u32::MAX, "Overflow should clamp to u32::MAX");
    Ok(())
}

#[test]
fn test_parse_danmaku_gift_with_normal_count() -> TestResult {
    let json = serde_json::json!({
        "cmd": "SEND_GIFT",
        "data": {
            "uname": "TestUser",
            "giftName": "TestGift",
            "num": 5
        }
    });

    let DanmakuMessage::Gift { count, .. } = parse_danmaku_cmd("SEND_GIFT", &json) else {
        return Err(missing("expected Gift message variant"));
    };
    assert_eq!(count, 5);
    Ok(())
}

#[test]
fn test_build_auth_packet_does_not_panic_on_normal_token() -> TestResult {
    let packet = build_auth_packet(12345, "normal_token_value")?;
    assert!(!packet.is_empty());
    let len = u32::from_be_bytes([packet[0], packet[1], packet[2], packet[3]]);
    assert_eq!(len as usize, packet.len());
    Ok(())
}

#[tokio::test]
#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
async fn test_resolve_validated_danmaku_addr_blocks_localhost_with_strict_ssrf() {
    let guard = SsrfGuard::strict_policy();
    let err = resolve_validated_danmaku_addr("localhost", 443, &guard)
        .await
        .expect_err("strict SSRF policy should block localhost");
    assert!(
        err.to_string().contains("blocked by SSRF policy"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
async fn test_resolve_validated_danmaku_addr_blocks_private_ip_literal_with_strict_ssrf() {
    let guard = SsrfGuard::strict_policy();
    let err = resolve_validated_danmaku_addr("127.0.0.1", 443, &guard)
        .await
        .expect_err("strict SSRF policy should block loopback IP literals");
    assert!(
        err.to_string().contains("blocked by SSRF policy"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
async fn test_resolve_validated_danmaku_addr_accepts_public_ip_literal() -> TestResult {
    let guard = SsrfGuard::strict_policy();
    let addr = resolve_validated_danmaku_addr("93.184.216.34", 443, &guard).await?;
    assert_eq!(addr, "93.184.216.34:443".parse::<std::net::SocketAddr>()?);
    Ok(())
}

#[tokio::test]
#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
async fn test_resolve_validated_danmaku_addr_rejects_out_of_range_port() {
    let guard = SsrfGuard::strict_policy();
    let err = resolve_validated_danmaku_addr("93.184.216.34", u32::from(u16::MAX) + 1, &guard)
        .await
        .expect_err("invalid WebSocket port must be rejected");
    assert!(
        err.to_string().contains("port out of range"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_parse_danmaku_packet_too_short_returns_empty() {
    let short_data = [0u8; 15];
    let result = parse_danmaku_packet(&short_data);
    assert!(result.is_empty());
}

#[test]
fn test_parse_danmaku_packet_empty_returns_empty() {
    let result = parse_danmaku_packet(&[]);
    assert!(result.is_empty());
}

#[test]
fn test_parse_danmaku_packet_invalid_zlib_returns_empty() {
    let mut packet = Vec::new();
    packet.extend_from_slice(&16u32.to_be_bytes());
    packet.extend_from_slice(&16u16.to_be_bytes());
    packet.extend_from_slice(&2u16.to_be_bytes());
    packet.extend_from_slice(&5u32.to_be_bytes());
    packet.extend_from_slice(&1u32.to_be_bytes());
    packet.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

    let result = parse_danmaku_packet(&packet);
    assert!(result.is_empty());
}

#[test]
fn test_parse_danmaku_packet_invalid_brotli_returns_empty() {
    let mut packet = Vec::new();
    packet.extend_from_slice(&20u32.to_be_bytes());
    packet.extend_from_slice(&16u16.to_be_bytes());
    packet.extend_from_slice(&3u16.to_be_bytes());
    packet.extend_from_slice(&5u32.to_be_bytes());
    packet.extend_from_slice(&1u32.to_be_bytes());
    packet.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

    let result = parse_danmaku_packet(&packet);
    assert!(result.is_empty());
}

#[test]
fn test_read_limited_danmaku_decompressed_rejects_oversized_output() -> TestResult {
    let oversized_len = usize::try_from(MAX_DANMAKU_DECOMPRESS_SIZE)?
        .checked_add(1)
        .ok_or_else(|| missing("danmaku decompression test allocation length must not overflow"))?;
    let oversized = vec![0u8; oversized_len];
    let result =
        read_limited_danmaku_decompressed(std::io::Cursor::new(oversized), "identity-test", 1);
    assert!(result.is_none());
    Ok(())
}

#[test]
fn test_parse_danmaku_packet_unknown_protocol_version_returns_empty() {
    let mut packet = Vec::new();
    packet.extend_from_slice(&20u32.to_be_bytes());
    packet.extend_from_slice(&16u16.to_be_bytes());
    packet.extend_from_slice(&99u16.to_be_bytes());
    packet.extend_from_slice(&5u32.to_be_bytes());
    packet.extend_from_slice(&1u32.to_be_bytes());
    packet.extend_from_slice(&[0, 0, 0, 0]);

    let result = parse_danmaku_packet(&packet);
    assert!(result.is_empty());
}

#[test]
fn test_parse_danmaku_packet_valid_heartbeat() -> TestResult {
    let mut packet = Vec::new();
    packet.extend_from_slice(&20u32.to_be_bytes());
    packet.extend_from_slice(&16u16.to_be_bytes());
    packet.extend_from_slice(&1u16.to_be_bytes());
    packet.extend_from_slice(&3u32.to_be_bytes());
    packet.extend_from_slice(&1u32.to_be_bytes());
    packet.extend_from_slice(&12345u32.to_be_bytes());

    let result = parse_danmaku_packet(&packet);
    assert_eq!(result.len(), 1);
    let DanmakuMessage::Heartbeat { online_count } = &result[0] else {
        return Err(missing("expected Heartbeat message"));
    };
    assert_eq!(*online_count, 12345);
    Ok(())
}

#[tokio::test]
async fn test_notify_timeout_mechanism() {
    let timeout_duration = std::time::Duration::from_millis(10);

    let local_notify = tokio::sync::Notify::new();

    let result = tokio::time::timeout(timeout_duration, local_notify.notified()).await;
    assert!(
        result.is_err(),
        "Should timeout when no notification is sent"
    );
}

#[tokio::test]
async fn test_notify_arrives_before_timeout() -> TestResult {
    use std::sync::Arc;
    let local_notify = Arc::new(tokio::sync::Notify::new());
    let timeout_duration = std::time::Duration::from_millis(100);

    let notify = Arc::clone(&local_notify);
    let wait_task = tokio::spawn(async move {
        let result = tokio::time::timeout(timeout_duration, notify.notified()).await;
        result.is_ok()
    });

    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    local_notify.notify_waiters();

    let notification_received = wait_task.await?;
    assert!(
        notification_received,
        "Notification should have arrived before timeout"
    );
    Ok(())
}

#[tokio::test]
async fn list_popular_videos_preserves_multi_part_metadata() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/popular"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {
                "list": [{
                    "aid": 42,
                    "bvid": "BV1xx411c7XZ",
                    "cid": 100,
                    "title": "Multi-part video",
                    "pic": "https://example.com/cover.jpg",
                    "duration": 120,
                    "videos": 3,
                    "pubdate": 1234,
                    "ctime": 1200,
                    "created": 1100,
                    "owner": {"name": "UP"}
                }],
                "no_more": true
            }
        })))
        .mount(&server)
        .await;
    let client = BilibiliClient::new_with_transport(
        test_http_client(),
        test_http_client(),
        test_endpoints(server.uri()),
        Arc::new(WbiState::default()),
        SsrfGuard::strict_policy(),
    );

    let page = client.list_popular_videos(1, 20).await?;

    assert!(!page.has_more);
    assert_eq!(page.items[0].part_count, 3);
    assert_eq!(page.items[0].cid, 100);
    assert_eq!(page.items[0].author, "UP");
    assert_eq!(page.items[0].published_at, 1234);
    Ok(())
}

#[tokio::test]
async fn list_video_parts_uses_first_frame_as_part_cover() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/view"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {
                "title": "Video",
                "pic": "https://example.com/cover.jpg",
                "bvid": "BV1xx411c7XZ",
                "aid": 42,
                "cid": 100,
                "owner": {"name": "UP", "face": "", "mid": 1},
                "pages": [{
                    "cid": 101,
                    "page": 1,
                    "part": "Part one",
                    "duration": 60,
                    "dimension": {"width": 1920, "height": 1080, "rotate": 0},
                    "first_frame": "https://example.com/frame.jpg"
                }]
            }
        })))
        .mount(&server)
        .await;
    let client = BilibiliClient::new_with_transport(
        test_http_client(),
        test_http_client(),
        test_endpoints(server.uri()),
        Arc::new(WbiState::default()),
        SsrfGuard::strict_policy(),
    );

    let parts = client.list_video_parts(0, "BV1xx411c7XZ").await?;

    assert_eq!(parts.parts[0].cover, "https://example.com/frame.jpg");
    assert_eq!(parts.parts[0].width, 1920);
    assert_eq!(parts.parts[0].duration_seconds, 60);
    Ok(())
}

#[test]
fn parse_dash_info_includes_dolby_flac_and_backup_urls() -> TestResult {
    let dash: types::DashInfo = serde_json::from_value(json!({
        "duration": 60.0,
        "minBufferTime": 1.5,
        "video": [{
            "id": 80,
            "baseUrl": "https://cdn.example.com/video.m4s",
            "backupUrl": ["https://backup.example.com/video.m4s"],
            "mimeType": "video/mp4",
            "codecs": "av01.0.08M.08",
            "codecid": 13,
            "width": 1920,
            "height": 1080,
            "frameRate": "60",
            "bandwidth": 1000,
            "sar": "1:1",
            "SegmentBase": {"Initialization": "0-1", "indexRange": "2-3"}
        }],
        "audio": [],
        "dolby": {"audio": [{
            "id": 30250,
            "baseUrl": "https://cdn.example.com/dolby.m4s",
            "mimeType": "audio/mp4",
            "codecs": "ec-3",
            "bandwidth": 448_000,
            "SegmentBase": {"Initialization": "0-1", "indexRange": "2-3"}
        }]},
        "flac": {"audio": {
            "id": 30251,
            "baseUrl": "https://cdn.example.com/flac.m4s",
            "backupUrl": ["https://backup.example.com/flac.m4s"],
            "mimeType": "audio/mp4",
            "codecs": "fLaC",
            "bandwidth": 1_200_000,
            "SegmentBase": {"Initialization": "0-1", "indexRange": "2-3"}
        }}
    }))?;

    let (dash, hevc) = parse_dash_info(&dash, &[]);

    assert!(hevc.video_streams.is_empty());
    assert_eq!(dash.video_streams[0].codecid, 13);
    assert_eq!(dash.video_streams[0].backup_urls.len(), 1);
    assert_eq!(dash.audio_streams.len(), 2);
    assert_eq!(dash.audio_streams[0].quality_name, "Dolby Atmos");
    assert_eq!(dash.audio_streams[1].quality_name, "Hi-Res FLAC");
    assert_eq!(dash.audio_streams[1].backup_urls.len(), 1);
    Ok(())
}

#[tokio::test]
async fn list_recommended_live_rooms_maps_room_metadata() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xlive/web-interface/v1/webMain/getMoreRecList"))
        .and(query_param("platform", "web"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {
                "recommend_room_list": [{
                    "roomid": "101",
                    "title": "Live room",
                    "keyframe": "https://example.com/live.jpg",
                    "uname": "Streamer",
                    "uid": 202,
                    "face": "https://example.com/avatar.jpg",
                    "area_v2_parent_id": "1",
                    "area_v2_parent_name": "Games",
                    "area_v2_id": 2,
                    "area_v2_name": "Indie",
                    "online": "303"
                }]
            }
        })))
        .mount(&server)
        .await;
    let client = BilibiliClient::new_with_transport(
        test_http_client(),
        test_http_client(),
        test_endpoints(server.uri()),
        Arc::new(WbiState::default()),
        SsrfGuard::strict_policy(),
    );

    let result = client.list_recommended_live_rooms(1, 20).await?;

    assert_eq!(result.items.len(), 1);
    let room = &result.items[0];
    assert_eq!(room.room_id, 101);
    assert_eq!(room.author, "Streamer");
    assert_eq!(room.parent_area_name, "Games");
    assert_eq!(room.area_name, "Indie");
    assert_eq!(room.online, 303);
    assert!(!result.has_more);
    Ok(())
}

#[tokio::test]
async fn list_followed_live_rooms_sends_cookie_and_preserves_pagination() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xlive/web-ucenter/user/following"))
        .and(query_param("page", "2"))
        .and(query_param("page_size", "10"))
        .and(header("cookie", "SESSDATA=session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {
                "list": [{"room_id": 404, "title": "Followed", "uname": "UP"}],
                "count": "21",
                "totalPage": "3"
            }
        })))
        .mount(&server)
        .await;
    let client = BilibiliClient::with_cookies_and_transport(
        HashMap::from([("SESSDATA".to_string(), "session".to_string())]),
        test_http_client(),
        test_http_client(),
        test_endpoints(server.uri()),
        Arc::new(WbiState::default()),
        SsrfGuard::strict_policy(),
    );

    let result = client.list_followed_live_rooms(2, 20).await?;

    assert_eq!(result.total, Some(21));
    assert!(result.has_more);
    assert_eq!(result.items[0].room_id, 404);
    Ok(())
}

#[tokio::test]
async fn list_area_live_rooms_sends_area_and_page_parameters() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xlive/web-interface/v1/second/getList"))
        .and(query_param("parent_area_id", "6"))
        .and(query_param("area_id", "7"))
        .and(query_param("page", "2"))
        .and(query_param("page_size", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {
                "list": [{"roomid": 505, "title": "Area room"}],
                "count": 45,
                "has_more": 1
            }
        })))
        .mount(&server)
        .await;
    let client = BilibiliClient::new_with_transport(
        test_http_client(),
        test_http_client(),
        test_endpoints(server.uri()),
        Arc::new(WbiState::default()),
        SsrfGuard::strict_policy(),
    );

    let result = client.list_area_live_rooms(6, 7, 2, 20).await?;

    assert_eq!(result.total, Some(45));
    assert!(result.has_more);
    assert_eq!(result.items[0].room_id, 505);
    Ok(())
}

#[tokio::test]
async fn list_live_areas_accepts_string_and_number_ids() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/room/v1/Area/getList"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": [{
                "id": "9",
                "name": "Parent",
                "list": [
                    {"id": "10", "parent_id": 9, "name": "Child A", "pic": "a", "hot_status": "1"},
                    {"id": 11, "name": "Child B", "parent_name": "Override"}
                ]
            }]
        })))
        .mount(&server)
        .await;
    let client = BilibiliClient::new_with_transport(
        test_http_client(),
        test_http_client(),
        test_endpoints(server.uri()),
        Arc::new(WbiState::default()),
        SsrfGuard::strict_policy(),
    );

    let areas = client.list_live_areas().await?;

    assert_eq!(areas.len(), 2);
    assert_eq!(areas[0].id, 10);
    assert_eq!(areas[0].parent_id, 9);
    assert_eq!(areas[0].parent_name, "Parent");
    assert!(areas[0].hot);
    assert_eq!(areas[1].id, 11);
    assert_eq!(areas[1].parent_name, "Override");
    Ok(())
}

#[tokio::test]
async fn parse_pgc_page_includes_named_extra_sections() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pgc/view/web/season"))
        .and(query_param("season_id", "42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "message": "success",
            "result": {
                "season_id": 42,
                "title": "Season",
                "cover": "https://example.com/season.jpg",
                "actors": "Actor",
                "episodes": [{
                    "title": "1",
                    "long_title": "Main episode",
                    "bvid": "BV1main",
                    "cid": 101,
                    "ep_id": 201,
                    "aid": 301,
                    "cover": "https://example.com/main.jpg",
                    "duration": 120_000
                }],
                "section": [{
                    "title": "PV",
                    "episodes": [{
                        "title": "PV1",
                        "long_title": "Trailer",
                        "bvid": "BV1extra",
                        "cid": 102,
                        "ep_id": 202,
                        "aid": 302,
                        "cover": "https://example.com/extra.jpg",
                        "duration": 30000
                    }]
                }]
            }
        })))
        .mount(&server)
        .await;
    let client = BilibiliClient::new_with_transport(
        test_http_client(),
        test_http_client(),
        test_endpoints(server.uri()),
        Arc::new(WbiState::default()),
        SsrfGuard::strict_policy(),
    );

    let page = client.parse_pgc_page(0, 42).await?;

    assert_eq!(page.video_infos.len(), 2);
    assert_eq!(page.video_infos[0].name, "Main episode");
    assert_eq!(page.video_infos[0].duration_seconds, 120);
    assert_eq!(page.video_infos[1].name, "PV - Trailer");
    assert_eq!(page.video_infos[1].page, 2);
    assert_eq!(page.video_infos[1].duration_seconds, 30);
    Ok(())
}

#[tokio::test]
async fn list_favorite_folders_uses_authenticated_mid_and_maps_attributes() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .and(header("cookie", "SESSDATA=session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "message": "0",
            "ttl": 1,
            "data": {
                "isLogin": true,
                "mid": 42,
                "uname": "Tester",
                "face": "",
                "vipStatus": 0
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/v3/fav/folder/created/list-all"))
        .and(query_param("up_mid", "42"))
        .and(header("cookie", "SESSDATA=session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {
                "list": [
                    {"id": 100, "attr": 0, "title": "Default", "media_count": 8},
                    {"id": 101, "attr": 3, "title": "Private", "media_count": 2}
                ]
            }
        })))
        .mount(&server)
        .await;
    let client = BilibiliClient::with_cookies_and_transport(
        HashMap::from([("SESSDATA".to_string(), "session".to_string())]),
        test_http_client(),
        test_http_client(),
        test_endpoints(server.uri()),
        Arc::new(WbiState::default()),
        SsrfGuard::strict_policy(),
    );

    let folders = client.list_favorite_folders().await?;

    assert_eq!(folders.len(), 2);
    assert!(folders[0].default_folder);
    assert!(!folders[0].private);
    assert!(folders[1].private);
    assert!(!folders[1].default_folder);
    assert_eq!(folders[1].media_count, 2);
    Ok(())
}

#[tokio::test]
async fn list_followed_pgc_uses_native_page_and_maps_season_metadata() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "message": "0",
            "ttl": 1,
            "data": {"isLogin": true, "mid": 42, "uname": "Tester", "vipStatus": 0}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/space/bangumi/follow/list"))
        .and(query_param("vmid", "42"))
        .and(query_param("type", "2"))
        .and(query_param("pn", "2"))
        .and(query_param("ps", "15"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {
                "total": 31,
                "list": [{
                    "season_id": 77,
                    "title": "Cinema",
                    "cover": "https://example.com/cinema.jpg",
                    "evaluate": "Description",
                    "new_ep": {"index_show": "Updated to 12"}
                }]
            }
        })))
        .mount(&server)
        .await;
    let client = BilibiliClient::with_cookies_and_transport(
        HashMap::from([("SESSDATA".to_string(), "session".to_string())]),
        test_http_client(),
        test_http_client(),
        test_endpoints(server.uri()),
        Arc::new(WbiState::default()),
        SsrfGuard::strict_policy(),
    );

    let result = client.list_followed_pgc(2, 2, 15).await?;

    assert_eq!(result.total, 31);
    assert!(result.has_more);
    assert_eq!(result.items[0].season_id, 77);
    assert_eq!(result.items[0].description, "Description");
    assert_eq!(result.items[0].latest_episode, "Updated to 12");
    Ok(())
}

#[tokio::test]
async fn list_history_forwards_native_cursor_and_preserves_playable_targets() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/history/cursor"))
        .and(query_param("type", "all"))
        .and(query_param("ps", "30"))
        .and(query_param("max", "77"))
        .and(query_param("view_at", "123456"))
        .and(query_param("business", "archive"))
        .and(header("cookie", "SESSDATA=session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {
                "cursor": {"max": 88, "view_at": 123_400, "business": "pgc"},
                "list": [
                    {
                        "title": "Video",
                        "long_title": "Part one",
                        "cover": "https://example.com/video.jpg",
                        "author_name": "Uploader",
                        "view_at": 123_455,
                        "progress": 42,
                        "duration": 120,
                        "history": {
                            "oid": 10,
                            "epid": 0,
                            "bvid": "BV1234567890",
                            "cid": 11,
                            "business": "archive"
                        }
                    },
                    {
                        "title": "Episode",
                        "view_at": 123_454,
                        "progress": 60,
                        "duration": 1500,
                        "history": {
                            "oid": 20,
                            "epid": 21,
                            "bvid": "",
                            "cid": 22,
                            "business": "pgc"
                        }
                    },
                    {
                        "title": "Offline live",
                        "live_status": 0,
                        "history": {"oid": 30, "business": "live"}
                    }
                ]
            }
        })))
        .mount(&server)
        .await;
    let client = BilibiliClient::with_cookies_and_transport(
        HashMap::from([("SESSDATA".to_string(), "session".to_string())]),
        test_http_client(),
        test_http_client(),
        test_endpoints(server.uri()),
        Arc::new(WbiState::default()),
        SsrfGuard::strict_policy(),
    );

    let result = client
        .list_history(
            "all",
            Some(&HistoryCursor {
                max: 77,
                view_at: 123_456,
                business: "archive".to_string(),
            }),
            50,
        )
        .await?;

    assert_eq!(result.items.len(), 2);
    assert!(matches!(
        &result.items[0].resource,
        HistoryResource::Video {
            aid: 10,
            cid: 11,
            ..
        }
    ));
    assert!(matches!(
        result.items[1].resource,
        HistoryResource::Pgc { epid: 21, cid: 22 }
    ));
    assert_eq!(result.cursor.as_ref().map(|cursor| cursor.max), Some(88));
    assert!(result.has_more);
    Ok(())
}

#[tokio::test]
async fn list_pgc_timeline_maps_schedule_and_resolves_published_episode_cid() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pgc/web/timeline"))
        .and(query_param("types", "4"))
        .and(query_param("before", "2"))
        .and(query_param("after", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "result": [{
                "date": "7-14",
                "day_of_week": 2,
                "episodes": [{
                    "episode_id": 501,
                    "season_id": 50,
                    "title": "Published show",
                    "pub_index": "Episode 1",
                    "cover": "https://example.com/season.jpg",
                    "ep_cover": "https://example.com/episode.jpg",
                    "pub_ts": 1_700_000_000,
                    "published": 1,
                    "delay": 0
                }, {
                    "episode_id": 502,
                    "season_id": 51,
                    "title": "Upcoming show",
                    "pub_index": "Episode 3",
                    "pub_ts": 1_700_003_600,
                    "published": 0,
                    "delay": 1,
                    "delay_reason": "Delayed until Friday"
                }]
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/pgc/view/web/season"))
        .and(query_param("ep_id", "501"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "message": "success",
            "result": {
                "season_id": 50,
                "title": "Published show",
                "cover": "https://example.com/season.jpg",
                "actors": "",
                "episodes": [{
                    "title": "1",
                    "long_title": "Episode 1",
                    "bvid": "BV1timeline",
                    "cid": 777,
                    "ep_id": 501,
                    "aid": 778,
                    "cover": "https://example.com/episode.jpg",
                    "duration": 1_500_000
                }]
            }
        })))
        .mount(&server)
        .await;
    let client = BilibiliClient::new_with_transport(
        test_http_client(),
        test_http_client(),
        test_endpoints(server.uri()),
        Arc::new(WbiState::default()),
        SsrfGuard::strict_policy(),
    );

    let items = client.list_pgc_timeline(4, 2, 5).await?;

    assert_eq!(items.len(), 2);
    assert_eq!(items[0].episode_id, 501);
    assert_eq!(items[0].cid, 777);
    assert!(items[0].published);
    assert_eq!(items[1].episode_id, 502);
    assert_eq!(items[1].cid, 0);
    assert!(items[1].delayed);
    assert_eq!(items[1].delay_reason, "Delayed until Friday");
    Ok(())
}

#[tokio::test]
async fn list_pgc_seasons_forwards_filters_and_maps_index_metadata() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pgc/season/index/result"))
        .and(query_param("season_type", "2"))
        .and(query_param("st", "2"))
        .and(query_param("type", "1"))
        .and(query_param("page", "3"))
        .and(query_param("pagesize", "25"))
        .and(query_param("order", "4"))
        .and(query_param("sort", "1"))
        .and(query_param("is_finish", "1"))
        .and(query_param("area", "2"))
        .and(query_param("release_date", "[2020,2026)"))
        .and(query_param("style_id", "10010"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "message": "success",
            "data": {
                "has_next": 1,
                "total": 77,
                "list": [{
                    "season_id": 900,
                    "media_id": 901,
                    "title": "Indexed movie",
                    "subTitle": "Movie subtitle",
                    "cover": "https://example.com/movie.jpg",
                    "badge": "Exclusive",
                    "index_show": "Full movie",
                    "score": "9.8",
                    "is_finish": 1,
                    "season_type": 2,
                    "first_ep": {
                        "cover": "https://example.com/first.jpg",
                        "ep_id": 902
                    }
                }]
            }
        })))
        .mount(&server)
        .await;
    let client = BilibiliClient::new_with_transport(
        test_http_client(),
        test_http_client(),
        test_endpoints(server.uri()),
        Arc::new(WbiState::default()),
        SsrfGuard::strict_policy(),
    );

    let page = client
        .list_pgc_seasons(
            2,
            3,
            25,
            4,
            true,
            Some(true),
            Some("2"),
            Some("[2020,2026)"),
            Some(10010),
        )
        .await?;

    assert_eq!(page.total, 77);
    assert!(page.has_more);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].season_id, 900);
    assert_eq!(page.items[0].media_id, 901);
    assert_eq!(page.items[0].first_episode_id, 902);
    assert_eq!(page.items[0].subtitle, "Movie subtitle");
    assert_eq!(page.items[0].score, "9.8");
    assert!(page.items[0].finished);
    assert_eq!(page.items[0].season_type, 2);
    Ok(())
}
