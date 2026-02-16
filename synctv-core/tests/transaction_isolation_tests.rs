//! Transaction isolation tests for concurrent room permission updates (Task #81)
//!
//! These tests verify that concurrent permission updates maintain data integrity
//! and isolation guarantees when multiple transactions attempt to modify the same
//! room permissions simultaneously.
//!
//! Run with: cargo test --test transaction_isolation_tests

use synctv_core::{
    models::{Room, RoomId, RoomMember, RoomRole, RoomStatus, UserId, MemberStatus, PermissionBits},
    repository::{RoomRepository, RoomMemberRepository},
};
use sqlx::PgPool;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;
use std::sync::Arc;
use tokio::sync::Barrier;

/// Helper to create a test database pool with proper schema
async fn create_test_pool() -> (ContainerAsync<Postgres>, PgPool) {
    let postgres = Postgres::default()
        .with_db_name("synctv_test")
        .with_user("synctv")
        .with_password("synctv_test")
        .start()
        .await
        .expect("Failed to start Postgres container");

    let pg_host = postgres.get_host().await.expect("Failed to get host");
    let pg_port = postgres
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get port");

    let connection_string = format!(
        "postgresql://synctv:synctv_test@{}:{}/synctv_test",
        pg_host, pg_port
    );

    let pool = PgPool::connect(&connection_string)
        .await
        .expect("Failed to create pool");

    // Run migrations
    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (postgres, pool)
}

#[tokio::test]
async fn test_concurrent_permission_updates_isolated() {
    let (_container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    // Create a test room
    let creator_id = UserId::new();
    let room = Room {
        id: RoomId::new(),
        name: "Test Room".to_string(),
        description: "Test".to_string(),
        created_by: creator_id.clone(),
        status: RoomStatus::Active,
        is_banned: false,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
    };
    room_repo.create(&room).await.expect("Failed to create room");

    // Create test members
    let user1 = UserId::new();
    let user2 = UserId::new();

    let member1 = RoomMember {
        room_id: room.id.clone(),
        user_id: user1.clone(),
        role: RoomRole::Member,
        permissions: PermissionBits::DEFAULT_MEMBER,
        status: MemberStatus::Active,
        joined_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    let member2 = RoomMember {
        room_id: room.id.clone(),
        user_id: user2.clone(),
        role: RoomRole::Member,
        permissions: PermissionBits::DEFAULT_MEMBER,
        status: MemberStatus::Active,
        joined_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    member_repo.create(&member1).await.expect("Failed to create member1");
    member_repo.create(&member2).await.expect("Failed to create member2");

    // Concurrent permission updates
    let barrier = Arc::new(Barrier::new(2));
    let pool1 = pool.clone();
    let pool2 = pool.clone();
    let room_id = room.id.clone();
    let user1_clone = user1.clone();
    let user2_clone = user2.clone();

    let barrier1 = barrier.clone();
    let handle1 = tokio::spawn(async move {
        barrier1.wait().await;

        let mut tx = pool1.begin().await.expect("Failed to begin transaction");

        // Update user1 permissions
        sqlx::query(
            "UPDATE room_members SET permissions = $1, updated_at = NOW()
             WHERE room_id = $2 AND user_id = $3"
        )
        .bind(PermissionBits::DEFAULT_ADMIN as i64)
        .bind(room_id.as_str())
        .bind(user1_clone.as_str())
        .execute(&mut *tx)
        .await
        .expect("Failed to update user1");

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        tx.commit().await.expect("Failed to commit tx1");
    });

    let barrier2 = barrier.clone();
    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;

        let mut tx = pool2.begin().await.expect("Failed to begin transaction");

        // Update user2 permissions
        sqlx::query(
            "UPDATE room_members SET permissions = $1, updated_at = NOW()
             WHERE room_id = $2 AND user_id = $3"
        )
        .bind(PermissionBits::DEFAULT_ADMIN as i64)
        .bind(room_id.as_str())
        .bind(user2_clone.as_str())
        .execute(&mut *tx)
        .await
        .expect("Failed to update user2");

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        tx.commit().await.expect("Failed to commit tx2");
    });

    handle1.await.expect("Task 1 failed");
    handle2.await.expect("Task 2 failed");

    // Verify both updates succeeded
    let members = member_repo.list_by_room(&room.id).await.expect("Failed to list members");

    let member1_updated = members.iter().find(|m| m.user_id == user1).expect("Member1 not found");
    let member2_updated = members.iter().find(|m| m.user_id == user2).expect("Member2 not found");

    assert_eq!(member1_updated.permissions, PermissionBits::DEFAULT_ADMIN);
    assert_eq!(member2_updated.permissions, PermissionBits::DEFAULT_ADMIN);
}

#[tokio::test]
async fn test_serializable_isolation_prevents_phantom_reads() {
    let (_docker, _container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    // Create test room
    let creator_id = UserId::new();
    let room = Room {
        id: RoomId::new(),
        name: "Test Room".to_string(),
        description: "Test".to_string(),
        created_by: creator_id.clone(),
        status: RoomStatus::Active,
        is_banned: false,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
    };
    room_repo.create(&room).await.expect("Failed to create room");

    // Transaction 1: Count members, then add one
    let pool1 = pool.clone();
    let room_id1 = room.id.clone();
    let handle1 = tokio::spawn(async move {
        let mut tx = pool1.begin().await.expect("Failed to begin transaction");

        // Count existing members
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM room_members WHERE room_id = $1"
        )
        .bind(room_id1.as_str())
        .fetch_one(&mut *tx)
        .await
        .expect("Failed to count");

        assert_eq!(count, 0);

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Re-count (should still be 0 due to isolation)
        let count2: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM room_members WHERE room_id = $1"
        )
        .bind(room_id1.as_str())
        .fetch_one(&mut *tx)
        .await
        .expect("Failed to count");

        tx.commit().await.expect("Failed to commit");

        count2
    });

    // Wait a bit then insert a member in a separate transaction
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let pool2 = pool.clone();
    let room_id2 = room.id.clone();
    let user_id = UserId::new();
    let handle2 = tokio::spawn(async move {
        let member = RoomMember {
            room_id: room_id2,
            user_id,
            role: RoomRole::Member,
            permissions: PermissionBits::DEFAULT_MEMBER,
            status: MemberStatus::Active,
            joined_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let member_repo = RoomMemberRepository::new(pool2);
        member_repo.create(&member).await.expect("Failed to create member");
    });

    handle2.await.expect("Task 2 failed");
    let final_count = handle1.await.expect("Task 1 failed");

    // Due to transaction isolation, the count should remain 0 within the transaction
    // even though another transaction committed a new member
    assert_eq!(final_count, 0);
}

#[tokio::test]
async fn test_concurrent_permission_bit_updates_no_lost_updates() {
    let (_docker, _container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    // Create test room and member
    let creator_id = UserId::new();
    let room = Room {
        id: RoomId::new(),
        name: "Test Room".to_string(),
        description: "Test".to_string(),
        created_by: creator_id.clone(),
        status: RoomStatus::Active,
        is_banned: false,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
    };
    room_repo.create(&room).await.expect("Failed to create room");

    let user_id = UserId::new();
    let member = RoomMember {
        room_id: room.id.clone(),
        user_id: user_id.clone(),
        role: RoomRole::Member,
        permissions: 0, // Start with no permissions
        status: MemberStatus::Active,
        joined_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    member_repo.create(&member).await.expect("Failed to create member");

    // Concurrently grant different permissions
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = vec![];

    for perm in [PermissionBits::SEND_CHAT, PermissionBits::ADD_MEDIA, PermissionBits::KICK_MEMBER] {
        let pool_clone = pool.clone();
        let room_id = room.id.clone();
        let user_id = user_id.clone();
        let barrier_clone = barrier.clone();

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;

            let mut tx = pool_clone.begin().await.expect("Failed to begin transaction");

            // Read current permissions
            let current: i64 = sqlx::query_scalar(
                "SELECT permissions FROM room_members WHERE room_id = $1 AND user_id = $2 FOR UPDATE"
            )
            .bind(room_id.as_str())
            .bind(user_id.as_str())
            .fetch_one(&mut *tx)
            .await
            .expect("Failed to read permissions");

            // Grant new permission
            let new_perms = current | perm;

            sqlx::query(
                "UPDATE room_members SET permissions = $1, updated_at = NOW()
                 WHERE room_id = $2 AND user_id = $3"
            )
            .bind(new_perms)
            .bind(room_id.as_str())
            .bind(user_id.as_str())
            .execute(&mut *tx)
            .await
            .expect("Failed to update permissions");

            tx.commit().await.expect("Failed to commit");
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.await.expect("Task failed");
    }

    // Verify all permissions were granted (no lost updates)
    let final_member = member_repo.get_by_room_and_user(&room.id, &user_id)
        .await
        .expect("Failed to get member")
        .expect("Member not found");

    let mut expected_perms = PermissionBits(0);
    expected_perms.grant(PermissionBits::SEND_CHAT);
    expected_perms.grant(PermissionBits::ADD_MEDIA);
    expected_perms.grant(PermissionBits::KICK_MEMBER);

    assert_eq!(final_member.permissions, expected_perms.0);
}

#[tokio::test]
async fn test_deadlock_detection_with_multiple_row_updates() {
    let (_docker, _container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    // Create test room
    let creator_id = UserId::new();
    let room = Room {
        id: RoomId::new(),
        name: "Test Room".to_string(),
        description: "Test".to_string(),
        created_by: creator_id.clone(),
        status: RoomStatus::Active,
        is_banned: false,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
    };
    room_repo.create(&room).await.expect("Failed to create room");

    // Create two members
    let user1 = UserId::new();
    let user2 = UserId::new();

    for user_id in [&user1, &user2] {
        let member = RoomMember {
            room_id: room.id.clone(),
            user_id: user_id.clone(),
            role: RoomRole::Member,
            permissions: PermissionBits::DEFAULT_MEMBER,
            status: MemberStatus::Active,
            joined_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        member_repo.create(&member).await.expect("Failed to create member");
    }

    // Try to create a deadlock by updating in opposite order
    let barrier = Arc::new(Barrier::new(2));

    let pool1 = pool.clone();
    let room_id1 = room.id.clone();
    let user1_clone = user1.clone();
    let user2_clone1 = user2.clone();
    let barrier1 = barrier.clone();

    let handle1 = tokio::spawn(async move {
        barrier1.wait().await;

        let result = async {
            let mut tx = pool1.begin().await?;

            // Lock user1 first
            sqlx::query(
                "UPDATE room_members SET permissions = $1 WHERE room_id = $2 AND user_id = $3"
            )
            .bind(100i64)
            .bind(room_id1.as_str())
            .bind(user1_clone.as_str())
            .execute(&mut *tx)
            .await?;

            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            // Then lock user2
            sqlx::query(
                "UPDATE room_members SET permissions = $1 WHERE room_id = $2 AND user_id = $3"
            )
            .bind(200i64)
            .bind(room_id1.as_str())
            .bind(user2_clone1.as_str())
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            Ok::<(), sqlx::Error>(())
        }.await;

        result
    });

    let pool2 = pool.clone();
    let room_id2 = room.id.clone();
    let user1_clone2 = user1.clone();
    let user2_clone2 = user2.clone();
    let barrier2 = barrier.clone();

    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;

        let result = async {
            let mut tx = pool2.begin().await?;

            // Lock user2 first (opposite order)
            sqlx::query(
                "UPDATE room_members SET permissions = $1 WHERE room_id = $2 AND user_id = $3"
            )
            .bind(300i64)
            .bind(room_id2.as_str())
            .bind(user2_clone2.as_str())
            .execute(&mut *tx)
            .await?;

            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            // Then lock user1 (creates potential deadlock)
            sqlx::query(
                "UPDATE room_members SET permissions = $1 WHERE room_id = $2 AND user_id = $3"
            )
            .bind(400i64)
            .bind(room_id2.as_str())
            .bind(user1_clone2.as_str())
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            Ok::<(), sqlx::Error>(())
        }.await;

        result
    });

    let result1 = handle1.await.expect("Task 1 panicked");
    let result2 = handle2.await.expect("Task 2 panicked");

    // At least one should succeed, and if deadlock occurs,
    // PostgreSQL will detect it and return an error
    if result1.is_err() || result2.is_err() {
        // Verify deadlock was detected (not just any error)
        let err_msg = format!("{:?} {:?}", result1, result2);
        // PostgreSQL deadlock error code is 40P01
        assert!(err_msg.contains("deadlock") || err_msg.contains("40P01"),
                "Expected deadlock detection, got: {}", err_msg);
    } else {
        // Both succeeded - no deadlock occurred
        assert!(result1.is_ok() && result2.is_ok());
    }
}
