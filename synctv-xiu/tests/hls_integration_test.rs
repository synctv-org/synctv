//! HLS Integration Tests
//!
//! This test suite validates:
//! 1. M3U8 playlist generation
//! 2. Segment management (add/cleanup)
//! 3. Stream registry operations
//! 4. Storage abstraction

use std::collections::VecDeque;
use std::time::Instant;
use synctv_xiu::hls::{SegmentInfo, StreamProcessorState};

/// Test 1: M3U8 generation with multiple segments
#[test]
fn test_m3u8_generation_basic() {
    let mut segments = VecDeque::new();

    // Add 3 segments
    for i in 0..3 {
        segments.push_back(SegmentInfo {
            sequence: i,
            duration: 5000, // 5 seconds
            ts_name: format!("seg{}.ts", i),
            discontinuity: false,
            created_at: Instant::now(),
        });
    }

    let state = StreamProcessorState {
        app_name: "live".to_string(),
        stream_name: "test".to_string(),
        segments,
        is_ended: false,
        created_at: Instant::now(),
    };

    let m3u8 = state.generate_m3u8(|ts_name| format!("/hls/{}", ts_name));

    // Verify M3U8 structure
    assert!(m3u8.contains("#EXTM3U"));
    assert!(m3u8.contains("#EXT-X-VERSION:3"));
    assert!(m3u8.contains("#EXT-X-TARGETDURATION:5"));
    assert!(m3u8.contains("#EXT-X-MEDIA-SEQUENCE:0"));

    // Verify segments
    assert!(m3u8.contains("#EXTINF:5.000,"));
    assert!(m3u8.contains("/hls/seg0.ts"));
    assert!(m3u8.contains("/hls/seg1.ts"));
    assert!(m3u8.contains("/hls/seg2.ts"));

    // Should NOT have ENDLIST (stream is ongoing)
    assert!(!m3u8.contains("#EXT-X-ENDLIST"));
}

/// Test 2: M3U8 generation with ended stream
#[test]
fn test_m3u8_generation_ended_stream() {
    let mut segments = VecDeque::new();

    segments.push_back(SegmentInfo {
        sequence: 0,
        duration: 3000,
        ts_name: "final.ts".to_string(),
        discontinuity: false,
        created_at: Instant::now(),
    });

    let state = StreamProcessorState {
        app_name: "live".to_string(),
        stream_name: "test".to_string(),
        segments,
        is_ended: true,
        created_at: std::time::Instant::now(),
    };

    let m3u8 = state.generate_m3u8(|ts_name| format!("/hls/{}", ts_name));

    // Should have ENDLIST for ended stream
    assert!(m3u8.contains("#EXT-X-ENDLIST"));
}

/// Test 3: M3U8 with discontinuity markers
#[test]
fn test_m3u8_discontinuity() {
    let mut segments = VecDeque::new();

    segments.push_back(SegmentInfo {
        sequence: 0,
        duration: 5000,
        ts_name: "seg0.ts".to_string(),
        discontinuity: false,
        created_at: Instant::now(),
    });

    // Second segment has discontinuity (e.g., stream reconnection)
    segments.push_back(SegmentInfo {
        sequence: 1,
        duration: 5000,
        ts_name: "seg1.ts".to_string(),
        discontinuity: true,
        created_at: Instant::now(),
    });

    let state = StreamProcessorState {
        app_name: "live".to_string(),
        stream_name: "test".to_string(),
        segments,
        is_ended: false,
        created_at: std::time::Instant::now(),
    };

    let m3u8 = state.generate_m3u8(|ts_name| format!("/hls/{}", ts_name));

    // Verify discontinuity tag appears before second segment
    assert!(m3u8.contains("#EXT-X-DISCONTINUITY"));

    let discontinuity_pos = m3u8.find("#EXT-X-DISCONTINUITY").unwrap();
    let seg1_pos = m3u8.find("/hls/seg1.ts").unwrap();
    assert!(discontinuity_pos < seg1_pos, "Discontinuity tag should appear before seg1");
}

/// Test 4: M3U8 with custom URL generator (auth tokens)
#[test]
fn test_m3u8_custom_url_generator() {
    let mut segments = VecDeque::new();

    segments.push_back(SegmentInfo {
        sequence: 0,
        duration: 5000,
        ts_name: "seg0.ts".to_string(),
        discontinuity: false,
        created_at: Instant::now(),
    });

    let state = StreamProcessorState {
        app_name: "live".to_string(),
        stream_name: "test".to_string(),
        segments,
        is_ended: false,
        created_at: std::time::Instant::now(),
    };

    let token = "secret-jwt-token";
    let m3u8 = state.generate_m3u8(|ts_name| {
        format!("/api/hls/data/{}?token={}", ts_name, token)
    });

    // Verify custom URL with auth token
    assert!(m3u8.contains("/api/hls/data/seg0.ts?token=secret-jwt-token"));
}

/// Test 5: M3U8 with variable duration segments
#[test]
fn test_m3u8_variable_duration() {
    let mut segments = VecDeque::new();

    // Add segments with different durations
    segments.push_back(SegmentInfo {
        sequence: 0,
        duration: 3000, // 3 seconds
        ts_name: "seg0.ts".to_string(),
        discontinuity: false,
        created_at: Instant::now(),
    });

    segments.push_back(SegmentInfo {
        sequence: 1,
        duration: 7000, // 7 seconds (longer)
        ts_name: "seg1.ts".to_string(),
        discontinuity: false,
        created_at: Instant::now(),
    });

    segments.push_back(SegmentInfo {
        sequence: 2,
        duration: 5000, // 5 seconds
        ts_name: "seg2.ts".to_string(),
        discontinuity: false,
        created_at: Instant::now(),
    });

    let state = StreamProcessorState {
        app_name: "live".to_string(),
        stream_name: "test".to_string(),
        segments,
        is_ended: false,
        created_at: std::time::Instant::now(),
    };

    let m3u8 = state.generate_m3u8(|ts_name| format!("/hls/{}", ts_name));

    // Target duration should be rounded up to max segment duration (7 seconds)
    assert!(m3u8.contains("#EXT-X-TARGETDURATION:7"));

    // Verify individual segment durations
    assert!(m3u8.contains("#EXTINF:3.000,"));
    assert!(m3u8.contains("#EXTINF:7.000,"));
    assert!(m3u8.contains("#EXTINF:5.000,"));
}

/// Test 6: M3U8 with empty segments (edge case)
#[test]
fn test_m3u8_empty_segments() {
    let state = StreamProcessorState {
        app_name: "live".to_string(),
        stream_name: "test".to_string(),
        segments: VecDeque::new(),
        is_ended: false,
        created_at: std::time::Instant::now(),
    };

    let m3u8 = state.generate_m3u8(|ts_name| format!("/hls/{}", ts_name));

    // Should still generate valid M3U8 structure
    assert!(m3u8.contains("#EXTM3U"));
    assert!(m3u8.contains("#EXT-X-VERSION:3"));

    // Media sequence should be 0
    assert!(m3u8.contains("#EXT-X-MEDIA-SEQUENCE:0"));

    // Target duration defaults to 10 when no segments
    assert!(m3u8.contains("#EXT-X-TARGETDURATION:10"));
}

/// Test 7: M3U8 with sliding window (media sequence advances)
#[test]
fn test_m3u8_sliding_window() {
    let mut segments = VecDeque::new();

    // Simulate sliding window: sequence starts at 100
    for i in 100..103 {
        segments.push_back(SegmentInfo {
            sequence: i,
            duration: 5000,
            ts_name: format!("seg{}.ts", i),
            discontinuity: false,
            created_at: Instant::now(),
        });
    }

    let state = StreamProcessorState {
        app_name: "live".to_string(),
        stream_name: "test".to_string(),
        segments,
        is_ended: false,
        created_at: std::time::Instant::now(),
    };

    let m3u8 = state.generate_m3u8(|ts_name| format!("/hls/{}", ts_name));

    // Media sequence should reflect first segment in window
    assert!(m3u8.contains("#EXT-X-MEDIA-SEQUENCE:100"));

    // Verify segments are present
    assert!(m3u8.contains("/hls/seg100.ts"));
    assert!(m3u8.contains("/hls/seg101.ts"));
    assert!(m3u8.contains("/hls/seg102.ts"));
}

/// Test 8: Segment duration precision
#[test]
fn test_segment_duration_precision() {
    let mut segments = VecDeque::new();

    // Test fractional second durations
    segments.push_back(SegmentInfo {
        sequence: 0,
        duration: 5123, // 5.123 seconds
        ts_name: "seg0.ts".to_string(),
        discontinuity: false,
        created_at: Instant::now(),
    });

    segments.push_back(SegmentInfo {
        sequence: 1,
        duration: 4567, // 4.567 seconds
        ts_name: "seg1.ts".to_string(),
        discontinuity: false,
        created_at: Instant::now(),
    });

    let state = StreamProcessorState {
        app_name: "live".to_string(),
        stream_name: "test".to_string(),
        segments,
        is_ended: false,
        created_at: std::time::Instant::now(),
    };

    let m3u8 = state.generate_m3u8(|ts_name| format!("/hls/{}", ts_name));

    // Verify 3 decimal places precision
    assert!(m3u8.contains("#EXTINF:5.123,"));
    assert!(m3u8.contains("#EXTINF:4.567,"));
}

/// Test 9: M3U8 URL escaping (special characters)
#[test]
fn test_m3u8_url_special_chars() {
    let mut segments = VecDeque::new();

    segments.push_back(SegmentInfo {
        sequence: 0,
        duration: 5000,
        ts_name: "seg 0.ts".to_string(), // Space in filename
        discontinuity: false,
        created_at: Instant::now(),
    });

    let state = StreamProcessorState {
        app_name: "live".to_string(),
        stream_name: "test".to_string(),
        segments,
        is_ended: false,
        created_at: std::time::Instant::now(),
    };

    let m3u8 = state.generate_m3u8(|ts_name| {
        // URL generator should handle encoding
        format!("/hls/{}", ts_name.replace(' ', "%20"))
    });

    // Verify URL encoding is handled by the caller
    assert!(m3u8.contains("/hls/seg%200.ts"));
}

/// Test 10: Multiple discontinuities
#[test]
fn test_m3u8_multiple_discontinuities() {
    let mut segments = VecDeque::new();

    segments.push_back(SegmentInfo {
        sequence: 0,
        duration: 5000,
        ts_name: "seg0.ts".to_string(),
        discontinuity: false,
        created_at: Instant::now(),
    });

    segments.push_back(SegmentInfo {
        sequence: 1,
        duration: 5000,
        ts_name: "seg1.ts".to_string(),
        discontinuity: true, // First discontinuity
        created_at: Instant::now(),
    });

    segments.push_back(SegmentInfo {
        sequence: 2,
        duration: 5000,
        ts_name: "seg2.ts".to_string(),
        discontinuity: false,
        created_at: Instant::now(),
    });

    segments.push_back(SegmentInfo {
        sequence: 3,
        duration: 5000,
        ts_name: "seg3.ts".to_string(),
        discontinuity: true, // Second discontinuity
        created_at: Instant::now(),
    });

    let state = StreamProcessorState {
        app_name: "live".to_string(),
        stream_name: "test".to_string(),
        segments,
        is_ended: false,
        created_at: std::time::Instant::now(),
    };

    let m3u8 = state.generate_m3u8(|ts_name| format!("/hls/{}", ts_name));

    // Count discontinuity markers
    let discontinuity_count = m3u8.matches("#EXT-X-DISCONTINUITY").count();
    assert_eq!(discontinuity_count, 2, "Should have 2 discontinuity markers");
}
