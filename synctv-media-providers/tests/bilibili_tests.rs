//! Bilibili provider tests
//!
//! Tests for URL matching and public API surface of `BilibiliClient`.
//! Private methods (`is_wbi_stale_error`, `build_cookie_header`) are tested
//! inline in src/bilibili/client.rs via #[cfg(test)].

#![allow(clippy::unwrap_used)]
use synctv_media_providers::BilibiliClient;

#[test]
fn test_match_url_bvid_standard() {
    let (media_type, id) =
        BilibiliClient::match_url("https://www.bilibili.com/video/BV1xx411c7mD").unwrap();
    assert_eq!(media_type, "bv");
    assert_eq!(id, "BV1xx411c7mD");
}

#[test]
fn test_match_url_bangumi_ep() {
    let (media_type, id) =
        BilibiliClient::match_url("https://www.bilibili.com/bangumi/play/ep123456").unwrap();
    assert_eq!(media_type, "ep");
    assert_eq!(id, "123456");
}

#[test]
fn test_match_url_bangumi_ss() {
    let (media_type, id) =
        BilibiliClient::match_url("https://www.bilibili.com/bangumi/play/ss654321").unwrap();
    assert_eq!(media_type, "ss");
    assert_eq!(id, "654321");
}

#[test]
fn test_match_url_live_room() {
    let (media_type, id) =
        BilibiliClient::match_url("https://live.bilibili.com/live/12345").unwrap();
    assert_eq!(media_type, "live");
    assert_eq!(id, "12345");
}

#[test]
fn test_match_url_unrecognized_returns_err() {
    let result = BilibiliClient::match_url("https://example.com/not-bilibili");
    assert!(result.is_err());

    let result = BilibiliClient::match_url("https://www.bilibili.com/unknown/page");
    assert!(result.is_err());

    let result = BilibiliClient::match_url("");
    assert!(result.is_err());
}
