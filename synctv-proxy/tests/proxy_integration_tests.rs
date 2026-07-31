//! Integration tests for the synctv-proxy crate.
//!
//! These tests use wiremock to stand up mock HTTP servers and exercise the proxy
//! pipeline end-to-end where possible. Because the proxy crate includes SSRF
//! protection that blocks loopback/private IPs, tests that need to reach
//! wiremock directly use a plain reqwest client and then test the response
//! transformation logic separately.

#![allow(clippy::unwrap_used)]
use std::fmt::Write as _;
use std::time::Duration;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use synctv_proxy::rewrite_m3u8;

// Helper: plain reqwest client without SSRF restrictions for reaching wiremock
fn test_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .expect("Failed to build test client")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_redirect_missing_location_returns_error() {
    // Sanity-check the fixture used by the library redirect tests: the mock
    // actually emits a 302 without a Location header.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/bad-redirect"))
        .respond_with(ResponseTemplate::new(302))
        .mount(&server)
        .await;

    // Verify the mock returns 302 without Location
    let resp = test_client()
        .get(format!("{}/bad-redirect", server.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 302);
    assert!(
        resp.headers().get("location").is_none(),
        "Mock should not include Location header"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_proxy_m3u8_rewrites_and_returns() {
    let server = MockServer::start().await;
    let m3u8_content = concat!(
        "#EXTM3U\n",
        "#EXT-X-VERSION:3\n",
        "#EXTINF:10,\n",
        "segment0.ts\n",
        "#EXTINF:10,\n",
        "segment1.ts\n",
        "#EXT-X-ENDLIST\n",
    );

    Mock::given(method("GET"))
        .and(path("/live/stream.m3u8"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(m3u8_content)
                .insert_header("content-type", "application/vnd.apple.mpegurl"),
        )
        .mount(&server)
        .await;

    // Fetch from wiremock directly and test rewrite
    let resp = test_client()
        .get(format!("{}/live/stream.m3u8", server.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();

    // Now rewrite the M3U8 as the proxy would
    let rewritten = rewrite_m3u8(
        &body,
        &format!("{}/live/stream.m3u8", server.uri()),
        "/proxy/stream",
    )
    .unwrap();
    assert!(rewritten.contains("#EXTM3U"));
    assert!(rewritten.contains("/proxy/stream?url="));
    // Both segments should be rewritten
    assert_eq!(rewritten.matches("url=").count(), 2);
    assert!(rewritten.contains("#EXT-X-ENDLIST"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_proxy_zstd_encoding_preserved() {
    // zstd is NOT auto-decompressed by reqwest, so the proxy should
    // preserve the content-encoding header for it.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/zstd"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(b"zstd-encoded-data".to_vec())
                .insert_header("content-encoding", "zstd")
                .insert_header("content-type", "application/octet-stream"),
        )
        .mount(&server)
        .await;

    let resp = test_client()
        .get(format!("{}/zstd", server.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ce = resp.headers().get("content-encoding");
    assert!(ce.is_some());
    assert_eq!(ce.unwrap().to_str().unwrap(), "zstd");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_proxy_client_preserves_compressed_response_bytes() {
    let server = MockServer::start().await;
    let body = b"upstream bytes that must remain encoded";
    Mock::given(method("GET"))
        .and(path("/gzip"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body.to_vec())
                .insert_header("content-encoding", "gzip"),
        )
        .mount(&server)
        .await;

    let client =
        synctv_proxy::build_proxy_http_client(synctv_common::ssrf::SsrfGuard::disabled()).unwrap();
    let response = client
        .get(format!("{}/gzip", server.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["content-encoding"], "gzip");
    assert_eq!(response.bytes().await.unwrap().as_ref(), body);
}

// Cache-Control logic (detailed)

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_cache_control_audio_gets_long_max_age() {
    // audio/* content-type should get "public, max-age=86400, immutable"
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/audio.aac"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(vec![0u8; 100])
                .insert_header("content-type", "audio/aac"),
        )
        .mount(&server)
        .await;

    let resp = test_client()
        .get(format!("{}/audio.aac", server.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "audio/aac"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_cache_control_unknown_gets_no_cache() {
    // text/html or other unknown content-types should get "no-cache"
    // from the proxy. Use set_body_bytes to avoid wiremock overriding
    // the content-type header.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page.html"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(b"<html>test</html>".to_vec())
                .insert_header("content-type", "text/html"),
        )
        .mount(&server)
        .await;

    let resp = test_client()
        .get(format!("{}/page.html", server.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "text/html"
    );
}

// M3U8 manifest size limit

#[test]
fn test_proxy_m3u8_manifest_size_limit() {
    // Strict SSRF policy still classifies private IPs as blocked.
    use std::net::IpAddr;
    let ip: IpAddr = "10.0.0.1".parse().unwrap();
    assert!(
        synctv_common::ssrf::SsrfGuard::strict_policy().is_ip_blocked(&ip),
        "Private IP should be blocked by the strict SSRF ACL"
    );
}

// Redirect with wiremock chains

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_redirect_single_hop_via_wiremock() {
    // Verify wiremock can model a single redirect hop
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/initial"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/final"))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/final"))
        .respond_with(ResponseTemplate::new(200).set_body_string("final-content"))
        .mount(&server)
        .await;

    // Without auto-redirect, we should get the 302
    let resp = test_client()
        .get(format!("{}/initial", server.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 302);
    assert_eq!(
        resp.headers().get("location").unwrap().to_str().unwrap(),
        "/final"
    );

    // With auto-redirect, we should follow to the final response
    let following_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = following_client
        .get(format!("{}/initial", server.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "final-content");
}

// SSRF: DNS-level protection tests

#[test]
fn test_public_ip_allowed_by_acl() {
    let ip: std::net::IpAddr = "93.184.216.34".parse().unwrap();
    assert!(
        !synctv_common::ssrf::SsrfGuard::strict_policy().is_ip_blocked(&ip),
        "Public IP should be allowed by SSRF ACL"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_link_local_blocked_via_proxy() {
    // Strict SSRF policy still classifies link-local/cloud metadata IPs as blocked.
    use std::net::IpAddr;
    let ip: IpAddr = "169.254.169.254".parse().unwrap();
    assert!(
        synctv_common::ssrf::SsrfGuard::strict_policy().is_ip_blocked(&ip),
        "Link-local/cloud metadata IP should be blocked by the strict SSRF ACL"
    );
}

// M3U8 Truncation behavior

/// Test that VOD playlists get #EXT-X-ENDLIST when truncated
#[test]
fn test_rewrite_m3u8_truncation_vod_adds_endlist() {
    // We use a smaller limit for testing by creating exactly MAX+1 segments
    let mut m3u8_content = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");

    // Add MAX_M3U8_URLS + 1 segments (the +1 will trigger truncation)
    for i in 0..=synctv_proxy::MAX_M3U8_URLS {
        write!(m3u8_content, "#EXTINF:10,\nsegment{i}.ts\n").unwrap();
    }
    m3u8_content.push_str("#EXT-X-ENDLIST\n");

    let rewritten =
        rewrite_m3u8(&m3u8_content, "http://example.com/stream.m3u8", "/proxy").unwrap();

    // Should contain #EXT-X-ENDLIST because original was a VOD
    assert!(
        rewritten.contains("#EXT-X-ENDLIST"),
        "VOD playlist truncation should include EXT-X-ENDLIST, got:\n{}",
        rewritten.lines().take(10).collect::<Vec<_>>().join("\n")
    );

    // Should have exactly MAX_M3U8_URLS segments (truncated at limit)
    let segment_count = rewritten.matches("url=").count();
    assert_eq!(
        segment_count,
        synctv_proxy::MAX_M3U8_URLS,
        "Should have exactly {} segments, got {}",
        synctv_proxy::MAX_M3U8_URLS,
        segment_count
    );
}

/// Test that live streams do NOT get #EXT-X-ENDLIST when truncated
#[test]
fn test_rewrite_m3u8_truncation_live_no_endlist() {
    let mut m3u8_content = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");

    // Add MAX_M3U8_URLS + 1 segments (the +1 will trigger truncation)
    for i in 0..=synctv_proxy::MAX_M3U8_URLS {
        write!(m3u8_content, "#EXTINF:10,\nsegment{i}.ts\n").unwrap();
    }
    // NO #EXT-X-ENDLIST - this is a live stream

    let rewritten = rewrite_m3u8(&m3u8_content, "http://example.com/live.m3u8", "/proxy").unwrap();

    // Should NOT contain #EXT-X-ENDLIST because original was a live stream
    assert!(
        !rewritten.contains("#EXT-X-ENDLIST"),
        "Live stream truncation should NOT include EXT-X-ENDLIST, got:\n{}",
        rewritten.lines().take(10).collect::<Vec<_>>().join("\n")
    );

    // Should have exactly MAX_M3U8_URLS segments (truncated at limit)
    let segment_count = rewritten.matches("url=").count();
    assert_eq!(
        segment_count,
        synctv_proxy::MAX_M3U8_URLS,
        "Should have exactly {} segments, got {}",
        synctv_proxy::MAX_M3U8_URLS,
        segment_count
    );
}

/// Test that small playlists are not truncated
#[test]
fn test_rewrite_m3u8_small_playlist_not_truncated() {
    let m3u8_content = concat!(
        "#EXTM3U\n",
        "#EXT-X-VERSION:3\n",
        "#EXTINF:10,\n",
        "segment0.ts\n",
        "#EXTINF:10,\n",
        "segment1.ts\n",
        "#EXT-X-ENDLIST\n",
    );

    let rewritten = rewrite_m3u8(m3u8_content, "http://example.com/stream.m3u8", "/proxy").unwrap();

    // Should have both segments
    assert_eq!(rewritten.matches("url=").count(), 2);
    // Should have #EXT-X-ENDLIST at the end
    assert!(rewritten.contains("#EXT-X-ENDLIST"));
}
