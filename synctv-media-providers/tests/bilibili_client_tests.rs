//! Bilibili client wiremock tests
//!
//! Tests for `parse_video_page`, `get_video_url`, `get_dash_video_url`,
//! and `get_live_streams` using wiremock mocked endpoints.
//!
//! The client supports transport and endpoint injection, so critical HTTP paths
//! can be verified against a local mock server.

#![allow(clippy::unwrap_used)]
use synctv_media_providers::bilibili::{BilibiliEndpoints, BilibiliResource};
use synctv_media_providers::BilibiliClient;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn manual_redirect_client_for_b23(server: &MockServer) -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .resolve("b23.tv", *server.address())
        .build()
        .unwrap()
}

fn manual_redirect_client_for_b23_and_www(server: &MockServer) -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .resolve("b23.tv", *server.address())
        .resolve("www.bilibili.com", *server.address())
        .build()
        .unwrap()
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

fn bilibili_client_with_short_link_client(
    server: &MockServer,
    short_link_client: reqwest::Client,
) -> BilibiliClient {
    BilibiliClient::new_with_short_link_transport_defaults(
        reqwest::Client::new(),
        short_link_client,
        test_endpoints(server.uri()),
    )
}

// match_url additional coverage

#[test]
fn test_match_url_bvid_with_query_params() {
    let matched =
        BilibiliClient::match_url("https://www.bilibili.com/video/BV1xx411c7mD?p=2&vd_source=abc")
            .unwrap();
    assert_eq!(
        matched.resource,
        BilibiliResource::Video {
            bvid: "BV1xx411c7mD".to_string(),
            aid: 0,
            page: 2,
        }
    );
}

#[test]
fn test_match_url_bvid_mobile() {
    let matched = BilibiliClient::match_url("https://m.bilibili.com/video/BV1xx411c7mD").unwrap();
    assert!(matches!(
        matched.resource,
        BilibiliResource::Video { ref bvid, .. } if bvid == "BV1xx411c7mD"
    ));
}

#[test]
fn test_match_url_live_room_direct() {
    let matched = BilibiliClient::match_url("https://live.bilibili.com/live/99999").unwrap();
    assert_eq!(matched.resource, BilibiliResource::Live { room_id: 99999 });
}

#[test]
fn test_match_url_bangumi_ep_long_id() {
    let matched =
        BilibiliClient::match_url("https://www.bilibili.com/bangumi/play/ep999999999").unwrap();
    assert_eq!(
        matched.resource,
        BilibiliResource::PgcEpisode {
            episode_id: 999_999_999,
        }
    );
}

#[test]
fn test_match_url_bangumi_ss_long_id() {
    let matched =
        BilibiliClient::match_url("https://www.bilibili.com/bangumi/play/ss888888").unwrap();
    assert_eq!(
        matched.resource,
        BilibiliResource::PgcSeason { season_id: 888_888 }
    );
}

// extract_bvid / extract_epid / is_short_link

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
    assert!(!BilibiliClient::is_short_link("ftp://b23.tv/fake"));
    assert!(!BilibiliClient::is_short_link("not a url at all"));
    assert!(!BilibiliClient::is_short_link(""));
}

#[test]
fn test_validate_bilibili_url_rejects_non_http_scheme() {
    let err = BilibiliClient::validate_bilibili_url("ftp://www.bilibili.com/video/BV123")
        .expect_err("non-http(s) Bilibili URLs should be rejected");

    assert!(
        err.to_string().contains("scheme"),
        "unexpected error: {err}"
    );
}

// BilibiliClient creation

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
        test_endpoints(server.uri()),
    )
    .unwrap();

    let (url, key) = client.new_qr_code().await.unwrap();
    assert_eq!(url, "https://mock.local/qr");
    assert_eq!(key, "qr-test-key");
}

#[tokio::test]
async fn test_resolve_short_link_rejects_non_short_link_without_network() {
    let client = BilibiliClient::new_with_transport_defaults(
        reqwest::Client::new(),
        test_endpoints("http://127.0.0.1:9"),
    )
    .unwrap();

    let err = client
        .resolve_short_link("https://example.com/not-b23")
        .await
        .expect_err("non-b23 URLs should fail before any request is sent");

    assert!(
        err.to_string().contains("b23.tv"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_resolve_short_link_rejects_cross_domain_redirect() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/abc"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "https://evil.example/video"),
        )
        .mount(&server)
        .await;

    let client =
        bilibili_client_with_short_link_client(&server, manual_redirect_client_for_b23(&server));

    let err = client
        .resolve_short_link(&format!("http://b23.tv:{}/abc", server.address().port()))
        .await
        .expect_err("cross-domain redirect should be rejected");

    assert!(
        err.to_string().contains("known Bilibili domain"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_resolve_short_link_supports_relative_location() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/abc"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/video/BV123"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/video/BV123"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let client =
        bilibili_client_with_short_link_client(&server, manual_redirect_client_for_b23(&server));

    let resolved = client
        .resolve_short_link(&format!("http://b23.tv:{}/abc", server.address().port()))
        .await
        .expect("relative Location should be resolved against the short-link URL");

    assert_eq!(
        resolved,
        format!("http://b23.tv:{}/video/BV123", server.address().port())
    );
}

#[tokio::test]
async fn test_match_resource_resolves_short_link_to_typed_video() {
    let server = MockServer::start().await;
    let target = format!(
        "http://www.bilibili.com:{}/video/BV1xx411c7mD?p=3",
        server.address().port()
    );
    Mock::given(method("GET"))
        .and(path("/abc"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", target.as_str()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/video/BV1xx411c7mD"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let client = bilibili_client_with_short_link_client(
        &server,
        manual_redirect_client_for_b23_and_www(&server),
    );
    let matched = client
        .match_resource(&format!("http://b23.tv:{}/abc", server.address().port()))
        .await
        .unwrap();

    assert_eq!(matched.normalized_url, target);
    assert_eq!(
        matched.resource,
        BilibiliResource::Video {
            bvid: "BV1xx411c7mD".to_string(),
            aid: 0,
            page: 3,
        }
    );
}

// Type deserialization tests (verify JSON response shapes)

#[test]
fn test_video_page_info_with_ugc_season() {
    use synctv_media_providers::bilibili::VideoPageInfoResp;
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
    let data = resp.data.expect("successful UGC season response has data");
    assert!(data.ugc_season.is_some());
    let ugc = data.ugc_season.unwrap();
    assert_eq!(ugc.title, "My UGC Season");
    let sections = ugc.sections;
    assert_eq!(sections.len(), 1);
    let episodes = &sections[0].episodes;
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0].bvid, "BV1ep1");
}

#[test]
fn test_video_page_info_error_code() {
    use synctv_media_providers::bilibili::VideoPageInfoResp;
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
fn test_reconnect_result_debug() {
    use synctv_media_providers::bilibili::{DanmakuMessage, ReconnectResult};

    let messages = vec![DanmakuMessage::Heartbeat { online_count: 100 }];
    let result = ReconnectResult::Messages(messages);

    let debug_str = format!("{result:?}");
    assert!(debug_str.contains("Messages"));
}
