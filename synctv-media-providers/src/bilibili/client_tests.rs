use super::*;

#[test]
fn test_extract_bvid() {
    assert_eq!(
        BilibiliClient::extract_bvid("https://www.bilibili.com/video/BV1xx411c7XZ"),
        Some("BV1xx411c7XZ".to_string())
    );
}

#[test]
fn test_extract_epid() {
    assert_eq!(
        BilibiliClient::extract_epid("https://www.bilibili.com/bangumi/play/ep12345"),
        Some("ep12345".to_string())
    );
}

#[test]
fn test_is_short_link() {
    assert!(BilibiliClient::is_short_link("https://b23.tv/abc123"));
    assert!(!BilibiliClient::is_short_link(
        "https://www.bilibili.com/video/BV123"
    ));
}

#[test]
fn test_quality_conversion() {
    assert_eq!(Quality::P1080.to_qn(), 80);
    assert_eq!(Quality::from_qn(64), Quality::P720);
    assert_eq!(Quality::P480.as_str(), "480P");
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
fn test_match_url_video() {
    let (media_type, id) =
        BilibiliClient::match_url("https://www.bilibili.com/video/BV1xx411c7XZ").unwrap();
    assert_eq!(media_type, "bv");
    assert_eq!(id, "BV1xx411c7XZ");
}

#[test]
fn test_match_url_bangumi_ep() {
    let (media_type, id) =
        BilibiliClient::match_url("https://www.bilibili.com/bangumi/play/ep12345").unwrap();
    assert_eq!(media_type, "ep");
    assert_eq!(id, "12345");
}

#[test]
fn test_match_url_bangumi_ss() {
    let (media_type, id) =
        BilibiliClient::match_url("https://www.bilibili.com/bangumi/play/ss67890").unwrap();
    assert_eq!(media_type, "ss");
    assert_eq!(id, "67890");
}

#[test]
fn test_match_url_live() {
    let (media_type, id) =
        BilibiliClient::match_url("https://live.bilibili.com/live/12345").unwrap();
    assert_eq!(media_type, "live");
    assert_eq!(id, "12345");

    let (media_type, id) =
        BilibiliClient::match_url("https://live.bilibili.com/76?live_from=85002").unwrap();
    assert_eq!(media_type, "live");
    assert_eq!(id, "76");

    let (media_type, id) =
        BilibiliClient::match_url("https://live.bilibili.com/21452505#main").unwrap();
    assert_eq!(media_type, "live");
    assert_eq!(id, "21452505");
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
fn test_client_creation_no_cookies() {
    let client = BilibiliClient::new().unwrap();
    assert!(client.cookies.is_none());
}

#[test]
fn test_client_creation_with_cookies() {
    let mut cookies = HashMap::new();
    cookies.insert("SESSDATA".to_string(), "abc123".to_string());
    let client = BilibiliClient::with_cookies(cookies.clone()).unwrap();
    assert!(client.cookies.is_some());
    assert_eq!(
        client.cookies.as_ref().unwrap().get("SESSDATA"),
        Some(&"abc123".to_string())
    );
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
fn test_video_page_info_deserialize() {
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
    let resp: types::VideoPageInfoResp = serde_json::from_str(json).unwrap();
    let data = resp.data.expect("video page data should deserialize");
    assert_eq!(data.title, "Test Video");
    assert_eq!(data.bvid, "BV1xx411c7XZ");
    assert_eq!(data.aid, 12345);
    assert_eq!(data.pages.len(), 1);
    assert_eq!(data.pages[0].duration, 120);
    assert_eq!(resp.code, 0);
}

#[test]
fn test_nav_resp_deserialize() {
    let json = r#"{
            "data": {"isLogin": true, "uname": "TestUser", "face": "https://example.com/face.jpg", "vipStatus": 1, "mid": 12345},
            "message": "0",
            "code": 0,
            "ttl": 1
        }"#;
    let resp: types::NavResp = serde_json::from_str(json).unwrap();
    assert!(resp.data.is_login);
    assert_eq!(resp.data.uname, "TestUser");
    assert_eq!(resp.data.mid, 12345);
}

#[test]
fn test_video_url_resp_deserialize() {
    let json = r#"{
            "data": {
                "accept_quality": [80, 64, 32],
                "accept_description": ["1080P", "720P", "480P"],
                "quality": 80,
                "durl": [{"url": "https://cdn.bilibili.com/video.flv", "size": 1000000, "length": 120}]
            },
            "message": "0",
            "code": 0,
            "ttl": 1
        }"#;
    let resp: types::VideoUrlResp = serde_json::from_str(json).unwrap();
    assert_eq!(resp.data.quality, 80);
    assert_eq!(resp.data.durl.len(), 1);
    assert_eq!(resp.data.accept_quality, vec![80, 64, 32]);
}

#[test]
fn test_qrcode_resp_deserialize() {
    let json = r#"{
            "data": {"url": "https://passport.bilibili.com/qrcode", "qrcode_key": "abc123"},
            "message": "0",
            "code": 0,
            "ttl": 180
        }"#;
    let resp: types::QrcodeResp = serde_json::from_str(json).unwrap();
    assert_eq!(resp.data.qrcode_key, "abc123");
    assert_eq!(resp.ttl, 180);
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
fn test_wbi_sign_produces_w_rid_and_wts() {
    let params = vec![
        ("bvid", "BV1xx411c7XZ".to_string()),
        ("cid", "12345".to_string()),
        ("fnval", "4048".to_string()),
    ];
    let mixin_key = "ea1db124af3c7062474693fa704f4ff8";
    let signed = wbi_sign(&params, mixin_key);

    // Should contain w_rid and wts in addition to original params
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

    // w_rid should be a 32-char hex MD5 hash
    let w_rid = signed
        .iter()
        .find(|(k, _)| k == "w_rid")
        .map(|(_, v)| v.as_str())
        .expect("w_rid missing");
    assert_eq!(w_rid.len(), 32);
    assert!(
        w_rid.chars().all(|c| c.is_ascii_hexdigit()),
        "w_rid should be hex"
    );
}

#[test]
fn test_wbi_sign_filters_special_chars() {
    let params = vec![("key", "hello!'()*world".to_string())];
    let mixin_key = "testkey12345678901234567890123456";
    let signed = wbi_sign(&params, mixin_key);

    // The value should have !'()* removed
    let val = signed
        .iter()
        .find(|(k, _)| k == "key")
        .map(|(_, v)| v.as_str())
        .expect("key missing");
    assert_eq!(val, "helloworld");
}

#[test]
fn test_wbi_sign_url_encodes_values_for_hash() {
    // Values with spaces and Chinese characters should be URL-encoded
    // before hashing, matching Go's url.Values.Encode() behavior.
    let params = vec![
        ("keyword", "hello world".to_string()),
        ("name", "hello".to_string()),
    ];
    let mixin_key = "ea1db124af3c7062474693fa704f4ff8";
    let signed = wbi_sign(&params, mixin_key);

    let w_rid = signed
        .iter()
        .find(|(k, _)| k == "w_rid")
        .map(|(_, v)| v.as_str())
        .expect("w_rid missing");
    let wts = signed
        .iter()
        .find(|(k, _)| k == "wts")
        .map(|(_, v)| v.as_str())
        .expect("wts missing");

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
fn test_wbi_sign_deterministic_for_same_timestamp() {
    // The same params + mixin_key should produce consistent signing
    // (modulo the wts which depends on system time)
    let params = vec![("bvid", "BV1test".to_string()), ("cid", "999".to_string())];
    let mixin_key = "ea1db124af3c7062474693fa704f4ff8";
    let signed1 = wbi_sign(&params, mixin_key);
    let signed2 = wbi_sign(&params, mixin_key);

    // The wts values should be very close (same second)
    let wts1 = signed1
        .iter()
        .find(|(k, _)| k == "wts")
        .map(|(_, v)| v.clone())
        .expect("wts missing");
    let wts2 = signed2
        .iter()
        .find(|(k, _)| k == "wts")
        .map(|(_, v)| v.clone())
        .expect("wts missing");
    // They should be the same if run within the same second
    assert_eq!(wts1, wts2, "wts should be same within the same second");

    // If wts is the same, w_rid must be the same too
    let w_rid1 = signed1
        .iter()
        .find(|(k, _)| k == "w_rid")
        .map(|(_, v)| v.clone())
        .expect("w_rid missing");
    let w_rid2 = signed2
        .iter()
        .find(|(k, _)| k == "w_rid")
        .map(|(_, v)| v.clone())
        .expect("w_rid missing");
    assert_eq!(
        w_rid1, w_rid2,
        "w_rid should be deterministic for same inputs"
    );
}

#[test]
fn test_nav_resp_with_wbi_img_deserialize() {
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
    let resp: types::NavResp = serde_json::from_str(json).unwrap();
    assert!(resp.data.wbi_img.is_some());
    let wbi_img = resp.data.wbi_img.unwrap();
    assert!(wbi_img.img_url.contains("7cd084941338484aae1ad9425b84077c"));
    assert!(wbi_img.sub_url.contains("4932caff0ff746eab6f01bf08b70ac45"));
}

#[test]
fn test_nav_resp_without_wbi_img_deserialize() {
    let json = r#"{
            "data": {
                "isLogin": false,
                "uname": "",
                "face": "",
                "vipStatus": 0,
                "mid": 0
            },
            "message": "0",
            "code": 0,
            "ttl": 1
        }"#;
    let resp: types::NavResp = serde_json::from_str(json).unwrap();
    assert!(resp.data.wbi_img.is_none());
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
fn test_build_cookie_header_empty_returns_none() {
    let client = BilibiliClient::new().unwrap();
    assert!(client.build_cookie_header().is_none());
}

#[test]
fn test_build_cookie_header_multiple_joined() {
    let mut cookies = HashMap::new();
    cookies.insert("SESSDATA".to_string(), "abc123".to_string());
    cookies.insert("bili_jct".to_string(), "token456".to_string());
    let client = BilibiliClient::with_cookies(cookies).unwrap();

    let header = client.build_cookie_header().unwrap();
    // Should contain both cookies joined by "; "
    assert!(header.contains("SESSDATA=abc123"));
    assert!(header.contains("bili_jct=token456"));
    assert!(header.contains("; "));
}

#[test]
fn test_build_cookie_header_sanitizes_crlf() {
    let mut cookies = HashMap::new();
    cookies.insert("evil\r\nkey".to_string(), "evil\r\nvalue".to_string());
    let client = BilibiliClient::with_cookies(cookies).unwrap();

    let header = client.build_cookie_header().unwrap();
    // CRLF characters should be stripped
    assert!(!header.contains('\r'));
    assert!(!header.contains('\n'));
    assert!(header.contains("evilkey=evilvalue"));
}

#[tokio::test]
async fn test_wbi_state_is_isolated_per_client_instance() {
    let client_a = BilibiliClient::new().unwrap();
    let client_b = BilibiliClient::new().unwrap();

    let state_a = client_a.shared_wbi_state();
    let state_b = client_b.shared_wbi_state();

    state_a.reset_for_tests().await;
    state_b.reset_for_tests().await;

    state_a.set_wbi_key("key-a".to_string()).await;
    state_b.set_wbi_key("key-b".to_string()).await;

    assert_eq!(state_a.get_valid_wbi_key().await.as_deref(), Some("key-a"));
    assert_eq!(state_b.get_valid_wbi_key().await.as_deref(), Some("key-b"));

    state_a.release_refresh_lock_on_failure_and_notify();
    state_a.release_refresh_lock_on_failure_and_notify();
    state_a.release_refresh_lock_on_failure_and_notify();

    assert!(state_a.has_exceeded_max_failures());
    assert!(!state_b.has_exceeded_max_failures());
    assert_eq!(state_a.api_call_count(), 0);
    assert_eq!(state_b.api_call_count(), 0);
}

// Note: WBI Key Refresh Coordination tests were removed as they referenced
// a non-existent WbiKeyCache struct. The WBI key caching is handled by
// instance-scoped `WbiState` shared explicitly by a `BilibiliService`.

#[test]
fn test_parse_danmaku_gift_with_huge_count_no_panic() {
    // The gift count field could exceed u32::MAX, which would cause
    // u32::try_from().expect("REASON") to panic. After the fix,
    // it should use unwrap_or(u32::MAX) instead.
    let json = serde_json::json!({
        "cmd": "SEND_GIFT",
        "data": {
            "uname": "TestUser",
            "giftName": "TestGift",
            "num": u64::from(u32::MAX) + 1  // exceeds u32
        }
    });

    // This should NOT panic after the fix
    let result = parse_danmaku_cmd("SEND_GIFT", &json);
    match result {
        DanmakuMessage::Gift { count, .. } => {
            assert_eq!(count, u32::MAX, "Overflow should clamp to u32::MAX");
        }
        _ => panic!("Expected Gift message variant"),
    }
}

#[test]
fn test_parse_danmaku_gift_with_normal_count() {
    let json = serde_json::json!({
        "cmd": "SEND_GIFT",
        "data": {
            "uname": "TestUser",
            "giftName": "TestGift",
            "num": 5
        }
    });

    let result = parse_danmaku_cmd("SEND_GIFT", &json);
    match result {
        DanmakuMessage::Gift { count, .. } => {
            assert_eq!(count, 5);
        }
        _ => panic!("Expected Gift message variant"),
    }
}

#[test]
fn test_build_auth_packet_does_not_panic_on_normal_token() {
    // build_auth_packet uses u32::try_from(packet_length).expect("REASON")
    // Normal tokens should work fine
    let packet = build_auth_packet(12345, "normal_token_value");
    assert!(!packet.is_empty());
    // The first 4 bytes encode the packet length as big-endian u32
    let len = u32::from_be_bytes([packet[0], packet[1], packet[2], packet[3]]);
    assert_eq!(len as usize, packet.len());
}

#[tokio::test]
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
async fn test_resolve_validated_danmaku_addr_accepts_public_ip_literal() {
    let guard = SsrfGuard::strict_policy();
    let addr = resolve_validated_danmaku_addr("93.184.216.34", 443, &guard)
        .await
        .expect("public IP literal should pass SSRF validation");
    assert_eq!(
        addr,
        "93.184.216.34:443".parse::<std::net::SocketAddr>().unwrap()
    );
}

#[tokio::test]
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
    // Packets less than 16 bytes should return empty Vec
    let short_data = [0u8; 15];
    let result = parse_danmaku_packet(&short_data);
    assert!(result.is_empty());
}

#[test]
fn test_parse_danmaku_packet_empty_returns_empty() {
    // Empty packet should return empty Vec
    let result = parse_danmaku_packet(&[]);
    assert!(result.is_empty());
}

#[test]
fn test_parse_danmaku_packet_invalid_zlib_returns_empty() {
    // Create a packet with operation=5 (notification) and protocol_version=2 (zlib)
    // but with invalid zlib data
    let mut packet = Vec::new();
    packet.extend_from_slice(&16u32.to_be_bytes()); // packet length
    packet.extend_from_slice(&16u16.to_be_bytes()); // header length
    packet.extend_from_slice(&2u16.to_be_bytes()); // protocol version = zlib
    packet.extend_from_slice(&5u32.to_be_bytes()); // operation = notification
    packet.extend_from_slice(&1u32.to_be_bytes()); // sequence
                                                   // Add invalid zlib data (not valid zlib compressed data)
    packet.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

    let result = parse_danmaku_packet(&packet);
    assert!(result.is_empty());
}

#[test]
fn test_parse_danmaku_packet_invalid_brotli_returns_empty() {
    // Create a packet with operation=5 (notification) and protocol_version=3 (brotli)
    // but with invalid brotli data
    let mut packet = Vec::new();
    packet.extend_from_slice(&20u32.to_be_bytes()); // packet length
    packet.extend_from_slice(&16u16.to_be_bytes()); // header length
    packet.extend_from_slice(&3u16.to_be_bytes()); // protocol version = brotli
    packet.extend_from_slice(&5u32.to_be_bytes()); // operation = notification
    packet.extend_from_slice(&1u32.to_be_bytes()); // sequence
                                                   // Add invalid brotli data
    packet.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

    let result = parse_danmaku_packet(&packet);
    assert!(result.is_empty());
}

#[test]
fn test_read_limited_danmaku_decompressed_rejects_oversized_output() {
    let oversized_len = usize::try_from(MAX_DANMAKU_DECOMPRESS_SIZE)
        .expect("danmaku decompression test limit must fit in usize")
        .checked_add(1)
        .expect("danmaku decompression test allocation length must not overflow");
    let oversized = vec![0u8; oversized_len];
    let result =
        read_limited_danmaku_decompressed(std::io::Cursor::new(oversized), "identity-test", 1);
    assert!(result.is_none());
}

#[test]
fn test_parse_danmaku_packet_unknown_protocol_version_returns_empty() {
    // Create a packet with operation=5 (notification) and unknown protocol_version
    let mut packet = Vec::new();
    packet.extend_from_slice(&20u32.to_be_bytes()); // packet length
    packet.extend_from_slice(&16u16.to_be_bytes()); // header length
    packet.extend_from_slice(&99u16.to_be_bytes()); // protocol version = unknown
    packet.extend_from_slice(&5u32.to_be_bytes()); // operation = notification
    packet.extend_from_slice(&1u32.to_be_bytes()); // sequence
    packet.extend_from_slice(&[0, 0, 0, 0]); // body

    let result = parse_danmaku_packet(&packet);
    assert!(result.is_empty());
}

#[test]
fn test_parse_danmaku_packet_valid_heartbeat() {
    // Create a valid heartbeat response packet
    let mut packet = Vec::new();
    packet.extend_from_slice(&20u32.to_be_bytes()); // packet length
    packet.extend_from_slice(&16u16.to_be_bytes()); // header length
    packet.extend_from_slice(&1u16.to_be_bytes()); // protocol version
    packet.extend_from_slice(&3u32.to_be_bytes()); // operation = heartbeat response
    packet.extend_from_slice(&1u32.to_be_bytes()); // sequence
    packet.extend_from_slice(&12345u32.to_be_bytes()); // online count

    let result = parse_danmaku_packet(&packet);
    assert_eq!(result.len(), 1);
    match &result[0] {
        DanmakuMessage::Heartbeat { online_count } => {
            assert_eq!(*online_count, 12345);
        }
        _ => panic!("Expected Heartbeat message"),
    }
}

/// Test that waiting with timeout actually times out when no notification comes.
/// This test verifies the timeout mechanism works in isolation.
#[tokio::test]
async fn test_notify_timeout_mechanism() {
    // We test that tokio::time::timeout works correctly with Notify.
    // This is a sanity check that our timeout approach is valid.
    let timeout_duration = std::time::Duration::from_millis(10);

    // Create a new Notify for this test (not the global one) to avoid interference
    let local_notify = tokio::sync::Notify::new();

    // This should timeout since we never call local_notify.notify_waiters()
    let result = tokio::time::timeout(timeout_duration, local_notify.notified()).await;
    assert!(
        result.is_err(),
        "Should timeout when no notification is sent"
    );
}

/// Test that notification arrives before timeout when sent quickly.
#[tokio::test]
async fn test_notify_arrives_before_timeout() {
    // Create a new Notify wrapped in Arc for this test to avoid interference
    use std::sync::Arc;
    let local_notify = Arc::new(tokio::sync::Notify::new());
    let timeout_duration = std::time::Duration::from_millis(100);

    // Spawn a task that waits with timeout
    let notify = Arc::clone(&local_notify);
    let wait_task = tokio::spawn(async move {
        let result = tokio::time::timeout(timeout_duration, notify.notified()).await;
        result.is_ok()
    });

    // Send notification quickly
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    local_notify.notify_waiters();

    let notification_received = wait_task.await.expect("Task should not panic");
    assert!(
        notification_received,
        "Notification should have arrived before timeout"
    );
}
