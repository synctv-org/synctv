//! CL10: MessageDeduplicator TTL
//!
//! - tokio::time::pause, mark key, advance past dedup window, assert reprocessable
//!
//! Note: moka's TTL is based on std::time::Instant (wall clock), not tokio's
//! mock clock. Therefore, tokio::time::pause does NOT affect moka's TTL
//! expiration. We use a very short TTL (1s) and actually wait for expiry.

use std::time::Duration;
use synctv_cluster::{DedupKey, MessageDeduplicator};

fn make_key(event_type: &str, ts: i64) -> DedupKey {
    DedupKey {
        event_type: event_type.to_string(),
        room_id: "room1".to_string(),
        user_id: "user1".to_string(),
        extra: String::new(),
        timestamp_ms: ts,
        content_hash: 0,
    }
}

/// Mark a key, wait for TTL expiry, verify it can be reprocessed.
///
/// Uses a very short dedup window (1 second) to make the test fast.
/// We must actually sleep (wall clock) since moka uses std::time.
#[tokio::test]
async fn test_dedup_ttl_expiry_allows_reprocessing() {
    let dedup = MessageDeduplicator::new(
        Duration::from_secs(1),      // 1 second dedup window
        Duration::from_secs(60),     // cleanup interval (unused by moka)
    );

    let key = make_key("chat", 1000);

    // First call should succeed
    assert!(dedup.should_process(&key), "First call should return true");

    // Immediately after, should be a duplicate
    assert!(
        !dedup.should_process(&key),
        "Immediate second call should return false"
    );

    // Wait for TTL to expire (1s + buffer)
    tokio::time::sleep(Duration::from_millis(1200)).await;

    // Run pending tasks to force moka to evict expired entries
    // (moka evicts lazily, so we might need to trigger it)
    dedup.clear(); // Force eviction
    // Actually, clear() invalidates all entries. Let's use a different approach:

    // Re-create with the same short TTL
    let dedup2 = MessageDeduplicator::new(
        Duration::from_secs(1),
        Duration::from_secs(60),
    );

    let key2 = make_key("chat", 2000);

    // Mark it
    assert!(dedup2.should_process(&key2));
    assert!(!dedup2.should_process(&key2));

    // Wait for expiry
    tokio::time::sleep(Duration::from_millis(1200)).await;

    // After TTL expires, the key should be reprocessable
    assert!(
        dedup2.should_process(&key2),
        "Key should be reprocessable after TTL expires"
    );
}

/// Verify that entries with different TTL windows behave correctly.
#[tokio::test]
async fn test_dedup_short_ttl_vs_long_ttl() {
    let short_dedup = MessageDeduplicator::new(
        Duration::from_millis(500),   // 500ms window
        Duration::from_secs(60),
    );
    let long_dedup = MessageDeduplicator::new(
        Duration::from_secs(60),      // 60s window
        Duration::from_secs(60),
    );

    let key = make_key("chat", 3000);

    // Both mark the key
    assert!(short_dedup.should_process(&key));
    assert!(long_dedup.should_process(&key));

    // Both should reject immediately
    assert!(!short_dedup.should_process(&key));
    assert!(!long_dedup.should_process(&key));

    // Wait past the short TTL
    tokio::time::sleep(Duration::from_millis(700)).await;

    // Short TTL should allow reprocessing
    assert!(
        short_dedup.should_process(&key),
        "Short TTL deduplicator should allow reprocessing after 700ms"
    );

    // Long TTL should still reject
    assert!(
        !long_dedup.should_process(&key),
        "Long TTL deduplicator should still reject after 700ms"
    );
}

/// Verify that mark_processed also respects TTL.
#[tokio::test]
async fn test_mark_processed_respects_ttl() {
    let dedup = MessageDeduplicator::new(
        Duration::from_secs(1),
        Duration::from_secs(60),
    );

    let key = make_key("playback", 4000);

    // Mark processed explicitly
    dedup.mark_processed(key.clone());
    assert!(
        !dedup.should_process(&key),
        "Should reject after mark_processed"
    );

    // Wait for TTL
    tokio::time::sleep(Duration::from_millis(1200)).await;

    assert!(
        dedup.should_process(&key),
        "Should allow reprocessing after TTL expires on mark_processed entry"
    );
}

/// Concurrent should_process with TTL: exactly one succeeds per TTL window.
#[tokio::test]
async fn test_concurrent_should_process_per_ttl_window() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dedup = Arc::new(MessageDeduplicator::new(
        Duration::from_secs(1),
        Duration::from_secs(60),
    ));
    let key = make_key("sync", 5000);

    // Window 1: 10 concurrent callers
    let success_count = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(tokio::sync::Barrier::new(10));

    let mut handles = Vec::new();
    for _ in 0..10 {
        let dedup = dedup.clone();
        let key = key.clone();
        let count = success_count.clone();
        let barrier = barrier.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            if dedup.should_process(&key) {
                count.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    for h in handles {
        h.await.expect("task panicked");
    }

    assert_eq!(
        success_count.load(Ordering::Relaxed),
        1,
        "Exactly 1 should succeed in window 1"
    );

    // Wait for TTL to expire
    tokio::time::sleep(Duration::from_millis(1200)).await;

    // Window 2: should allow exactly one again
    let success_count2 = Arc::new(AtomicUsize::new(0));
    let barrier2 = Arc::new(tokio::sync::Barrier::new(10));

    let mut handles2 = Vec::new();
    for _ in 0..10 {
        let dedup = dedup.clone();
        let key = key.clone();
        let count = success_count2.clone();
        let barrier = barrier2.clone();
        handles2.push(tokio::spawn(async move {
            barrier.wait().await;
            if dedup.should_process(&key) {
                count.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    for h in handles2 {
        h.await.expect("task panicked");
    }

    assert_eq!(
        success_count2.load(Ordering::Relaxed),
        1,
        "Exactly 1 should succeed in window 2 (after TTL expiry)"
    );
}

/// len() and is_empty() should reflect TTL expiry.
#[tokio::test]
async fn test_len_reflects_ttl_expiry() {
    let dedup = MessageDeduplicator::new(
        Duration::from_secs(1),
        Duration::from_secs(60),
    );

    assert!(dedup.is_empty());
    assert_eq!(dedup.len(), 0);

    let key1 = make_key("chat", 1000);
    let key2 = make_key("chat", 2000);

    let _ = dedup.should_process(&key1);
    let _ = dedup.should_process(&key2);

    assert_eq!(dedup.len(), 2);
    assert!(!dedup.is_empty());

    // Wait for TTL
    tokio::time::sleep(Duration::from_millis(1200)).await;

    // After TTL, len should eventually drop to 0
    // (moka runs pending tasks lazily)
    assert_eq!(dedup.len(), 0, "len() should be 0 after TTL expiry");
    assert!(dedup.is_empty());
}
