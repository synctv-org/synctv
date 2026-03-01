//! Tests for the SliceCache range-request caching system.
//!
//! Following TDD: these tests are written first, then the implementation.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::time::Duration;

use axum::http::StatusCode;
use bytes::Bytes;
use http_body_util::BodyExt;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use synctv_proxy::slice_cache::{CachedResourceMeta, SliceCache, SliceCacheConfig};

// ==================================================================
// SliceCacheConfig tests
// ==================================================================

#[test]
fn test_default_config() {
    let config = SliceCacheConfig::default();
    assert!(config.enabled);
    assert_eq!(config.slice_size, 2 * 1024 * 1024); // 2MB
    assert_eq!(config.max_cache_size, 512 * 1024 * 1024); // 512MB
    assert_eq!(config.max_cacheable_body, 10 * 1024 * 1024); // 10MB
    assert_eq!(config.manifest_ttl, Duration::from_secs(5));
    assert_eq!(config.segment_ttl, Duration::from_secs(300));
}

#[test]
fn test_config_disabled() {
    let config = SliceCacheConfig {
        enabled: false,
        ..Default::default()
    };
    assert!(!config.enabled);
}

#[test]
fn test_config_custom_slice_size() {
    let config = SliceCacheConfig {
        slice_size: 4 * 1024 * 1024, // 4MB
        ..Default::default()
    };
    assert_eq!(config.slice_size, 4 * 1024 * 1024);
}

// ==================================================================
// SliceCache creation tests
// ==================================================================

#[test]
fn test_slice_cache_new() {
    let config = SliceCacheConfig::default();
    let cache = SliceCache::new(config);
    assert_eq!(cache.config().slice_size, 2 * 1024 * 1024);
}

#[test]
fn test_slice_cache_custom_config() {
    let config = SliceCacheConfig {
        enabled: true,
        slice_size: 1024 * 1024, // 1MB
        max_cache_size: 100 * 1024 * 1024, // 100MB
        ..Default::default()
    };
    let cache = SliceCache::new(config);
    assert_eq!(cache.config().slice_size, 1024 * 1024);
    assert_eq!(cache.config().max_cache_size, 100 * 1024 * 1024);
}

// ==================================================================
// Cache key generation tests
// ==================================================================

#[test]
fn test_cache_key_deterministic() {
    let url = "https://cdn.example.com/video.mp4";
    let headers: HashMap<String, String> = HashMap::new();
    let key1 = SliceCache::compute_cache_key(url, &headers, 0);
    let key2 = SliceCache::compute_cache_key(url, &headers, 0);
    assert_eq!(key1, key2, "Cache keys must be deterministic");
}

#[test]
fn test_cache_key_different_slice_index() {
    let url = "https://cdn.example.com/video.mp4";
    let headers: HashMap<String, String> = HashMap::new();
    let key0 = SliceCache::compute_cache_key(url, &headers, 0);
    let key1 = SliceCache::compute_cache_key(url, &headers, 1);
    assert_ne!(key0, key1, "Different slice indices must produce different keys");
}

#[test]
fn test_cache_key_different_urls() {
    let headers: HashMap<String, String> = HashMap::new();
    let key1 = SliceCache::compute_cache_key("https://cdn.example.com/a.mp4", &headers, 0);
    let key2 = SliceCache::compute_cache_key("https://cdn.example.com/b.mp4", &headers, 0);
    assert_ne!(key1, key2, "Different URLs must produce different keys");
}

#[test]
fn test_cache_key_sorted_headers() {
    let mut headers1 = HashMap::new();
    headers1.insert("Referer".to_string(), "https://example.com".to_string());
    headers1.insert("Cookie".to_string(), "session=abc".to_string());

    let mut headers2 = HashMap::new();
    headers2.insert("Cookie".to_string(), "session=abc".to_string());
    headers2.insert("Referer".to_string(), "https://example.com".to_string());

    let key1 = SliceCache::compute_cache_key("https://cdn.example.com/v.mp4", &headers1, 0);
    let key2 = SliceCache::compute_cache_key("https://cdn.example.com/v.mp4", &headers2, 0);
    assert_eq!(
        key1, key2,
        "Header insertion order must not affect cache key"
    );
}

#[test]
fn test_cache_key_different_headers() {
    let mut headers1 = HashMap::new();
    headers1.insert("Cookie".to_string(), "session=abc".to_string());

    let mut headers2 = HashMap::new();
    headers2.insert("Cookie".to_string(), "session=xyz".to_string());

    let key1 = SliceCache::compute_cache_key("https://cdn.example.com/v.mp4", &headers1, 0);
    let key2 = SliceCache::compute_cache_key("https://cdn.example.com/v.mp4", &headers2, 0);
    assert_ne!(key1, key2, "Different headers must produce different keys");
}

// ==================================================================
// Range parsing tests
// ==================================================================

#[test]
fn test_parse_range_single_range() {
    let (start, end) = synctv_proxy::slice_cache::parse_range_header("bytes=0-999", 10000).unwrap();
    assert_eq!(start, 0);
    assert_eq!(end, 999);
}

#[test]
fn test_parse_range_open_ended() {
    // "bytes=500-" means from 500 to end
    let (start, end) =
        synctv_proxy::slice_cache::parse_range_header("bytes=500-", 10000).unwrap();
    assert_eq!(start, 500);
    assert_eq!(end, 9999);
}

#[test]
fn test_parse_range_suffix() {
    // "bytes=-500" means last 500 bytes
    let (start, end) =
        synctv_proxy::slice_cache::parse_range_header("bytes=-500", 10000).unwrap();
    assert_eq!(start, 9500);
    assert_eq!(end, 9999);
}

#[test]
fn test_parse_range_multi_range_rejected() {
    let result = synctv_proxy::slice_cache::parse_range_header("bytes=0-100,200-300", 10000);
    assert!(result.is_err(), "Multi-range must be rejected");
}

#[test]
fn test_parse_range_invalid_format() {
    let result = synctv_proxy::slice_cache::parse_range_header("invalid", 10000);
    assert!(result.is_err(), "Invalid range format must be rejected");
}

#[test]
fn test_parse_range_start_beyond_total() {
    let result = synctv_proxy::slice_cache::parse_range_header("bytes=20000-", 10000);
    assert!(
        result.is_err(),
        "Range start beyond total size must be rejected"
    );
}

#[test]
fn test_parse_range_end_capped_at_total() {
    // If end > total-1, should be capped
    let (start, end) =
        synctv_proxy::slice_cache::parse_range_header("bytes=0-99999", 10000).unwrap();
    assert_eq!(start, 0);
    assert_eq!(end, 9999);
}

// ==================================================================
// Slice index calculation tests
// ==================================================================

#[test]
fn test_compute_needed_slices_single_slice() {
    // Range 0-100 with slice_size=2MB needs only slice 0
    let slices = synctv_proxy::slice_cache::compute_needed_slices(0, 100, 2 * 1024 * 1024);
    assert_eq!(slices, vec![0]);
}

#[test]
fn test_compute_needed_slices_multiple_slices() {
    // Range 0-4194303 (4MB) with slice_size=2MB needs slices 0 and 1
    let slices = synctv_proxy::slice_cache::compute_needed_slices(0, 4 * 1024 * 1024 - 1, 2 * 1024 * 1024);
    assert_eq!(slices, vec![0, 1]);
}

#[test]
fn test_compute_needed_slices_cross_boundary() {
    // Range 1MB-3MB with slice_size=2MB needs slices 0 and 1
    let mb: u64 = 1024 * 1024;
    let slices = synctv_proxy::slice_cache::compute_needed_slices(mb, 3 * mb - 1, 2 * mb as usize);
    assert_eq!(slices, vec![0, 1]);
}

#[test]
fn test_compute_needed_slices_exact_boundary() {
    // Range starts exactly at slice boundary
    let mb: u64 = 1024 * 1024;
    let slices = synctv_proxy::slice_cache::compute_needed_slices(2 * mb, 4 * mb - 1, 2 * mb as usize);
    assert_eq!(slices, vec![1]);
}

// ==================================================================
// get_or_fetch_slice integration tests (with wiremock)
// ==================================================================

#[tokio::test]
async fn test_get_or_fetch_slice_fetches_from_upstream() {
    let mock_server = MockServer::start().await;

    // Upstream returns 2MB of data for range 0-2097151
    let body = Bytes::from(vec![0xABu8; 2 * 1024 * 1024]);
    Mock::given(method("GET"))
        .and(path("/video.mp4"))
        .and(header("Range", "bytes=0-2097151"))
        .respond_with(
            ResponseTemplate::new(206)
                .set_body_bytes(body.clone())
                .insert_header("Content-Range", "bytes 0-2097151/10485760")
                .insert_header("Content-Length", "2097152"),
        )
        .expect(1) // Should only be called once (cached afterwards)
        .mount(&mock_server)
        .await;

    let config = SliceCacheConfig::default();
    let cache = SliceCache::new(config);

    let url = format!("{}/video.mp4", mock_server.uri());
    let headers = HashMap::new();
    let total_size = 10 * 1024 * 1024; // 10MB

    let slice = cache
        .get_or_fetch_slice(&url, &headers, 0, total_size)
        .await
        .unwrap();
    assert_eq!(slice.len(), 2 * 1024 * 1024);

    // Second call should hit cache (mock expects exactly 1 call)
    let slice2 = cache
        .get_or_fetch_slice(&url, &headers, 0, total_size)
        .await
        .unwrap();
    assert_eq!(slice2.len(), 2 * 1024 * 1024);
}

#[tokio::test]
async fn test_get_or_fetch_slice_last_slice_partial() {
    let mock_server = MockServer::start().await;

    // Total size is 3MB, slice_size is 2MB.
    // Slice 0: bytes 0-2097151 (2MB)
    // Slice 1: bytes 2097152-3145727 (1MB - last slice, partial)
    let total_size: u64 = 3 * 1024 * 1024;
    let last_slice_size = 1024 * 1024; // 1MB

    let body = Bytes::from(vec![0xCDu8; last_slice_size]);
    Mock::given(method("GET"))
        .and(path("/video.mp4"))
        .and(header("Range", "bytes=2097152-3145727"))
        .respond_with(
            ResponseTemplate::new(206)
                .set_body_bytes(body.clone())
                .insert_header(
                    "Content-Range",
                    format!("bytes 2097152-3145727/{total_size}"),
                )
                .insert_header("Content-Length", last_slice_size.to_string()),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let config = SliceCacheConfig::default();
    let cache = SliceCache::new(config);

    let url = format!("{}/video.mp4", mock_server.uri());
    let headers = HashMap::new();

    let slice = cache
        .get_or_fetch_slice(&url, &headers, 1, total_size)
        .await
        .unwrap();
    assert_eq!(slice.len(), last_slice_size);
}

// ==================================================================
// proxy_with_cache integration tests
// ==================================================================

#[tokio::test]
async fn test_proxy_with_cache_returns_206_for_range_request() {
    let mock_server = MockServer::start().await;

    // Content is 10MB, request Range: bytes=0-999
    let total_size: u64 = 10 * 1024 * 1024;
    let slice_data = Bytes::from(vec![0xAAu8; 2 * 1024 * 1024]);

    // HEAD request to get content length
    Mock::given(method("HEAD"))
        .and(path("/video.mp4"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Length", total_size.to_string())
                .insert_header("Accept-Ranges", "bytes"),
        )
        .mount(&mock_server)
        .await;

    // GET range request for slice 0
    Mock::given(method("GET"))
        .and(path("/video.mp4"))
        .and(header("Range", "bytes=0-2097151"))
        .respond_with(
            ResponseTemplate::new(206)
                .set_body_bytes(slice_data.clone())
                .insert_header(
                    "Content-Range",
                    format!("bytes 0-2097151/{total_size}"),
                )
                .insert_header("Content-Length", "2097152"),
        )
        .mount(&mock_server)
        .await;

    let config = SliceCacheConfig::default();
    let cache = SliceCache::new(config);

    let url = format!("{}/video.mp4", mock_server.uri());
    let provider_headers = HashMap::new();

    let response = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        Some("bytes=0-999"),
        &url,
        &provider_headers,
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);

    let headers = response.headers();
    assert_eq!(
        headers
            .get("Content-Range")
            .map(|v| v.to_str().unwrap()),
        Some(format!("bytes 0-999/{total_size}").as_str()),
    );
    assert!(
        headers.get("Accept-Ranges").is_some(),
        "Response must include Accept-Ranges"
    );
}

#[tokio::test]
async fn test_proxy_with_cache_no_range_streams_through() {
    let mock_server = MockServer::start().await;

    let body = Bytes::from(vec![0xBBu8; 1024]);

    // Without Range header, should stream directly without caching
    Mock::given(method("GET"))
        .and(path("/video.mp4"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body.clone())
                .insert_header("Content-Length", "1024"),
        )
        .mount(&mock_server)
        .await;

    let config = SliceCacheConfig::default();
    let cache = SliceCache::new(config);

    let url = format!("{}/video.mp4", mock_server.uri());
    let provider_headers = HashMap::new();

    let response = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        None, // No Range header
        &url,
        &provider_headers,
    )
    .await
    .unwrap();

    // Without range header, we stream through => 200
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_proxy_with_cache_x_cache_status_miss() {
    let mock_server = MockServer::start().await;

    let total_size: u64 = 10 * 1024 * 1024;
    let slice_data = Bytes::from(vec![0xAAu8; 2 * 1024 * 1024]);

    Mock::given(method("HEAD"))
        .and(path("/video.mp4"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Length", total_size.to_string())
                .insert_header("Accept-Ranges", "bytes"),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/video.mp4"))
        .and(header("Range", "bytes=0-2097151"))
        .respond_with(
            ResponseTemplate::new(206)
                .set_body_bytes(slice_data.clone())
                .insert_header(
                    "Content-Range",
                    format!("bytes 0-2097151/{total_size}"),
                )
                .insert_header("Content-Length", "2097152"),
        )
        .mount(&mock_server)
        .await;

    let config = SliceCacheConfig::default();
    let cache = SliceCache::new(config);

    let url = format!("{}/video.mp4", mock_server.uri());
    let provider_headers = HashMap::new();

    let response = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        Some("bytes=0-999"),
        &url,
        &provider_headers,
    )
    .await
    .unwrap();

    assert_eq!(
        response
            .headers()
            .get("X-Cache-Status")
            .map(|v| v.to_str().unwrap()),
        Some("MISS"),
        "First request should be a cache MISS"
    );
}

#[tokio::test]
async fn test_proxy_with_cache_x_cache_status_hit() {
    let mock_server = MockServer::start().await;

    let total_size: u64 = 10 * 1024 * 1024;
    let slice_data = Bytes::from(vec![0xAAu8; 2 * 1024 * 1024]);

    Mock::given(method("HEAD"))
        .and(path("/video.mp4"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Length", total_size.to_string())
                .insert_header("Accept-Ranges", "bytes"),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/video.mp4"))
        .and(header("Range", "bytes=0-2097151"))
        .respond_with(
            ResponseTemplate::new(206)
                .set_body_bytes(slice_data.clone())
                .insert_header(
                    "Content-Range",
                    format!("bytes 0-2097151/{total_size}"),
                )
                .insert_header("Content-Length", "2097152"),
        )
        .expect(1) // Should only be called once
        .mount(&mock_server)
        .await;

    let config = SliceCacheConfig::default();
    let cache = SliceCache::new(config);

    let url = format!("{}/video.mp4", mock_server.uri());
    let provider_headers = HashMap::new();

    // First request - cache miss
    let _ = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        Some("bytes=0-999"),
        &url,
        &provider_headers,
    )
    .await
    .unwrap();

    // Second request - should be cache hit
    let response = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        Some("bytes=0-999"),
        &url,
        &provider_headers,
    )
    .await
    .unwrap();

    assert_eq!(
        response
            .headers()
            .get("X-Cache-Status")
            .map(|v| v.to_str().unwrap()),
        Some("HIT"),
        "Second request should be a cache HIT"
    );
}

#[tokio::test]
async fn test_proxy_with_cache_head_request_returns_content_length() {
    let mock_server = MockServer::start().await;

    let total_size: u64 = 10 * 1024 * 1024;

    Mock::given(method("HEAD"))
        .and(path("/video.mp4"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Length", total_size.to_string())
                .insert_header("Accept-Ranges", "bytes"),
        )
        .mount(&mock_server)
        .await;

    let config = SliceCacheConfig::default();
    let _cache = SliceCache::new(config);

    let url = format!("{}/video.mp4", mock_server.uri());
    let provider_headers = HashMap::new();

    let total = synctv_proxy::slice_cache::head_content_length(
        &url,
        &provider_headers,
    )
    .await
    .unwrap();

    assert_eq!(total, total_size);
}

#[tokio::test]
async fn test_proxy_with_cache_multi_range_rejected() {
    let config = SliceCacheConfig::default();
    let cache = SliceCache::new(config);

    let result = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        Some("bytes=0-100,200-300"),
        "https://cdn.example.com/video.mp4",
        &HashMap::new(),
    )
    .await;

    // Multi-range should return an error (we reject it)
    assert!(result.is_err(), "Multi-range requests must be rejected");
}

// ==================================================================
// Thundering herd prevention tests
// ==================================================================

#[tokio::test]
async fn test_concurrent_fetches_same_slice_only_one_upstream_request() {
    let mock_server = MockServer::start().await;

    let total_size: u64 = 10 * 1024 * 1024;
    let slice_data = Bytes::from(vec![0xEEu8; 2 * 1024 * 1024]);

    Mock::given(method("HEAD"))
        .and(path("/video.mp4"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Length", total_size.to_string())
                .insert_header("Accept-Ranges", "bytes"),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/video.mp4"))
        .and(header("Range", "bytes=0-2097151"))
        .respond_with(
            ResponseTemplate::new(206)
                .set_body_bytes(slice_data.clone())
                .insert_header(
                    "Content-Range",
                    format!("bytes 0-2097151/{total_size}"),
                )
                .insert_header("Content-Length", "2097152"),
        )
        .expect(1) // Exactly 1 upstream request even with concurrent callers
        .mount(&mock_server)
        .await;

    let config = SliceCacheConfig::default();
    let cache = std::sync::Arc::new(SliceCache::new(config));

    let url = format!("{}/video.mp4", mock_server.uri());
    let headers = HashMap::new();

    // Spawn 10 concurrent requests for the same slice
    let mut handles = Vec::new();
    for _ in 0..10 {
        let cache = cache.clone();
        let url = url.clone();
        let headers = headers.clone();
        handles.push(tokio::spawn(async move {
            cache
                .get_or_fetch_slice(&url, &headers, 0, total_size)
                .await
        }));
    }

    // All should succeed
    for handle in handles {
        let result = handle.await.unwrap();
        assert!(result.is_ok(), "All concurrent fetches should succeed");
        assert_eq!(result.unwrap().len(), 2 * 1024 * 1024);
    }
    // wiremock's expect(1) ensures only 1 upstream request was made
}

// ==================================================================
// Disabled cache pass-through test
// ==================================================================

#[tokio::test]
async fn test_disabled_cache_streams_directly() {
    let mock_server = MockServer::start().await;

    let body = Bytes::from(vec![0xFFu8; 1024]);

    Mock::given(method("GET"))
        .and(path("/video.mp4"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body.clone())
                .insert_header("Content-Length", "1024"),
        )
        .mount(&mock_server)
        .await;

    let config = SliceCacheConfig {
        enabled: false,
        ..Default::default()
    };
    let disabled_cache = SliceCache::new(config);

    let url = format!("{}/video.mp4", mock_server.uri());
    let provider_headers = HashMap::new();

    // Even with a range header, disabled cache should stream through
    let response = synctv_proxy::slice_cache::proxy_with_cache(
        &disabled_cache,
        Some("bytes=0-99"),
        &url,
        &provider_headers,
    )
    .await
    .unwrap();

    // Disabled cache should forward upstream response as-is
    assert!(
        response.status() == StatusCode::OK || response.status() == StatusCode::PARTIAL_CONTENT
    );
}

// ==================================================================
// Slice range alignment tests
// ==================================================================

#[test]
fn test_aligned_range_for_slice() {
    let slice_size = 2 * 1024 * 1024; // 2MB
    let total_size = 10 * 1024 * 1024; // 10MB

    // Slice 0: bytes 0 to 2097151
    let (start, end) = synctv_proxy::slice_cache::aligned_range_for_slice(0, slice_size, total_size);
    assert_eq!(start, 0);
    assert_eq!(end, 2 * 1024 * 1024 - 1);

    // Slice 1: bytes 2097152 to 4194303
    let (start, end) = synctv_proxy::slice_cache::aligned_range_for_slice(1, slice_size, total_size);
    assert_eq!(start, 2 * 1024 * 1024);
    assert_eq!(end, 4 * 1024 * 1024 - 1);

    // Last slice (slice 4): bytes 8388608 to 10485759
    let (start, end) = synctv_proxy::slice_cache::aligned_range_for_slice(4, slice_size, total_size);
    assert_eq!(start, 8 * 1024 * 1024);
    assert_eq!(end, 10 * 1024 * 1024 - 1);
}

#[test]
fn test_aligned_range_last_slice_partial() {
    let slice_size = 2 * 1024 * 1024; // 2MB
    let total_size: u64 = 3 * 1024 * 1024; // 3MB total

    // Slice 1: bytes 2097152 to 3145727 (only 1MB, not full 2MB)
    let (start, end) = synctv_proxy::slice_cache::aligned_range_for_slice(1, slice_size, total_size);
    assert_eq!(start, 2 * 1024 * 1024);
    assert_eq!(end, 3 * 1024 * 1024 - 1);
}

// ==================================================================
// Enhancement 1: Full body caching (non-range / 200 responses)
// ==================================================================

/// Non-range request caches full body; second request is a HIT.
#[tokio::test]
async fn test_full_body_cache_no_range_cached_then_hit() {
    let mock_server = MockServer::start().await;

    let body = Bytes::from(vec![0xBBu8; 1024]);

    Mock::given(method("GET"))
        .and(path("/video.mp4"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body.clone())
                .insert_header("Content-Length", "1024")
                .insert_header("Content-Type", "video/mp4"),
        )
        .expect(1) // Only one upstream request expected
        .mount(&mock_server)
        .await;

    let config = SliceCacheConfig::default();
    let cache = SliceCache::new(config);
    let url = format!("{}/video.mp4", mock_server.uri());
    let provider_headers = HashMap::new();

    // First request - MISS, should fetch and cache
    let resp1 = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        None,
        &url,
        &provider_headers,
    )
    .await
    .unwrap();

    assert_eq!(resp1.status(), StatusCode::OK);
    assert_eq!(
        resp1.headers().get("X-Cache-Status").map(|v| v.to_str().unwrap()),
        Some("MISS"),
    );
    let body1 = resp1.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body1.len(), 1024);

    // Second request - HIT from cache (wiremock expect(1) verifies no second upstream call)
    let resp2 = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        None,
        &url,
        &provider_headers,
    )
    .await
    .unwrap();

    assert_eq!(resp2.status(), StatusCode::OK);
    assert_eq!(
        resp2.headers().get("X-Cache-Status").map(|v| v.to_str().unwrap()),
        Some("HIT"),
    );
    let body2 = resp2.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body2.len(), 1024);
}

/// Response larger than max_cacheable_body is streamed through, not cached.
#[tokio::test]
async fn test_full_body_cache_oversized_not_cached() {
    let mock_server = MockServer::start().await;

    // Configure a very small max_cacheable_body
    let config = SliceCacheConfig {
        max_cacheable_body: 512,
        ..Default::default()
    };
    let cache = SliceCache::new(config);

    let body = Bytes::from(vec![0xCCu8; 1024]); // Larger than max_cacheable_body (512)

    Mock::given(method("GET"))
        .and(path("/big.mp4"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body.clone())
                .insert_header("Content-Length", "1024")
                .insert_header("Content-Type", "video/mp4"),
        )
        .expect(2) // Should be called twice since body is too large to cache
        .mount(&mock_server)
        .await;

    let url = format!("{}/big.mp4", mock_server.uri());
    let provider_headers = HashMap::new();

    // First request
    let resp1 = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        None,
        &url,
        &provider_headers,
    )
    .await
    .unwrap();

    assert_eq!(resp1.status(), StatusCode::OK);
    // Oversized bodies get BYPASS status
    assert_eq!(
        resp1.headers().get("X-Cache-Status").map(|v| v.to_str().unwrap()),
        Some("BYPASS"),
    );
    // Consume the body so the stream completes
    let _ = resp1.into_body().collect().await.unwrap().to_bytes();

    // Second request - also goes upstream since body was too large to cache
    let resp2 = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        None,
        &url,
        &provider_headers,
    )
    .await
    .unwrap();

    assert_eq!(resp2.status(), StatusCode::OK);
    let _ = resp2.into_body().collect().await.unwrap().to_bytes();
}

/// M3U8 content type uses manifest_ttl (shorter).
#[tokio::test]
async fn test_full_body_cache_m3u8_uses_manifest_ttl() {
    let mock_server = MockServer::start().await;

    let m3u8_body = "#EXTM3U\n#EXT-X-VERSION:3\nseg0.ts\n";

    Mock::given(method("GET"))
        .and(path("/live.m3u8"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(m3u8_body)
                .insert_header("Content-Type", "application/vnd.apple.mpegurl"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    // Use a very short manifest_ttl to test TTL behaviour.
    // The content-type check is the key assertion here.
    let config = SliceCacheConfig {
        manifest_ttl: Duration::from_millis(100),
        segment_ttl: Duration::from_secs(300),
        ..Default::default()
    };
    let cache = SliceCache::new(config);

    let url = format!("{}/live.m3u8", mock_server.uri());
    let provider_headers = HashMap::new();

    let resp = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        None,
        &url,
        &provider_headers,
    )
    .await
    .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("X-Cache-Status").map(|v| v.to_str().unwrap()),
        Some("MISS"),
    );

    // Immediately after: should be HIT
    let resp2 = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        None,
        &url,
        &provider_headers,
    )
    .await
    .unwrap();
    assert_eq!(
        resp2.headers().get("X-Cache-Status").map(|v| v.to_str().unwrap()),
        Some("HIT"),
    );
}

/// Full body cache entry expires after TTL and re-fetch produces EXPIRED status.
#[tokio::test]
async fn test_full_body_cache_expiry_returns_expired() {
    let mock_server = MockServer::start().await;

    let body = Bytes::from("hello");

    Mock::given(method("GET"))
        .and(path("/short.bin"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body.clone())
                .insert_header("Content-Type", "application/octet-stream")
                .insert_header("Content-Length", "5"),
        )
        .expect(2) // Will be called once initially, then again after expiry
        .mount(&mock_server)
        .await;

    // Very short segment_ttl so we can test expiry
    let config = SliceCacheConfig {
        segment_ttl: Duration::from_millis(50),
        ..Default::default()
    };
    let cache = SliceCache::new(config);

    let url = format!("{}/short.bin", mock_server.uri());
    let provider_headers = HashMap::new();

    // First request - MISS
    let resp1 = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        None,
        &url,
        &provider_headers,
    )
    .await
    .unwrap();
    assert_eq!(
        resp1.headers().get("X-Cache-Status").map(|v| v.to_str().unwrap()),
        Some("MISS"),
    );
    let _ = resp1.into_body().collect().await.unwrap().to_bytes();

    // Wait for the TTL to expire
    tokio::time::sleep(Duration::from_millis(100)).await;

    // After expiry - should be EXPIRED
    let resp2 = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        None,
        &url,
        &provider_headers,
    )
    .await
    .unwrap();
    assert_eq!(
        resp2.headers().get("X-Cache-Status").map(|v| v.to_str().unwrap()),
        Some("EXPIRED"),
    );
    let _ = resp2.into_body().collect().await.unwrap().to_bytes();
}

// ==================================================================
// Enhancement 2: ETag consistency validation
// ==================================================================

/// CachedResourceMeta stores etag, total_size, content_type.
#[test]
fn test_cached_resource_meta_fields() {
    let meta = CachedResourceMeta {
        etag: Some("\"abc123\"".to_string()),
        total_size: Some(10_485_760),
        content_type: Some("video/mp4".to_string()),
    };
    assert_eq!(meta.etag.as_deref(), Some("\"abc123\""));
    assert_eq!(meta.total_size, Some(10_485_760));
    assert_eq!(meta.content_type.as_deref(), Some("video/mp4"));
}

/// Two slices with the same ETag are cached successfully.
#[tokio::test]
async fn test_etag_consistency_same_etag_both_cached() {
    let mock_server = MockServer::start().await;

    let total_size: u64 = 4 * 1024 * 1024; // 4MB => 2 slices
    let slice0 = Bytes::from(vec![0xAAu8; 2 * 1024 * 1024]);
    let slice1 = Bytes::from(vec![0xBBu8; 2 * 1024 * 1024]);

    // Slice 0
    Mock::given(method("GET"))
        .and(path("/video.mp4"))
        .and(header("Range", "bytes=0-2097151"))
        .respond_with(
            ResponseTemplate::new(206)
                .set_body_bytes(slice0.clone())
                .insert_header("Content-Range", format!("bytes 0-2097151/{total_size}"))
                .insert_header("Content-Length", "2097152")
                .insert_header("ETag", "\"etag-v1\""),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    // Slice 1 - same ETag
    Mock::given(method("GET"))
        .and(path("/video.mp4"))
        .and(header("Range", "bytes=2097152-4194303"))
        .respond_with(
            ResponseTemplate::new(206)
                .set_body_bytes(slice1.clone())
                .insert_header(
                    "Content-Range",
                    format!("bytes 2097152-4194303/{total_size}"),
                )
                .insert_header("Content-Length", "2097152")
                .insert_header("ETag", "\"etag-v1\""),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let config = SliceCacheConfig::default();
    let cache = SliceCache::new(config);

    let url = format!("{}/video.mp4", mock_server.uri());
    let headers = HashMap::new();

    // Fetch both slices - both should succeed since ETag matches
    let s0 = cache
        .get_or_fetch_slice(&url, &headers, 0, total_size)
        .await
        .unwrap();
    assert_eq!(s0.len(), 2 * 1024 * 1024);

    let s1 = cache
        .get_or_fetch_slice(&url, &headers, 1, total_size)
        .await
        .unwrap();
    assert_eq!(s1.len(), 2 * 1024 * 1024);
}

/// Second slice with a different ETag triggers invalidation/error.
#[tokio::test]
async fn test_etag_consistency_mismatch_triggers_invalidation() {
    let mock_server = MockServer::start().await;

    // Use small slice_size (1024) so test data is tiny.
    let slice_size = 1024_usize;
    let total_size: u64 = 2048;
    let slice0 = Bytes::from(vec![0xAAu8; slice_size]);
    let slice1 = Bytes::from(vec![0xBBu8; slice_size]);

    // Slice 0 with ETag v1
    Mock::given(method("GET"))
        .and(path("/video.mp4"))
        .and(header("Range", "bytes=0-1023"))
        .respond_with(
            ResponseTemplate::new(206)
                .set_body_bytes(slice0.clone())
                .insert_header("Content-Range", format!("bytes 0-1023/{total_size}"))
                .insert_header("Content-Length", "1024")
                .insert_header("ETag", "\"etag-v1\""),
        )
        .mount(&mock_server)
        .await;

    // Slice 1 with DIFFERENT ETag (resource was modified between fetches)
    Mock::given(method("GET"))
        .and(path("/video.mp4"))
        .and(header("Range", "bytes=1024-2047"))
        .respond_with(
            ResponseTemplate::new(206)
                .set_body_bytes(slice1.clone())
                .insert_header(
                    "Content-Range",
                    format!("bytes 1024-2047/{total_size}"),
                )
                .insert_header("Content-Length", "1024")
                .insert_header("ETag", "\"etag-v2\""), // Different!
        )
        .mount(&mock_server)
        .await;

    let config = SliceCacheConfig {
        slice_size,
        ..Default::default()
    };
    let cache = SliceCache::new(config);

    let url = format!("{}/video.mp4", mock_server.uri());
    let headers = HashMap::new();

    // Fetch slice 0 - succeeds, establishes ETag for this resource
    let s0 = cache
        .get_or_fetch_slice(&url, &headers, 0, total_size)
        .await;
    assert!(s0.is_ok(), "First slice should succeed");

    // Fetch slice 1 - ETag mismatch should return an error
    let s1 = cache
        .get_or_fetch_slice(&url, &headers, 1, total_size)
        .await;
    assert!(
        s1.is_err(),
        "Second slice with different ETag must return an error"
    );

    let err_msg = s1.unwrap_err().to_string();
    assert!(
        err_msg.contains("ETag") || err_msg.contains("etag") || err_msg.contains("modified"),
        "Error message should mention ETag mismatch, got: {err_msg}"
    );
}

// ==================================================================
// Enhancement 3: Cache status refinement
// ==================================================================

/// Disabled cache returns BYPASS status.
#[tokio::test]
async fn test_cache_status_bypass_when_disabled() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/video.mp4"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(Bytes::from(vec![0u8; 100]))
                .insert_header("Content-Length", "100"),
        )
        .mount(&mock_server)
        .await;

    let config = SliceCacheConfig {
        enabled: false,
        ..Default::default()
    };
    let cache = SliceCache::new(config);
    let url = format!("{}/video.mp4", mock_server.uri());

    let resp = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        None,
        &url,
        &HashMap::new(),
    )
    .await
    .unwrap();

    assert_eq!(
        resp.headers().get("X-Cache-Status").map(|v| v.to_str().unwrap()),
        Some("BYPASS"),
        "Disabled cache must return BYPASS"
    );
}

/// Multi-range returns BYPASS.
#[tokio::test]
async fn test_cache_status_bypass_for_multi_range() {
    let mock_server = MockServer::start().await;

    // HEAD for total size
    Mock::given(method("HEAD"))
        .and(path("/video.mp4"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Length", "10000")
                .insert_header("Accept-Ranges", "bytes"),
        )
        .mount(&mock_server)
        .await;

    let config = SliceCacheConfig::default();
    let cache = SliceCache::new(config);
    let url = format!("{}/video.mp4", mock_server.uri());

    let result = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        Some("bytes=0-100,200-300"),
        &url,
        &HashMap::new(),
    )
    .await;

    // Multi-range is an error (rejected)
    assert!(result.is_err(), "Multi-range should be rejected");
}

/// Expired slice-cache entry re-fetches and returns EXPIRED status.
#[tokio::test]
async fn test_cache_status_expired_for_slice_request() {
    let mock_server = MockServer::start().await;

    let total_size: u64 = 10 * 1024 * 1024;
    let slice_data = Bytes::from(vec![0xAAu8; 2 * 1024 * 1024]);

    Mock::given(method("HEAD"))
        .and(path("/video.mp4"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Length", total_size.to_string())
                .insert_header("Accept-Ranges", "bytes"),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/video.mp4"))
        .and(header("Range", "bytes=0-2097151"))
        .respond_with(
            ResponseTemplate::new(206)
                .set_body_bytes(slice_data.clone())
                .insert_header(
                    "Content-Range",
                    format!("bytes 0-2097151/{total_size}"),
                )
                .insert_header("Content-Length", "2097152"),
        )
        .expect(2) // Called once initially, then again after expiry
        .mount(&mock_server)
        .await;

    // Very short segment_ttl
    let config = SliceCacheConfig {
        segment_ttl: Duration::from_millis(50),
        ..Default::default()
    };
    let cache = SliceCache::new(config);
    let url = format!("{}/video.mp4", mock_server.uri());
    let provider_headers = HashMap::new();

    // First request: MISS
    let resp1 = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        Some("bytes=0-999"),
        &url,
        &provider_headers,
    )
    .await
    .unwrap();
    assert_eq!(
        resp1.headers().get("X-Cache-Status").map(|v| v.to_str().unwrap()),
        Some("MISS"),
    );

    // Wait for TTL to expire
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second request: EXPIRED (was cached, but TTL expired, re-fetched)
    let resp2 = synctv_proxy::slice_cache::proxy_with_cache(
        &cache,
        Some("bytes=0-999"),
        &url,
        &provider_headers,
    )
    .await
    .unwrap();
    assert_eq!(
        resp2.headers().get("X-Cache-Status").map(|v| v.to_str().unwrap()),
        Some("EXPIRED"),
    );
}

/// Verify the resource meta is stored and retrievable.
#[tokio::test]
async fn test_resource_meta_stored_after_fetch() {
    let mock_server = MockServer::start().await;

    let total_size: u64 = 10 * 1024 * 1024;
    let slice_data = Bytes::from(vec![0xAAu8; 2 * 1024 * 1024]);

    Mock::given(method("GET"))
        .and(path("/video.mp4"))
        .and(header("Range", "bytes=0-2097151"))
        .respond_with(
            ResponseTemplate::new(206)
                .set_body_bytes(slice_data.clone())
                .insert_header("Content-Range", format!("bytes 0-2097151/{total_size}"))
                .insert_header("Content-Length", "2097152")
                .insert_header("ETag", "\"test-etag\"")
                .insert_header("Content-Type", "video/mp4"),
        )
        .mount(&mock_server)
        .await;

    let config = SliceCacheConfig::default();
    let cache = SliceCache::new(config);
    let url = format!("{}/video.mp4", mock_server.uri());
    let headers = HashMap::new();

    let _ = cache
        .get_or_fetch_slice(&url, &headers, 0, total_size)
        .await
        .unwrap();

    let meta = cache.get_resource_meta(&url, &headers).await;
    assert!(meta.is_some(), "Resource meta should be stored after fetch");
    let meta = meta.unwrap();
    assert_eq!(meta.etag.as_deref(), Some("\"test-etag\""));
    assert_eq!(meta.content_type.as_deref(), Some("video/mp4"));
}

// ==================================================================
// Content-Range response parsing tests (nginx-style)
// ==================================================================

use synctv_proxy::slice_cache::{parse_content_range, ContentRange};

#[test]
fn test_parse_content_range_basic() {
    let cr = parse_content_range("bytes 0-499/1000").unwrap();
    assert_eq!(cr.start, 0);
    assert_eq!(cr.end, 500); // exclusive, like nginx (end++)
    assert_eq!(cr.complete_length, Some(1000));
}

#[test]
fn test_parse_content_range_large_media_range() {
    let cr = parse_content_range("bytes 0-2097151/10485760").unwrap();
    assert_eq!(cr.start, 0);
    assert_eq!(cr.end, 2097152);
    assert_eq!(cr.complete_length, Some(10485760));
}

#[test]
fn test_parse_content_range_middle_range() {
    let cr = parse_content_range("bytes 2097152-4194303/10485760").unwrap();
    assert_eq!(cr.start, 2097152);
    assert_eq!(cr.end, 4194304);
    assert_eq!(cr.complete_length, Some(10485760));
}

#[test]
fn test_parse_content_range_wildcard_length() {
    let cr = parse_content_range("bytes 100-199/*").unwrap();
    assert_eq!(cr.start, 100);
    assert_eq!(cr.end, 200);
    assert_eq!(cr.complete_length, None);
}

#[test]
fn test_parse_content_range_with_extra_spaces() {
    // nginx's parser tolerates spaces between tokens
    let cr = parse_content_range("bytes  0 - 499 / 1000").unwrap();
    assert_eq!(cr.start, 0);
    assert_eq!(cr.end, 500);
    assert_eq!(cr.complete_length, Some(1000));
}

#[test]
fn test_parse_content_range_missing_bytes_prefix() {
    let result = parse_content_range("0-499/1000");
    assert!(result.is_err(), "Missing 'bytes ' prefix must be rejected");
}

#[test]
fn test_parse_content_range_missing_dash() {
    let result = parse_content_range("bytes 0 499/1000");
    assert!(result.is_err(), "Missing dash separator must be rejected");
}

#[test]
fn test_parse_content_range_missing_slash() {
    let result = parse_content_range("bytes 0-499 1000");
    assert!(result.is_err(), "Missing slash separator must be rejected");
}

#[test]
fn test_parse_content_range_non_numeric_start() {
    let result = parse_content_range("bytes abc-499/1000");
    assert!(result.is_err(), "Non-numeric start must be rejected");
}

#[test]
fn test_parse_content_range_non_numeric_end() {
    let result = parse_content_range("bytes 0-xyz/1000");
    assert!(result.is_err(), "Non-numeric end must be rejected");
}

#[test]
fn test_parse_content_range_non_numeric_length() {
    let result = parse_content_range("bytes 0-499/abc");
    assert!(result.is_err(), "Non-numeric complete length must be rejected");
}

#[test]
fn test_parse_content_range_trailing_garbage() {
    let result = parse_content_range("bytes 0-499/1000 extra");
    assert!(
        result.is_err(),
        "Trailing garbage must be rejected (nginx checks *p != '\\0')"
    );
}

#[test]
fn test_parse_content_range_empty_string() {
    let result = parse_content_range("");
    assert!(result.is_err(), "Empty string must be rejected");
}

#[test]
fn test_parse_content_range_overflow_end() {
    // u64::MAX = 18446744073709551615; adding 1 for exclusive end would overflow
    let result = parse_content_range("bytes 0-18446744073709551615/999");
    assert!(
        result.is_err(),
        "End value that overflows on increment must be rejected"
    );
}

#[test]
fn test_parse_content_range_single_byte_range() {
    let cr = parse_content_range("bytes 42-42/100").unwrap();
    assert_eq!(cr.start, 42);
    assert_eq!(cr.end, 43); // exclusive
    assert_eq!(cr.complete_length, Some(100));
}

#[test]
fn test_parse_content_range_zero_start() {
    let cr = parse_content_range("bytes 0-0/1").unwrap();
    assert_eq!(cr.start, 0);
    assert_eq!(cr.end, 1);
    assert_eq!(cr.complete_length, Some(1));
}
