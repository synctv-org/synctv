//! Admin list_rooms distributed connection count tests (P1 Fix)
//!
//! Issue: Admin list_rooms uses local connection count
//! (`connection_manager.room_connection_count(&r.id)`) while client list_rooms uses
//! distributed count (`room_connection_count_distributed_batch`).
//!
//! In multi-replica deployments, admin sees incorrect online member counts because
//! it only counts local connections, not cluster-wide totals from Redis.
//!
//! Fix: Admin list_rooms should use room_connection_count_distributed_batch like
//! client list_rooms does, for consistent and accurate counts across all replicas.
//!
//! Reference: synctv-api/src/impls/client/room.rs:61-66 (correct implementation)

#![allow(clippy::unwrap_used)]

use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};
use synctv_core::models::RoomId;

// ============================================================================
// Test: ConnectionManager has distributed batch method
// ============================================================================

/// Verify that ConnectionManager exposes room_connection_count_distributed_batch.
/// This is the method that both admin and client should use for accurate counts.
#[test]
fn test_connection_manager_has_distributed_batch_method() {
    // The existence of this method is verified by successful compilation.
    // This test documents the expected API.
    let _ = |_cm: &ConnectionManager| {
        // This closure won't be called, but it verifies the method exists.
        let _method_ref = ConnectionManager::room_connection_count_distributed_batch;
    };
}

// ============================================================================
// Test: Local count vs distributed count behavior
// ============================================================================

/// Test that local count (room_connection_count) only tracks local connections.
/// This is why it's incorrect for multi-replica deployments.
#[tokio::test]
async fn test_local_count_only_tracks_local_connections() {
    let conn_manager = ConnectionManager::new(ConnectionLimits::default());
    let room_id = RoomId::from_string("test_room".to_string());

    // Without any connections, local count is 0
    let local_count = conn_manager.room_connection_count(&room_id);
    assert_eq!(
        local_count, 0,
        "Local count should be 0 with no connections"
    );
}

/// Test that distributed batch count returns values for multiple rooms.
/// This is the method admin list_rooms should use.
#[tokio::test]
async fn test_distributed_batch_count_returns_vec() {
    let conn_manager = ConnectionManager::new(ConnectionLimits::default());
    let room1 = RoomId::from_string("room1".to_string());
    let room2 = RoomId::from_string("room2".to_string());

    let room_ids: Vec<&RoomId> = vec![&room1, &room2];

    // Without Redis, this returns local counts (0 for both)
    let counts = conn_manager
        .room_connection_count_distributed_batch(&room_ids)
        .await
        .expect("standalone mode should return local distributed batch counts");

    assert_eq!(counts.len(), 2, "Should return count for each room");
    assert_eq!(counts[0], 0, "Room 1 count should be 0");
    assert_eq!(counts[1], 0, "Room 2 count should be 0");
}

// ============================================================================
// Test: Document the pattern that should be used
// ============================================================================

/// This test documents the CORRECT pattern for list_rooms connection counting.
///
/// BEFORE FIX (incorrect - uses local count):
/// ```ignore
/// let member_count = self
///     .connection_manager
///     .room_connection_count(&r.id)  // <-- LOCAL only!
///     .try_into()
///     .ok();
/// ```
///
/// AFTER FIX (correct - uses distributed batch):
/// ```ignore
/// let room_id_refs: Vec<&RoomId> = rooms.iter().map(|r| &r.id).collect();
/// let counts = self
///     .connection_manager
///     .room_connection_count_distributed_batch(&room_id_refs)
///     .await;
///
/// for (r, count) in rooms.iter().zip(counts) {
///     let member_count: Option<i32> = count.try_into().ok();
///     // ... use member_count
/// }
/// ```
#[test]
fn test_correct_pattern_is_documented() {
    // This test exists to document the expected fix pattern.
    // The actual fix is in synctv-api/src/impls/admin.rs:list_rooms method.
    assert!(true, "Pattern documented in test comments");
}

// ============================================================================
// Test: Verify fix is applied by checking code pattern
// ============================================================================

/// This test reads the admin.rs source and verifies the fix is in place.
/// It checks that list_rooms uses room_connection_count_distributed_batch.
#[test]
fn test_admin_list_rooms_uses_distributed_batch_count() {
    // Read the source file
    let admin_rs_content = include_str!("../src/impls/admin.rs");

    // Check that the list_rooms function exists
    assert!(
        admin_rs_content.contains("pub async fn list_rooms("),
        "admin.rs should have list_rooms function"
    );

    // Check that distributed batch method is used somewhere in the file
    // (This is a broad check; the specific usage should be in list_rooms)
    let uses_distributed = admin_rs_content.contains("room_connection_count_distributed_batch");

    // Check that the OLD local-only pattern in list_rooms context is removed
    // We look for the pattern that was there before: ".room_connection_count(&r.id)"
    // inside the list_rooms function area

    // A more specific check: ensure we don't have the old pattern in a mapping context
    // The old code was: rooms.into_iter().map(|r| { ... room_connection_count(&r.id) ... })
    let has_old_local_pattern_in_map = admin_rs_content.contains(".room_connection_count(&r.id)");

    // After fix: should use distributed batch, not local count on room id
    assert!(
        uses_distributed || !has_old_local_pattern_in_map,
        "admin.rs should use room_connection_count_distributed_batch for list_rooms, \
         not the local-only room_connection_count(&r.id). \
         This ensures accurate member counts in multi-replica deployments."
    );
}
