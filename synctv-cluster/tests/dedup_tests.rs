//! Deduplication tests
//!
//! Tests for MessageDeduplicator behavior: mark_processed prevents
//! reprocessing, len/is_empty tracking, and different events at the same
//! timestamp produce different dedup keys.

use synctv_cluster::{MessageDeduplicator, DedupKey};

fn make_key(event_type: &str, room: &str, user: &str, ts: i64, hash: u64) -> DedupKey {
    DedupKey {
        event_type: event_type.to_string(),
        room_id: room.to_string(),
        user_id: user.to_string(),
        extra: String::new(),
        timestamp_ms: ts,
        content_hash: hash,
    }
}

// ============================================================================
// Test 1: mark_processed prevents reprocessing
// ============================================================================

#[tokio::test]
async fn test_mark_processed_prevents_reprocessing() {
    let dedup = MessageDeduplicator::with_defaults();
    let key = make_key("chat", "room1", "user1", 1000, 42);

    // Before marking, should_process returns true
    assert!(
        dedup.should_process(&key),
        "First call should return true"
    );

    // After should_process returned true and the entry is now in the cache,
    // should_process returns false
    assert!(
        !dedup.should_process(&key),
        "Duplicate call should return false"
    );

    // Using mark_processed explicitly on a new key
    let key2 = make_key("chat", "room2", "user1", 2000, 99);
    dedup.mark_processed(key2.clone());
    assert!(
        !dedup.should_process(&key2),
        "After mark_processed, should_process should return false"
    );
}

// ============================================================================
// Test 2: len and is_empty tracking
// ============================================================================

#[tokio::test]
async fn test_dedup_len_and_is_empty() {
    let dedup = MessageDeduplicator::with_defaults();

    assert!(dedup.is_empty(), "New dedup should be empty");
    assert_eq!(dedup.len(), 0);

    let key1 = make_key("chat", "r1", "u1", 1000, 0);
    let _ = dedup.should_process(&key1);
    assert_eq!(dedup.len(), 1, "One entry after first should_process");
    assert!(!dedup.is_empty());

    let key2 = make_key("chat", "r2", "u2", 2000, 0);
    let _ = dedup.should_process(&key2);
    assert_eq!(dedup.len(), 2, "Two entries after second should_process");

    // Duplicate does not increase len
    let _ = dedup.should_process(&key1);
    assert_eq!(dedup.len(), 2, "Duplicate should not increase len");

    // Clear resets
    dedup.clear();
    assert!(dedup.is_empty(), "Should be empty after clear");
    assert_eq!(dedup.len(), 0);
}

// ============================================================================
// Test 3: Different events at same timestamp with different keys
// ============================================================================

#[tokio::test]
async fn test_different_events_same_timestamp_different_keys() {
    let dedup = MessageDeduplicator::with_defaults();
    let ts = 5000i64;

    // Two different events at the exact same timestamp
    let key_a = make_key("chat", "room1", "user1", ts, 111);
    let key_b = make_key("chat", "room1", "user1", ts, 222);

    assert!(
        dedup.should_process(&key_a),
        "First event should be processed"
    );
    assert!(
        dedup.should_process(&key_b),
        "Second event with different content_hash should also be processed"
    );

    // Both should now be duplicates
    assert!(!dedup.should_process(&key_a));
    assert!(!dedup.should_process(&key_b));

    // Same content_hash but different event_type
    let key_c = make_key("playback", "room1", "user1", ts, 111);
    assert!(
        dedup.should_process(&key_c),
        "Different event_type should produce a different key"
    );
}
