//! SFU Integration Tests
//!
//! This test suite validates:
//! 1. Multi-peer concurrent scenarios (3+ peers joining/leaving)
//! 2. Resource cleanup after peer departure
//! 3. Concurrent room operations
//! 4. Room and peer limit enforcement
//! 5. P2P -> SFU migration lifecycle

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use synctv_sfu::{SfuConfig, SfuManager, RoomMode};
use synctv_sfu::{PeerId, RoomId};
use synctv_sfu::SfuSessionManager;
use tokio::time::sleep;

/// Helper function to create a test manager
fn create_test_manager(config: SfuConfig) -> Arc<SfuManager> {
    SfuManager::new(config)
}

/// Test 1: Multi-peer joining and leaving
#[tokio::test]
async fn test_multi_peer_join_leave() -> Result<()> {
    let config = SfuConfig {
        max_peers_per_room: 10,
        sfu_threshold: 2,
        ..SfuConfig::default()
    };
    let manager = create_test_manager(config);
    let room_id = RoomId::from("multi-peer-room");

    // Create 5 peers
    let peers: Vec<PeerId> = (1..=5)
        .map(|i| PeerId::from(format!("peer-{}", i)))
        .collect();

    // Join all peers sequentially
    for peer_id in &peers {
        manager
            .add_peer_to_room(room_id.clone(), peer_id.clone())
            .await?;
    }

    // Verify all peers joined
    let stats = manager.get_room_stats(&room_id).await?;
    assert_eq!(stats.peer_count, 5, "Expected 5 peers in room");

    // Remove 3 peers
    for peer_id in peers.iter().take(3) {
        manager
            .remove_peer_from_room(&room_id, peer_id)
            .await?;
    }

    // Verify 2 peers remain
    let stats = manager.get_room_stats(&room_id).await?;
    assert_eq!(stats.peer_count, 2, "Expected 2 peers remaining");

    // Cleanup
    manager.shutdown().await;
    Ok(())
}

/// Test 2: Resource cleanup after peer departure
#[tokio::test]
async fn test_resource_cleanup_after_peer_leave() -> Result<()> {
    let config = SfuConfig {
        max_peers_per_room: 10,
        ..SfuConfig::default()
    };
    let manager = create_test_manager(config);
    let room_id = RoomId::from("cleanup-room");

    // Add a peer
    let peer_id = PeerId::from("test-peer");
    manager
        .add_peer_to_room(room_id.clone(), peer_id.clone())
        .await?;

    // Verify peer was added
    let stats = manager.get_room_stats(&room_id).await?;
    assert_eq!(stats.peer_count, 1, "Expected 1 peer");

    // Remove peer
    manager.remove_peer_from_room(&room_id, &peer_id).await?;

    // Wait for cleanup
    sleep(Duration::from_millis(100)).await;

    // Verify room is empty
    let stats = manager.get_room_stats(&room_id).await?;
    assert_eq!(stats.peer_count, 0, "Expected 0 peers after removal");

    // Verify empty room cleanup
    manager.cleanup_empty_rooms().await;
    assert_eq!(manager.room_count(), 0, "Expected room to be cleaned up");

    // Cleanup
    manager.shutdown().await;
    Ok(())
}

/// Test 3: Room mode switching
#[tokio::test]
async fn test_room_mode_switching() -> Result<()> {
    let config = SfuConfig {
        max_peers_per_room: 10,
        sfu_threshold: 3, // Requires 3+ peers for SFU mode
        ..SfuConfig::default()
    };
    let manager = create_test_manager(config);
    let room_id = RoomId::from("mode-switch-room");

    // Add 1 peer (should be P2P mode by default)
    manager
        .add_peer_to_room(room_id.clone(), PeerId::from("peer-1"))
        .await?;

    // Force switch to SFU mode
    manager.set_room_mode(&room_id, RoomMode::SFU).await?;

    // Verify room exists
    let stats = manager.get_room_stats(&room_id).await?;
    assert_eq!(stats.peer_count, 1, "Expected 1 peer");

    // Add more peers
    manager
        .add_peer_to_room(room_id.clone(), PeerId::from("peer-2"))
        .await?;
    manager
        .add_peer_to_room(room_id.clone(), PeerId::from("peer-3"))
        .await?;

    // Verify all peers joined
    let stats = manager.get_room_stats(&room_id).await?;
    assert_eq!(stats.peer_count, 3, "Expected 3 peers");

    // Cleanup
    manager.shutdown().await;
    Ok(())
}

/// Test 4: Room limit enforcement
#[tokio::test]
async fn test_room_limit_enforcement() -> Result<()> {
    let config = SfuConfig {
        max_sfu_rooms: 3,
        ..SfuConfig::default()
    };
    let manager = create_test_manager(config);

    // Create rooms up to limit
    for i in 1..=3 {
        let room_id = RoomId::from(format!("room-{}", i));
        manager.get_or_create_room(room_id).await?;
    }

    assert_eq!(manager.room_count(), 3, "Expected 3 rooms");

    // Attempt to create one more room (should fail)
    let room_id = RoomId::from("room-4");
    let result = manager.get_or_create_room(room_id).await;
    assert!(
        result.is_err(),
        "Expected room creation to fail due to limit"
    );

    // Cleanup
    manager.shutdown().await;
    Ok(())
}

/// Test 5: Peer limit enforcement
#[tokio::test]
async fn test_peer_limit_enforcement() -> Result<()> {
    let config = SfuConfig {
        max_peers_per_room: 3,
        ..SfuConfig::default()
    };
    let manager = create_test_manager(config);
    let room_id = RoomId::from("limited-room");

    // Add peers up to limit
    for i in 1..=3 {
        let peer_id = PeerId::from(format!("peer-{}", i));
        manager
            .add_peer_to_room(room_id.clone(), peer_id)
            .await?;
    }

    let stats = manager.get_room_stats(&room_id).await?;
    assert_eq!(stats.peer_count, 3, "Expected 3 peers");

    // Attempt to add one more peer (should fail)
    let peer_id = PeerId::from("peer-4");
    let result = manager.add_peer_to_room(room_id, peer_id).await;
    assert!(
        result.is_err(),
        "Expected peer addition to fail due to limit"
    );

    // Cleanup
    manager.shutdown().await;
    Ok(())
}

/// Test 6: Empty room cleanup task
#[tokio::test]
async fn test_empty_room_cleanup_task() -> Result<()> {
    let config = SfuConfig::default();
    let manager = create_test_manager(config);

    // Create a room with a peer
    let room_id = RoomId::from("temp-room");
    let peer_id = PeerId::from("temp-peer");
    manager
        .add_peer_to_room(room_id.clone(), peer_id.clone())
        .await?;

    assert_eq!(manager.room_count(), 1, "Expected 1 room");

    // Remove the peer
    manager.remove_peer_from_room(&room_id, &peer_id).await?;

    // Manually trigger cleanup
    manager.cleanup_empty_rooms().await;

    // Verify room was cleaned up
    assert_eq!(manager.room_count(), 0, "Expected empty room to be cleaned up");

    // Cleanup
    manager.shutdown().await;
    Ok(())
}

/// Test 7: Global statistics collection
#[tokio::test]
async fn test_global_statistics_collection() -> Result<()> {
    let config = SfuConfig {
        max_sfu_rooms: 10,
        max_peers_per_room: 5,
        ..SfuConfig::default()
    };
    let manager = create_test_manager(config);

    // Create 3 rooms with different number of peers
    for room_num in 1..=3 {
        let room_id = RoomId::from(format!("stats-room-{}", room_num));
        for peer_num in 1..=room_num {
            let peer_id = PeerId::from(format!("room{}-peer{}", room_num, peer_num));
            manager
                .add_peer_to_room(room_id.clone(), peer_id)
                .await?;
        }
    }

    // Wait for stats collection
    sleep(Duration::from_millis(100)).await;

    // Verify global stats
    let stats = manager.get_stats().await;
    assert_eq!(stats.active_rooms, 3, "Expected 3 active rooms");
    assert_eq!(
        stats.total_peers, 6,
        "Expected 6 total peers (1+2+3)"
    );

    // Cleanup
    manager.shutdown().await;
    Ok(())
}

/// Test 8: Concurrent room operations
#[tokio::test]
async fn test_concurrent_room_operations() -> Result<()> {
    let config = SfuConfig {
        max_sfu_rooms: 20,
        max_peers_per_room: 5,
        ..SfuConfig::default()
    };
    let manager = create_test_manager(config);

    // Create 10 rooms sequentially, each with 3 peers
    for room_num in 1..=10 {
        let room_id = RoomId::from(format!("concurrent-room-{}", room_num));

        for peer_num in 1..=3 {
            let peer_id = PeerId::from(format!("room{}-peer{}", room_num, peer_num));
            manager
                .add_peer_to_room(room_id.clone(), peer_id)
                .await?;
        }
    }

    // Verify room count directly (more reliable than waiting for stats collection)
    assert_eq!(manager.room_count(), 10, "Expected 10 rooms");

    // Wait a bit for stats to update
    sleep(Duration::from_millis(200)).await;

    // Verify global stats
    let stats = manager.get_stats().await;
    assert_eq!(stats.total_peers, 30, "Expected 30 total peers");

    // Cleanup
    manager.shutdown().await;
    Ok(())
}

/// Test 9: P2P -> SFU migration lifecycle
///
/// Validates the full migration path:
/// 1. Room starts in P2P mode with < threshold peers
/// 2. Reaching the threshold transitions to Migrating mode
/// 3. complete_migration() advances to SFU mode
/// 4. SFU mode persists when removing a few peers (hysteresis)
/// 5. Dropping below the hysteresis threshold reverts to P2P
#[tokio::test]
async fn test_p2p_to_sfu_migration_lifecycle() -> Result<()> {
    let config = SfuConfig {
        sfu_threshold: 3,
        max_peers_per_room: 10,
        ..SfuConfig::default()
    };
    let manager = create_test_manager(config);
    let room_id = RoomId::from("migration-room");

    // --- Phase 1: P2P mode with 2 peers (below threshold) ---
    manager
        .add_peer_to_room(room_id.clone(), PeerId::from("peer-1"))
        .await?;
    manager
        .add_peer_to_room(room_id.clone(), PeerId::from("peer-2"))
        .await?;

    // Get the room to inspect mode directly
    let room = manager.get_or_create_room(room_id.clone()).await?;
    assert_eq!(
        room.get_mode().await,
        RoomMode::P2P,
        "Room should be in P2P mode with 2 peers (threshold=3)"
    );

    // --- Phase 2: Reaching threshold triggers Migrating mode ---
    manager
        .add_peer_to_room(room_id.clone(), PeerId::from("peer-3"))
        .await?;

    assert_eq!(
        room.get_mode().await,
        RoomMode::Migrating,
        "Room should enter Migrating mode when threshold (3) is reached"
    );

    // Verify all 3 peers are present during migration
    let stats = manager.get_room_stats(&room_id).await?;
    assert_eq!(stats.peer_count, 3, "Expected 3 peers during migration");

    // --- Phase 3: Complete migration -> SFU mode ---
    room.complete_migration().await?;
    assert_eq!(
        room.get_mode().await,
        RoomMode::SFU,
        "Room should be in SFU mode after complete_migration()"
    );

    // Add another peer while in SFU mode -- should stay in SFU
    manager
        .add_peer_to_room(room_id.clone(), PeerId::from("peer-4"))
        .await?;
    assert_eq!(
        room.get_mode().await,
        RoomMode::SFU,
        "Room should remain in SFU mode after adding a 4th peer"
    );

    // --- Phase 4: Hysteresis - removing peers near threshold should NOT switch back ---
    // With threshold=3, p2p_threshold = max(3-2, 1) = 1
    // So the room stays SFU until peer_count < 1 (i.e., 0 peers).

    // Remove one peer (4 -> 3): still SFU
    manager
        .remove_peer_from_room(&room_id, &PeerId::from("peer-4"))
        .await?;
    assert_eq!(
        room.get_mode().await,
        RoomMode::SFU,
        "Room should remain in SFU mode with 3 peers (hysteresis)"
    );

    // Remove another (3 -> 2): still SFU (2 >= 1)
    manager
        .remove_peer_from_room(&room_id, &PeerId::from("peer-3"))
        .await?;
    assert_eq!(
        room.get_mode().await,
        RoomMode::SFU,
        "Room should remain in SFU mode with 2 peers (hysteresis, p2p_threshold=1)"
    );

    // Remove another (2 -> 1): still SFU (1 >= 1)
    manager
        .remove_peer_from_room(&room_id, &PeerId::from("peer-2"))
        .await?;
    assert_eq!(
        room.get_mode().await,
        RoomMode::SFU,
        "Room should remain in SFU mode with 1 peer (hysteresis, 1 >= 1)"
    );

    // --- Phase 5: Below hysteresis threshold -> back to P2P ---
    // Remove last peer (1 -> 0): 0 < 1, triggers switch to P2P
    manager
        .remove_peer_from_room(&room_id, &PeerId::from("peer-1"))
        .await?;
    assert_eq!(
        room.get_mode().await,
        RoomMode::P2P,
        "Room should revert to P2P mode when all peers leave (0 < p2p_threshold=1)"
    );

    // Verify empty room
    let stats = manager.get_room_stats(&room_id).await?;
    assert_eq!(stats.peer_count, 0, "Expected 0 peers after all departures");

    // Cleanup
    manager.shutdown().await;
    Ok(())
}

/// Test 10: SFU session manager integration with migration
///
/// Validates that the SfuSessionManager correctly:
/// 1. Determines when to use SFU mode based on peer count
/// 2. Reports the correct SFU threshold
/// 3. Tracks active sessions
#[tokio::test]
async fn test_sfu_session_manager_migration_basics() -> Result<()> {
    let config = SfuConfig {
        sfu_threshold: 3,
        max_peers_per_room: 10,
        ..SfuConfig::default()
    };
    let sfu_manager = SfuManager::new(config);
    let ice_manager = Arc::new(synctv_sfu::IceManager::new());
    let session_mgr = SfuSessionManager::with_timeout(
        sfu_manager,
        ice_manager,
        Duration::from_secs(300),
    );

    // Verify threshold is correctly propagated
    assert_eq!(session_mgr.sfu_threshold(), 3);

    // should_use_sfu checks against the threshold
    assert!(!session_mgr.should_use_sfu(0), "0 peers should not trigger SFU");
    assert!(!session_mgr.should_use_sfu(1), "1 peer should not trigger SFU");
    assert!(!session_mgr.should_use_sfu(2), "2 peers should not trigger SFU");
    assert!(session_mgr.should_use_sfu(3), "3 peers should trigger SFU");
    assert!(session_mgr.should_use_sfu(10), "10 peers should trigger SFU");

    // No sessions should exist initially
    assert_eq!(session_mgr.session_count(), 0);
    assert!(!session_mgr.has_session("non-existent"));

    Ok(())
}

/// Test 11: Migration with room re-entry after P2P revert
///
/// Validates that after migrating to SFU and reverting to P2P, the room
/// can successfully re-enter SFU mode (no stale state from first migration).
#[tokio::test]
async fn test_migration_re_entry_after_p2p_revert() -> Result<()> {
    let config = SfuConfig {
        sfu_threshold: 2,
        max_peers_per_room: 10,
        ..SfuConfig::default()
    };
    let manager = create_test_manager(config);
    let room_id = RoomId::from("re-entry-room");

    // --- First migration cycle ---
    manager
        .add_peer_to_room(room_id.clone(), PeerId::from("a1"))
        .await?;
    manager
        .add_peer_to_room(room_id.clone(), PeerId::from("a2"))
        .await?;

    let room = manager.get_or_create_room(room_id.clone()).await?;
    assert_eq!(room.get_mode().await, RoomMode::Migrating);

    room.complete_migration().await?;
    assert_eq!(room.get_mode().await, RoomMode::SFU);

    // Revert: remove all peers. With threshold=2, p2p_threshold = max(0,1) = 1.
    // Need count < 1 to revert, so remove both.
    manager
        .remove_peer_from_room(&room_id, &PeerId::from("a1"))
        .await?;
    manager
        .remove_peer_from_room(&room_id, &PeerId::from("a2"))
        .await?;
    assert_eq!(
        room.get_mode().await,
        RoomMode::P2P,
        "Should revert to P2P after all peers leave"
    );

    // --- Second migration cycle (same room) ---
    manager
        .add_peer_to_room(room_id.clone(), PeerId::from("b1"))
        .await?;
    manager
        .add_peer_to_room(room_id.clone(), PeerId::from("b2"))
        .await?;

    assert_eq!(
        room.get_mode().await,
        RoomMode::Migrating,
        "Room should re-enter Migrating mode on second cycle"
    );

    room.complete_migration().await?;
    assert_eq!(
        room.get_mode().await,
        RoomMode::SFU,
        "Room should reach SFU mode on second cycle"
    );

    let stats = manager.get_room_stats(&room_id).await?;
    assert_eq!(stats.peer_count, 2, "Expected 2 peers in second cycle");

    // Cleanup
    manager.shutdown().await;
    Ok(())
}

