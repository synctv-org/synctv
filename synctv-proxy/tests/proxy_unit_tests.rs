//! Unit tests for pure functions in the synctv-proxy crate.

use std::collections::HashMap;
use synctv_proxy::{
    apply_provider_headers, make_absolute, percent_encode, rewrite_m3u8,
    rewrite_uri_attribute_with_count, validate_proxy_url_static,
};

// ------------------------------------------------------------------
// Helper: build a reqwest request and inspect the resulting headers
// ------------------------------------------------------------------

fn build_and_get_headers(
    url: &str,
    provider_headers: &HashMap<String, String>,
) -> reqwest::header::HeaderMap {
    let client = reqwest::Client::new();
    let request = client.get(url);
    let request = apply_provider_headers(request, url, provider_headers);
    let built = request
        .build()
        .expect("Failed to build request");
    built.headers().clone()
}

// ==================================================================
// apply_provider_headers
// ==================================================================

#[test]
fn test_default_user_agent_added_when_absent() {
    let headers = build_and_get_headers("https://example.com/video.mp4", &HashMap::new());
    let ua = headers
        .get("user-agent")
        .expect("User-Agent should be set by default");
    assert!(
        ua.to_str().unwrap().contains("Mozilla"),
        "Default User-Agent should look like a browser"
    );
}

#[test]
fn test_custom_user_agent_not_overridden() {
    let mut provider = HashMap::new();
    provider.insert("User-Agent".to_string(), "CustomAgent/1.0".to_string());
    let headers = build_and_get_headers("https://example.com/video.mp4", &provider);
    let ua = headers.get("user-agent").expect("User-Agent should exist");
    assert_eq!(ua.to_str().unwrap(), "CustomAgent/1.0");
}

#[test]
fn test_default_referer_constructed_from_url() {
    let headers =
        build_and_get_headers("https://cdn.example.com/path/to/video.mp4", &HashMap::new());
    let referer = headers.get("referer").expect("Referer should be set by default");
    assert_eq!(
        referer.to_str().unwrap(),
        "https://cdn.example.com/path/to/video.mp4"
    );
}

#[test]
fn test_custom_referer_not_overridden() {
    let mut provider = HashMap::new();
    provider.insert("Referer".to_string(), "https://custom.example.com/".to_string());
    let headers = build_and_get_headers("https://cdn.example.com/video.mp4", &provider);
    let referer = headers.get("referer").expect("Referer should exist");
    assert_eq!(referer.to_str().unwrap(), "https://custom.example.com/");
}

#[test]
fn test_unparseable_url_no_referer_crash() {
    // apply_provider_headers should not panic when url::Url::parse fails.
    // We use a valid HTTP URL with a nonsense host so reqwest can build it,
    // but url::Url::parse on a truly malformed string would skip Referer.
    // Here we test that a URL with an odd host still produces a Referer
    // derived from the URL itself (since url::Url::parse succeeds for http:// URLs).
    let client = reqwest::Client::new();
    let request = client.get("https://example.com/path");
    // Pass a malformed URL string to apply_provider_headers directly.
    // The function uses url::Url::parse internally; if it fails, no Referer is set.
    let request = apply_provider_headers(request, ":::invalid", &HashMap::new());
    let built = request.build().expect("Request should still build");
    // url::Url::parse(":::invalid") fails, so no Referer header should be set.
    assert!(
        built.headers().get("referer").is_none(),
        "Unparseable URL should not produce a Referer header"
    );
}

// ==================================================================
// rewrite_m3u8
// ==================================================================

#[test]
fn test_rewrite_m3u8_absolute_segment_unchanged() {
    let m3u8 = "#EXTM3U\nhttps://cdn.example.com/seg1.ts\n";
    let rewritten = rewrite_m3u8(
        m3u8,
        "https://origin.example.com/master.m3u8",
        "/proxy/stream",
    );
    // The absolute URL should be percent-encoded in the url= parameter
    assert!(rewritten.contains("url=https%3A%2F%2Fcdn%2Eexample%2Ecom%2Fseg1%2Ets"));
}

#[test]
fn test_rewrite_m3u8_ext_x_key_uri_rewritten() {
    let m3u8 = concat!(
        "#EXTM3U\n",
        "#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n",
        "seg1.ts\n",
    );
    let rewritten = rewrite_m3u8(
        m3u8,
        "https://cdn.example.com/path/master.m3u8",
        "/proxy/stream",
    );
    // The URI in EXT-X-KEY should be rewritten to an absolute proxied URL
    assert!(
        rewritten.contains("URI=\"/proxy/stream?url="),
        "EXT-X-KEY URI should be rewritten, got: {rewritten}"
    );
    // The key URL should be made absolute against the source base
    assert!(
        rewritten.contains("cdn%2Eexample%2Ecom"),
        "Key URL should contain the CDN host, got: {rewritten}"
    );
}

#[test]
fn test_rewrite_m3u8_ext_x_media_uri_rewritten() {
    let m3u8 = concat!(
        "#EXTM3U\n",
        "#EXT-X-MEDIA:TYPE=AUDIO,URI=\"audio/en.m3u8\"\n",
        "video.m3u8\n",
    );
    let rewritten = rewrite_m3u8(
        m3u8,
        "https://cdn.example.com/hls/master.m3u8",
        "/proxy/stream",
    );
    assert!(
        rewritten.contains("URI=\"/proxy/stream?url="),
        "EXT-X-MEDIA URI should be rewritten, got: {rewritten}"
    );
}

#[test]
fn test_rewrite_m3u8_proxy_base_with_query_uses_ampersand() {
    let m3u8 = "#EXTM3U\nseg1.ts\n";
    let rewritten = rewrite_m3u8(
        m3u8,
        "https://cdn.example.com/path/master.m3u8",
        "/proxy/stream?provider=abc",
    );
    // When proxy_base already has a '?', the separator should be '&'
    assert!(
        rewritten.contains("/proxy/stream?provider=abc&url="),
        "Should use & as separator when proxy_base has query, got: {rewritten}"
    );
}

#[test]
fn test_rewrite_m3u8_empty_playlist() {
    let m3u8 = "#EXTM3U\n#EXT-X-VERSION:3\n";
    let rewritten = rewrite_m3u8(
        m3u8,
        "https://cdn.example.com/master.m3u8",
        "/proxy/stream",
    );
    // Should not crash; output should still contain the header tags
    assert!(rewritten.contains("#EXTM3U"));
    assert!(rewritten.contains("#EXT-X-VERSION:3"));
    // No url= parameters since there are no segments
    assert!(!rewritten.contains("url="));
}

#[test]
fn test_rewrite_m3u8_max_urls_truncated() {
    // Build a playlist with 1002 segments (exceeds MAX_M3U8_URLS = 1000)
    let mut m3u8 = String::from("#EXTM3U\n");
    for i in 0..1002 {
        m3u8.push_str(&format!("seg{i}.ts\n"));
    }
    let rewritten = rewrite_m3u8(
        &m3u8,
        "https://cdn.example.com/master.m3u8",
        "/proxy/stream",
    );
    // Should contain #EXT-X-ENDLIST indicating truncation
    assert!(
        rewritten.contains("#EXT-X-ENDLIST"),
        "Playlist exceeding 1000 URLs should be truncated with #EXT-X-ENDLIST"
    );
    // Count how many url= references there are - should be exactly 1000
    let url_count = rewritten.matches("url=").count();
    assert_eq!(
        url_count, 1000,
        "Should have exactly 1000 rewritten URLs, got {url_count}"
    );
}

// ==================================================================
// percent_encode
// ==================================================================

#[test]
fn test_percent_encode_special_chars() {
    let encoded = percent_encode("https://example.com/path?key=value&foo=bar");
    assert!(encoded.contains("%3A")); // ':'
    assert!(encoded.contains("%2F")); // '/'
    assert!(encoded.contains("%3F")); // '?'
    assert!(encoded.contains("%3D")); // '='
    assert!(encoded.contains("%26")); // '&'
}

#[test]
fn test_percent_encode_empty_string() {
    assert_eq!(percent_encode(""), "");
}

#[test]
fn test_percent_encode_already_safe() {
    // NON_ALPHANUMERIC encodes everything except A-Z, a-z, 0-9.
    // Characters like -, _, ., ~ ARE encoded by this encode set.
    let input = "abcXYZ012";
    assert_eq!(percent_encode(input), input);
    // Verify that special unreserved chars are percent-encoded
    assert_eq!(percent_encode("-"), "%2D");
    assert_eq!(percent_encode("_"), "%5F");
    assert_eq!(percent_encode("."), "%2E");
    assert_eq!(percent_encode("~"), "%7E");
}

// ==================================================================
// validate_proxy_url_static
// ==================================================================

#[test]
fn test_validate_static_private_ip_blocked() {
    assert!(validate_proxy_url_static("http://192.168.1.1/path").is_err());
    assert!(validate_proxy_url_static("http://10.0.0.1/path").is_err());
    assert!(validate_proxy_url_static("http://172.16.0.1/path").is_err());
}

#[test]
fn test_validate_static_public_ip_allowed() {
    assert!(validate_proxy_url_static("https://1.1.1.1/dns-query").is_ok());
    assert!(validate_proxy_url_static("https://8.8.8.8/resolve").is_ok());
}

#[test]
fn test_validate_static_loopback_blocked() {
    assert!(validate_proxy_url_static("http://127.0.0.1/secret").is_err());
    assert!(validate_proxy_url_static("http://localhost/secret").is_err());
}

// ==================================================================
// Content-type -> media_type label mapping
// (tested indirectly through a helper that mirrors the logic)
// ==================================================================

/// Mirror of the media_type derivation logic from proxy_fetch_and_forward.
/// Extracted here to enable direct unit testing.
fn derive_media_type(content_type: &str) -> &'static str {
    if content_type.contains("mpegurl") || content_type.contains("m3u8") {
        "hls"
    } else if content_type.contains("dash") || content_type.contains("mpd") {
        "dash"
    } else if content_type.contains("video/") {
        "video"
    } else if content_type.contains("audio/") {
        "audio"
    } else if content_type.contains("octet-stream") {
        "binary"
    } else {
        "other"
    }
}

#[test]
fn test_media_type_hls() {
    assert_eq!(derive_media_type("application/vnd.apple.mpegurl"), "hls");
    // Case-sensitive: the real code checks lowercase "mpegurl" and "m3u8".
    // HTTP Content-Type values from well-behaved servers are typically lowercase.
    assert_eq!(derive_media_type("application/x-mpegurl"), "hls");
    assert_eq!(derive_media_type("application/x-m3u8"), "hls");
}

#[test]
fn test_media_type_dash() {
    assert_eq!(derive_media_type("application/dash+xml"), "dash");
    assert_eq!(derive_media_type("video/vnd.mpeg.dash.mpd"), "dash");
}

#[test]
fn test_media_type_video() {
    assert_eq!(derive_media_type("video/mp4"), "video");
    assert_eq!(derive_media_type("video/webm"), "video");
}

#[test]
fn test_media_type_audio() {
    assert_eq!(derive_media_type("audio/mpeg"), "audio");
    assert_eq!(derive_media_type("audio/aac"), "audio");
}

#[test]
fn test_media_type_binary() {
    assert_eq!(derive_media_type("application/octet-stream"), "binary");
}

// ==================================================================
// make_absolute (additional coverage)
// ==================================================================

#[test]
fn test_make_absolute_no_base_returns_raw() {
    assert_eq!(make_absolute("seg1.ts", None), "seg1.ts");
}

#[test]
fn test_make_absolute_root_relative() {
    let base = url::Url::parse("https://cdn.example.com/path/master.m3u8").unwrap();
    assert_eq!(
        make_absolute("/other/seg1.ts", Some(&base)),
        "https://cdn.example.com/other/seg1.ts"
    );
}

// ==================================================================
// rewrite_uri_attribute_with_count
// ==================================================================

#[test]
fn test_rewrite_uri_single_uri() {
    let base = url::Url::parse("https://cdn.example.com/hls/master.m3u8").unwrap();
    let (result, count) = rewrite_uri_attribute_with_count(
        "#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"",
        Some(&base),
        "/proxy",
    );
    assert_eq!(count, 1);
    assert!(result.contains("URI=\"/proxy?url="));
    assert!(result.contains("cdn%2Eexample%2Ecom"));
}

#[test]
fn test_rewrite_uri_multiple_uris() {
    // A line with two URI= attributes (unusual but possible)
    let base = url::Url::parse("https://cdn.example.com/hls/master.m3u8").unwrap();
    let line = "#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"key1.bin\",KEYFORMAT=\"urn\",URI=\"key2.bin\"";
    let (result, count) = rewrite_uri_attribute_with_count(line, Some(&base), "/proxy");
    assert_eq!(count, 2, "Should rewrite both URI attributes");
    // Both URIs should be proxied
    let url_matches: Vec<_> = result.match_indices("/proxy?url=").collect();
    assert_eq!(url_matches.len(), 2);
}

#[test]
fn test_rewrite_uri_malformed_no_closing_quote() {
    // URI=" without a closing " -- should not panic
    let (result, count) = rewrite_uri_attribute_with_count(
        "#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin",
        None,
        "/proxy",
    );
    assert_eq!(count, 0, "Malformed URI (no closing quote) should not be rewritten");
    // The malformed content should still be in the output
    assert!(result.contains("URI=\""));
    assert!(result.contains("key.bin"));
}

#[test]
fn test_rewrite_uri_no_uri_attribute() {
    // A tag line with no URI= at all
    let (result, count) = rewrite_uri_attribute_with_count(
        "#EXT-X-VERSION:3",
        None,
        "/proxy",
    );
    assert_eq!(count, 0);
    assert_eq!(result, "#EXT-X-VERSION:3");
}

// ==================================================================
// percent_encode - unicode
// ==================================================================

#[test]
fn test_percent_encode_unicode() {
    let encoded = percent_encode("hello\u{00e9}"); // hello + e-acute
    assert!(encoded.starts_with("hello"));
    // The e-acute (U+00E9) should be percent-encoded as %C3%A9 (UTF-8)
    assert!(encoded.contains("%C3%A9"));
}

#[test]
fn test_percent_encode_cjk() {
    let encoded = percent_encode("\u{4e16}\u{754c}"); // Chinese: "world"
    // Should be percent-encoded (multi-byte UTF-8)
    assert!(!encoded.contains('\u{4e16}'));
    assert!(encoded.contains('%'));
}

// ==================================================================
// validate_proxy_url_static - additional edge cases
// ==================================================================

#[test]
fn test_validate_static_empty_url_blocked() {
    assert!(validate_proxy_url_static("").is_err());
}

#[test]
fn test_validate_static_link_local_blocked() {
    assert!(validate_proxy_url_static("http://169.254.1.1/metadata").is_err());
}

#[test]
fn test_validate_static_cgnat_blocked() {
    assert!(validate_proxy_url_static("http://100.64.0.1/internal").is_err());
}

// ==================================================================
// media_type mapping - edge cases
// ==================================================================

#[test]
fn test_media_type_other() {
    assert_eq!(derive_media_type("text/html"), "other");
    assert_eq!(derive_media_type("application/json"), "other");
    assert_eq!(derive_media_type(""), "other");
}
