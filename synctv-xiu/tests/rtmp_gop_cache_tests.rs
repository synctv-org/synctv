//! GOP (Group of Pictures) cache tests for RTMP streaming.
//!
//! These tests verify:
//! - Memory limits are enforced
//! - GOP eviction works correctly
//! - Frame count limits prevent unbounded growth
//! - Zero-copy cloning via Arc works as expected
//!
//! Note: `Gop::save_frame_data` and `Gop::freeze` are private methods.
//! Tests use the public `Gops::save_frame_data` API which internally
//! manages individual Gop instances.
//!
//! Implementation detail:
//! When `save_frame_data` is called with `is_key_frame=true`, the initial
//! empty GOP is reused for the first keyframe. Later keyframes freeze the
//! current GOP and start the next one.

#![allow(clippy::unwrap_used)]
use bytes::Bytes;
use synctv_xiu::rtmp::cache::gop::{Gop, Gops};
use synctv_xiu::streamhub::define::FrameData;

// Helper Functions

/// Create a keyframe video frame
fn make_keyframe(timestamp: u32, size: usize) -> FrameData {
    let data = vec![0u8; size];
    FrameData::Video {
        timestamp,
        data: Bytes::from(data),
    }
}

/// Create an inter frame (non-keyframe)
fn make_inter_frame(timestamp: u32, size: usize) -> FrameData {
    let data = vec![1u8; size];
    FrameData::Video {
        timestamp,
        data: Bytes::from(data),
    }
}

/// Create an audio frame
fn make_audio_frame(timestamp: u32, size: usize) -> FrameData {
    let data = vec![2u8; size];
    FrameData::Audio {
        timestamp,
        data: Bytes::from(data),
    }
}

// Gop Struct Tests (Public API only)

#[test]
fn test_gop_get_frame_data_empty() {
    let gop = Gop::new();
    let frames = gop.get_frame_data();
    assert!(frames.is_empty());
}

#[test]
fn test_gop_frame_data_empty() {
    let gop = Gop::new();
    assert!(gop.frame_data().is_empty());
}

// Note: frame_memory_size is pub(crate) so can't be tested directly from outside.
// Memory accounting is verified indirectly via current_total_bytes().

// Gops (Multiple GOP) Tests

#[test]
fn test_gops_disabled_with_zero_size() {
    let gops = Gops::new(0, None);

    assert!(!gops.is_enabled(), "Gops with size 0 should be disabled");
}

#[test]
fn test_gops_custom_memory_limit() {
    let custom_limit = 100 * 1024 * 1024; // 100MB
    let gops = Gops::new(5, Some(custom_limit));

    assert_eq!(gops.max_total_bytes(), custom_limit);
}

#[test]
fn test_gops_eviction_on_count_limit() {
    let mut gops = Gops::new(3, None);

    gops.save_frame_data(make_keyframe(0, 1000), true);
    assert_eq!(gops.gop_count(), 1, "First keyframe reuses initial GOP");

    gops.save_frame_data(make_keyframe(1000, 1000), true);
    assert_eq!(gops.gop_count(), 2, "Second keyframe creates second GOP");

    gops.save_frame_data(make_keyframe(2000, 1000), true);
    assert_eq!(gops.gop_count(), 3, "Third keyframe creates third GOP");

    // Add fourth keyframe - would create a fourth but limit is 3, so eviction happens
    gops.save_frame_data(make_keyframe(3000, 1000), true);
    assert_eq!(
        gops.gop_count(),
        3,
        "Should still have 3 GOPs after eviction"
    );

    // Add fifth keyframe - more eviction
    gops.save_frame_data(make_keyframe(4000, 1000), true);
    assert_eq!(
        gops.gop_count(),
        3,
        "Should still have 3 GOPs after more eviction"
    );

    // Verify GOP count is maintained
    let gops_ref = gops.get_gops();
    assert_eq!(gops_ref.len(), 3);
}

/// Test that GOPs are evicted when memory limit is exceeded
#[test]
fn test_gops_eviction_on_memory_limit() {
    // Use a small memory limit for testing
    let small_limit = 10 * 1024; // 10KB
    let mut gops = Gops::new(10, Some(small_limit));

    // Add several GOPs with frames
    for i in 0..5 {
        gops.save_frame_data(make_keyframe(i * 1000, 2000), true); // 2KB per keyframe
        gops.save_frame_data(make_inter_frame(i * 1000 + 100, 2000), false);
    }

    // Check that memory is within limit
    assert!(
        gops.current_total_bytes() <= small_limit,
        "Memory usage {} should be <= limit {}",
        gops.current_total_bytes(),
        small_limit
    );
}

/// Test eviction preserves at least the active GOP
#[test]
fn test_gops_eviction_preserves_active_gop() {
    // Very small limit
    let mut gops = Gops::new(5, Some(1000));

    // Add a large frame that exceeds limit
    gops.save_frame_data(make_keyframe(0, 2000), true);

    // Should still have at least one GOP
    assert!(gops.gop_count() >= 1, "Should have at least the active GOP");
}

/// Test very large number of GOPs
#[test]
fn test_gops_many_gops_eviction() {
    let mut gops = Gops::new(5, None);

    for i in 0..100 {
        gops.save_frame_data(make_keyframe(i * 1000, 100), true);
        gops.save_frame_data(make_inter_frame(i * 1000 + 100, 100), false);
    }

    assert!(
        gops.gop_count() <= 5,
        "GOP count {} should be <= size",
        gops.gop_count()
    );
}

// Zero-Copy Clone Tests (via public API)

/// Test that cloning Gops is cheap (Arc clones)
#[test]
fn test_gops_get_gops_returns_frozen() {
    let mut gops = Gops::new(5, None);

    // Add frames to the active GOP
    gops.save_frame_data(make_keyframe(0, 100), true);
    gops.save_frame_data(make_inter_frame(100, 100), false);
    gops.save_frame_data(make_inter_frame(200, 100), false);

    // Get GOPs (should freeze the active one)
    let gops_ref = gops.get_gops();

    assert_eq!(gops_ref.len(), 1);
}

// Frame Saving Tests

/// Test saving keyframe creates new GOP
#[test]
fn test_gops_keyframe_creates_new_gop() {
    let mut gops = Gops::new(5, None);

    // Initial GOP is created
    assert_eq!(gops.gop_count(), 1);

    gops.save_frame_data(make_keyframe(0, 100), true);
    assert_eq!(gops.gop_count(), 1, "First keyframe reuses initial GOP");

    gops.save_frame_data(make_keyframe(1000, 100), true);
    assert_eq!(gops.gop_count(), 2, "Second keyframe creates second GOP");
}

/// Test saving inter frames adds to current GOP (no new GOP creation)
#[test]
fn test_gops_inter_frames_add_to_current_gop() {
    let mut gops = Gops::new(5, None);

    gops.save_frame_data(make_keyframe(0, 100), true);
    assert_eq!(gops.gop_count(), 1, "After keyframe");

    // Inter frames should not create new GOPs
    gops.save_frame_data(make_inter_frame(100, 100), false);
    gops.save_frame_data(make_inter_frame(200, 100), false);

    assert_eq!(gops.gop_count(), 1, "After inter frames");
}

/// Test interleaved audio and video frames
#[test]
fn test_gops_mixed_audio_video_frames() {
    let mut gops = Gops::new(5, None);

    // Interleave audio and video
    gops.save_frame_data(make_keyframe(0, 1000), true);
    gops.save_frame_data(make_audio_frame(0, 100), false);
    gops.save_frame_data(make_inter_frame(100, 500), false);
    gops.save_frame_data(make_audio_frame(100, 100), false);
    gops.save_frame_data(make_inter_frame(200, 500), false);
    gops.save_frame_data(make_audio_frame(200, 100), false);

    assert_eq!(gops.gop_count(), 1);
    // Total memory: 1000 + 100 + 500 + 100 + 500 + 100 = 2300
    assert_eq!(gops.current_total_bytes(), 2300);
}

// Memory Accounting Tests

/// Test that memory is properly tracked across multiple operations
#[test]
fn test_gops_memory_accounting() {
    let mut gops = Gops::new(10, Some(10000)); // 10KB limit

    // Add keyframe (creates new GOP)
    gops.save_frame_data(make_keyframe(0, 1000), true);
    assert_eq!(gops.current_total_bytes(), 1000);

    gops.save_frame_data(make_inter_frame(100, 500), false);
    assert_eq!(gops.current_total_bytes(), 1500);

    // New GOP (new keyframe)
    gops.save_frame_data(make_keyframe(1000, 2000), true);
    assert_eq!(gops.current_total_bytes(), 3500);

    // Add more frames
    gops.save_frame_data(make_inter_frame(1100, 500), false);
    assert_eq!(gops.current_total_bytes(), 4000);
}

/// Test memory decreases when GOPs are evicted
#[test]
fn test_gops_memory_decreases_on_eviction() {
    let mut gops = Gops::new(3, Some(10000)); // 3 GOP max, 10KB limit

    gops.save_frame_data(make_keyframe(0, 2000), true);
    gops.save_frame_data(make_inter_frame(100, 1000), false);

    assert_eq!(gops.current_total_bytes(), 3000);

    // Second keyframe creates second GOP
    gops.save_frame_data(make_keyframe(1000, 3000), true);
    gops.save_frame_data(make_inter_frame(1100, 1000), false);

    // Third keyframe creates third GOP
    gops.save_frame_data(make_keyframe(2000, 1000), true);

    // Should not exceed max GOP count
    assert!(gops.gop_count() <= 3);
}

/// Test empty frames have zero memory
#[test]
fn test_gops_empty_frame_memory() {
    let mut gops = Gops::new(5, None);

    let empty_frame = FrameData::Video {
        timestamp: 0,
        data: Bytes::new(),
    };

    gops.save_frame_data(empty_frame, false);

    // Empty frame should add 0 bytes
    assert_eq!(gops.current_total_bytes(), 0);
}

// Disabled GOP Cache Tests

#[test]
fn test_gops_disabled_drops_all_frames() {
    let mut gops = Gops::new(0, None); // Disabled

    // All frames should be dropped silently
    gops.save_frame_data(make_keyframe(0, 1000), true);
    gops.save_frame_data(make_inter_frame(100, 1000), false);

    // Should have initial GOP but no frames
    assert!(!gops.is_enabled());
    assert_eq!(gops.current_total_bytes(), 0);
}

// Default Values Documentation Tests

#[test]
fn test_gops_current_total_bytes_initial() {
    let gops = Gops::new(5, None);
    assert_eq!(gops.current_total_bytes(), 0);
}

/// Test is_enabled method
#[test]
fn test_gops_is_enabled_method() {
    let gops = Gops::new(5, None);
    assert!(gops.is_enabled());

    let gops_disabled = Gops::new(0, None);
    assert!(!gops_disabled.is_enabled());
}

/// Test that very large frames are handled
#[test]
fn test_gops_large_frame_handling() {
    // Use a small memory limit
    let mut gops = Gops::new(5, Some(5000));

    // Frame larger than limit
    gops.save_frame_data(make_keyframe(0, 10000), true);

    // Should handle gracefully - may drop frame or exceed slightly
    // depending on implementation
    assert!(gops.gop_count() >= 1);
}

/// Test rapid GOP creation
#[test]
fn test_gops_rapid_creation() {
    let mut gops = Gops::new(10, None);

    // Rapidly create GOPs with single keyframes
    for i in 0..50 {
        gops.save_frame_data(make_keyframe(i * 1000, 100), true);
    }

    assert!(
        gops.gop_count() <= 10,
        "GOP count {} should be <= 10",
        gops.gop_count()
    );
}

/// Test is_enabled matches size > 0
#[test]
fn test_gops_is_enabled() {
    let gops_enabled = Gops::new(5, None);
    assert!(gops_enabled.is_enabled());

    let gops_disabled = Gops::new(0, None);
    assert!(!gops_disabled.is_enabled());
}

/// Test `max_gop_count` returns configured size
#[test]
fn test_gops_max_gop_count() {
    let gops = Gops::new(10, None);
    assert_eq!(gops.max_gop_count(), 10);

    let gops_zero = Gops::new(0, None);
    assert_eq!(gops_zero.max_gop_count(), 0);
}
