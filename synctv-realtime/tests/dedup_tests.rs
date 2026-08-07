//! Deduplication tests
//!
//! Tests for `MessageDeduplicator` behavior: `mark_processed` prevents
//! reprocessing, `len` tracking, and room-scoped event IDs produce different
//! dedup keys.

#![allow(clippy::unwrap_used)]
use synctv_realtime::sync::{DedupKey, MessageDeduplicator};

fn make_key(room_id: i64, event_id: impl Into<String>) -> DedupKey {
    DedupKey {
        room_id: Some(room_id),
        event_id: event_id.into(),
    }
}

// Test 1: mark_processed prevents reprocessing

#[tokio::test]
async fn test_mark_processed_prevents_reprocessing() {
    let dedup = MessageDeduplicator::default();
    let key = make_key(1, "event-1");

    // Before marking, should_process returns true
    assert!(dedup.should_process(&key), "First call should return true");

    // After should_process returned true and the entry is now in the cache,
    // should_process returns false
    assert!(
        !dedup.should_process(&key),
        "Duplicate call should return false"
    );

    // Using mark_processed explicitly on a new key
    let key2 = make_key(2, "event-2");
    dedup.mark_processed(key2.clone());
    assert!(
        !dedup.should_process(&key2),
        "After mark_processed, should_process should return false"
    );
}

// Test 2: len tracking

#[tokio::test]
async fn test_dedup_len_tracking() {
    let dedup = MessageDeduplicator::default();

    assert_eq!(dedup.len(), 0);

    let key1 = make_key(1, "event-1");
    let _ = dedup.should_process(&key1);
    assert_eq!(dedup.len(), 1, "One entry after first should_process");

    let key2 = make_key(2, "event-2");
    let _ = dedup.should_process(&key2);
    assert_eq!(dedup.len(), 2, "Two entries after second should_process");

    // Duplicate does not increase len
    let _ = dedup.should_process(&key1);
    assert_eq!(dedup.len(), 2, "Duplicate should not increase len");

    // Clear resets
    dedup.clear();
    assert_eq!(dedup.len(), 0);
}

// Test 3: Different events at same timestamp with different keys

#[tokio::test]
async fn test_room_scoped_event_ids_produce_different_keys() {
    let dedup = MessageDeduplicator::default();

    let key_a = make_key(1, "shared-event");
    let key_b = make_key(2, "shared-event");

    assert!(
        dedup.should_process(&key_a),
        "First event should be processed"
    );
    assert!(
        dedup.should_process(&key_b),
        "Second room-scoped key should also be processed"
    );

    // Both should now be duplicates
    assert!(!dedup.should_process(&key_a));
    assert!(!dedup.should_process(&key_b));

    let key_c = make_key(1, "different-event");
    assert!(
        dedup.should_process(&key_c),
        "Different event_id should produce a different key"
    );
}
