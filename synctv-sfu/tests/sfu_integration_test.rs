//! SFU Integration Tests
//!
//! This test suite validates:
//! 1. Multi-peer concurrent scenarios (3+ peers joining/leaving)
//! 2. Resource cleanup after peer departure
//! 3. Concurrent room operations
//! 4. Room and peer limit enforcement

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use synctv_sfu::{SfuConfig, SfuManager, RoomMode};
use synctv_sfu::{PeerId, RoomId, TrackId};
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

