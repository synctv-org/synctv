//! Proxy Integration Tests
//!
//! This test suite validates:
//! 1. MPD generation correctness
//! 2. URL rewriting for proxy paths
//! 3. Rate limiting enforcement
//! 4. XML escaping

use synctv_proto::dash::{DashAudioStream, DashManifestData, DashSegmentBase, DashVideoStream};
use synctv_proxy::mpd::{generate_mpd, MpdOptions};
use synctv_proxy::{check_proxy_rate_limit, percent_encode};

/// Test 1: MPD generation with video and audio streams
#[test]
fn test_mpd_generation_basic() {
    let data = DashManifestData {
        duration: 120.5,
        min_buffer_time: 2.0,
        video_streams: vec![DashVideoStream {
            id: "video-1".to_string(),
            base_url: "https://cdn.example.com/video.m4s".to_string(),
            backup_urls: vec![],
            codecs: "avc1.42E01E".to_string(),
            mime_type: "video/mp4".to_string(),
            width: 1920,
            height: 1080,
            frame_rate: "30".to_string(),
            sar: "1:1".to_string(),
            start_with_sap: 1,
            bandwidth: 3000000,
            segment_base: DashSegmentBase {
                initialization: "0-1234".to_string(),
                index_range: "1235-2345".to_string(),
            },
        }],
        audio_streams: vec![DashAudioStream {
            id: "audio-1".to_string(),
            base_url: "https://cdn.example.com/audio.m4s".to_string(),
            backup_urls: vec![],
            codecs: "mp4a.40.2".to_string(),
            mime_type: "audio/mp4".to_string(),
            bandwidth: 128000,
            audio_sampling_rate: 44100,
            start_with_sap: 1,
            segment_base: DashSegmentBase {
                initialization: "0-567".to_string(),
                index_range: "568-890".to_string(),
            },
        }],
    };

    let opts = MpdOptions {
        proxy_base_url: None,
        token: None,
    };

    let mpd = generate_mpd(&data, &opts);

    // Verify MPD structure
    assert!(mpd.contains("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
    assert!(mpd.contains("<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\""));
    assert!(mpd.contains("mediaPresentationDuration=\"PT2M0.5S\""));
    assert!(mpd.contains("minBufferTime=\"PT2S\""));

    // Verify video stream
    assert!(mpd.contains("video-1"));
    assert!(mpd.contains("avc1.42E01E"));
    assert!(mpd.contains("width=\"1920\""));
    assert!(mpd.contains("height=\"1080\""));
    assert!(mpd.contains("https://cdn.example.com/video.m4s"));
    assert!(mpd.contains("indexRange=\"1235-2345\""));
    assert!(mpd.contains("<Initialization range=\"0-1234\"/>"));

    // Verify audio stream
    assert!(mpd.contains("audio-1"));
    assert!(mpd.contains("mp4a.40.2"));
    assert!(mpd.contains("bandwidth=\"128000\""));
    assert!(mpd.contains("https://cdn.example.com/audio.m4s"));
    assert!(mpd.contains("audioSamplingRate=\"44100\""));
}

/// Test 2: MPD generation with proxy URL rewriting
#[test]
fn test_mpd_generation_with_proxy_urls() {
    let data = DashManifestData {
        duration: 60.0,
        min_buffer_time: 1.5,
        video_streams: vec![
            DashVideoStream {
                id: "video-high".to_string(),
                base_url: "https://cdn.example.com/video_high.m4s".to_string(),
                backup_urls: vec![],
                codecs: "avc1.42E01E".to_string(),
                mime_type: "video/mp4".to_string(),
                width: 1920,
                height: 1080,
                frame_rate: "30".to_string(),
                sar: "1:1".to_string(),
                start_with_sap: 1,
                bandwidth: 5000000,
                segment_base: DashSegmentBase {
                    initialization: "0-1000".to_string(),
                    index_range: "1001-2000".to_string(),
                },
            },
            DashVideoStream {
                id: "video-low".to_string(),
                base_url: "https://cdn.example.com/video_low.m4s".to_string(),
                backup_urls: vec![],
                codecs: "avc1.42E01E".to_string(),
                mime_type: "video/mp4".to_string(),
                width: 1280,
                height: 720,
                frame_rate: "30".to_string(),
                sar: "1:1".to_string(),
                start_with_sap: 1,
                bandwidth: 2000000,
                segment_base: DashSegmentBase {
                    initialization: "0-800".to_string(),
                    index_range: "801-1600".to_string(),
                },
            },
        ],
        audio_streams: vec![DashAudioStream {
            id: "audio-aac".to_string(),
            base_url: "https://cdn.example.com/audio.m4s".to_string(),
            backup_urls: vec![],
            codecs: "mp4a.40.2".to_string(),
            mime_type: "audio/mp4".to_string(),
            bandwidth: 128000,
            audio_sampling_rate: 48000,
            start_with_sap: 1,
            segment_base: DashSegmentBase {
                initialization: "0-500".to_string(),
                index_range: "501-1000".to_string(),
            },
        }],
    };

    let opts = MpdOptions {
        proxy_base_url: Some("/api/v1/proxy/dash"),
        token: Some("test-jwt-token"),
    };

    let mpd = generate_mpd(&data, &opts);

    // Verify proxy URLs are used instead of CDN URLs
    assert!(!mpd.contains("cdn.example.com"));

    // Video streams should be at index 0 and 1
    assert!(mpd.contains("/api/v1/proxy/dash/stream/0?token=test%2Djwt%2Dtoken"));
    assert!(mpd.contains("/api/v1/proxy/dash/stream/1?token=test%2Djwt%2Dtoken"));

    // Audio stream should be at index 2 (after 2 video streams)
    assert!(mpd.contains("/api/v1/proxy/dash/stream/2?token=test%2Djwt%2Dtoken"));

    // Segment base info should still be present
    assert!(mpd.contains("indexRange=\"1001-2000\""));
    assert!(mpd.contains("indexRange=\"801-1600\""));
    assert!(mpd.contains("indexRange=\"501-1000\""));
}

/// Test 3: MPD generation with empty streams
#[test]
fn test_mpd_generation_empty_streams() {
    let data = DashManifestData {
        duration: 0.0,
        min_buffer_time: 1.0,
        video_streams: vec![],
        audio_streams: vec![],
    };

    let opts = MpdOptions {
        proxy_base_url: None,
        token: None,
    };

    let mpd = generate_mpd(&data, &opts);

    // Should still generate valid XML structure
    assert!(mpd.contains("<?xml version=\"1.0\""));
    assert!(mpd.contains("<MPD"));
    assert!(mpd.contains("</MPD>"));
    assert!(mpd.contains("<Period>"));
    assert!(mpd.contains("</Period>"));

    // Duration should be PT0S for zero duration
    assert!(mpd.contains("mediaPresentationDuration=\"PT0S\""));
}

/// Test 4: Duration formatting edge cases
#[test]
fn test_duration_formatting() {
    let test_cases = vec![
        (0.0, "PT0S"),
        (1.0, "PT1S"),
        (60.0, "PT1M"),
        (61.5, "PT1M1.5S"),
        (3661.0, "PT1H1M1S"),
        (3723.8, "PT1H2M3.8S"),
    ];

    for (duration, expected_substr) in test_cases {
        let data = DashManifestData {
            duration,
            min_buffer_time: 1.0,
            video_streams: vec![],
            audio_streams: vec![],
        };

        let opts = MpdOptions {
            proxy_base_url: None,
            token: None,
        };

        let mpd = generate_mpd(&data, &opts);
        assert!(
            mpd.contains(&format!("mediaPresentationDuration=\"{}\"", expected_substr)),
            "Expected duration '{}' not found in MPD for input {}",
            expected_substr,
            duration
        );
    }
}

/// Test 5: URL percent encoding
#[test]
fn test_percent_encoding() {
    assert_eq!(percent_encode("hello world"), "hello%20world");
    assert_eq!(percent_encode("test@email.com"), "test%40email%2Ecom");
    assert_eq!(percent_encode("a/b?c=d&e=f"), "a%2Fb%3Fc%3Dd%26e%3Df");
    assert_eq!(
        percent_encode("https://example.com/path"),
        "https%3A%2F%2Fexample%2Ecom%2Fpath"
    );
}

/// Test 6: Rate limiting allows burst
#[test]
fn test_rate_limit_allows_burst() {
    let key = "test-burst-ip-6";

    // Should allow multiple requests in quick succession (burst)
    for i in 0..10 {
        let result = check_proxy_rate_limit(key);
        assert!(
            result.is_ok(),
            "Request {} should be allowed within burst window",
            i
        );
    }
}

/// Test 7: Rate limiting is per-key
#[test]
fn test_rate_limit_per_key() {
    let key1 = "test-key-7-1";
    let key2 = "test-key-7-2";

    // Consume all requests for key1
    for _ in 0..60 {
        let _ = check_proxy_rate_limit(key1);
    }

    // key1 should be rate limited
    let result1 = check_proxy_rate_limit(key1);
    assert!(result1.is_err(), "key1 should be rate limited");

    // key2 should still be allowed
    let result2 = check_proxy_rate_limit(key2);
    assert!(
        result2.is_ok(),
        "key2 should not be affected by key1's rate limit"
    );
}

/// Test 8: Rate limit response includes Retry-After header
#[test]
fn test_rate_limit_response_headers() {
    let key = "test-retry-after-8";

    // Exhaust the rate limit
    for _ in 0..60 {
        let _ = check_proxy_rate_limit(key);
    }

    // Next request should fail with proper headers
    let result = check_proxy_rate_limit(key);
    assert!(result.is_err());

    let response = result.unwrap_err();
    assert_eq!(
        response.status(),
        axum::http::StatusCode::TOO_MANY_REQUESTS
    );

    let headers = response.headers();
    assert!(
        headers.contains_key("Retry-After"),
        "Rate limit response must include Retry-After header"
    );
}

/// Test 9: MPD with special characters in stream IDs
#[test]
fn test_mpd_xml_escaping() {
    let data = DashManifestData {
        duration: 10.0,
        min_buffer_time: 1.0,
        video_streams: vec![DashVideoStream {
            id: "video<>&\"'".to_string(), // Contains XML special characters
            base_url: "https://cdn.example.com/video.m4s".to_string(),
            backup_urls: vec![],
            codecs: "avc1.42E01E".to_string(),
            mime_type: "video/mp4".to_string(),
            width: 1920,
            height: 1080,
            frame_rate: "30".to_string(),
            sar: "1:1".to_string(),
            start_with_sap: 1,
            bandwidth: 1000000,
            segment_base: DashSegmentBase {
                initialization: "0-100".to_string(),
                index_range: "101-200".to_string(),
            },
        }],
        audio_streams: vec![],
    };

    let opts = MpdOptions {
        proxy_base_url: None,
        token: None,
    };

    let mpd = generate_mpd(&data, &opts);

    // XML special characters should be escaped (<, >, &, ")
    // Single quote (') doesn't need to be escaped in XML text content
    assert!(mpd.contains("video&lt;&gt;&amp;&quot;"));
    assert!(
        !mpd.contains("video<>&\""),
        "Raw special characters <, >, &, \" should not appear in XML"
    );
}

/// Test 10: MPD without proxy base URL uses CDN URLs directly
#[test]
fn test_mpd_without_proxy_uses_cdn_urls() {
    let data = DashManifestData {
        duration: 30.0,
        min_buffer_time: 1.5,
        video_streams: vec![DashVideoStream {
            id: "v1".to_string(),
            base_url: "https://cdn.bilibili.com/video.m4s".to_string(),
            backup_urls: vec![],
            codecs: "avc1.42E01E".to_string(),
            mime_type: "video/mp4".to_string(),
            width: 1920,
            height: 1080,
            frame_rate: "24".to_string(),
            sar: "1:1".to_string(),
            start_with_sap: 1,
            bandwidth: 2500000,
            segment_base: DashSegmentBase {
                initialization: "0-500".to_string(),
                index_range: "501-1000".to_string(),
            },
        }],
        audio_streams: vec![],
    };

    let opts = MpdOptions {
        proxy_base_url: None,
        token: None,
    };

    let mpd = generate_mpd(&data, &opts);

    // Should use original CDN URL
    assert!(mpd.contains("https://cdn.bilibili.com/video.m4s"));
    assert!(!mpd.contains("/stream/"));
}
