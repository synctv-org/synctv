//! Database deadlock detection tests
//!
//! Tests verify that the application properly detects and handles deadlocks.
//!
//! Requires Docker for testcontainers.

use sqlx::PgPool;
use std::sync::Arc;
use synctv_core::{
    models::{
        MemberStatus, Room, RoomId, RoomMember, RoomRole, SignupMethod, User, UserId, UserStatus,
    },
    repository::{RoomMemberRepository, RoomRepository, UserRepository},
};
use synctv_core_testing::{create_test_database_with_options_and_label, ok, TestDatabase};
use tokio::sync::Barrier;

async fn create_test_pool() -> TestDatabase {
    create_test_database_with_options_and_label(
        "synctv_test",
        "database-deadlock",
        20,
        std::time::Duration::from_secs(30),
    )
    .await
}

/// Create a test user in the database (required for FK constraints)
async fn create_test_user(pool: &PgPool, user_id: &UserId) -> UserId {
    let username = format!("test_user_{user_id}");
    let user = User {
        id: *user_id,
        username,
        signup_method: SignupMethod::Email,
        role: synctv_core::models::UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };
    let user_repo = UserRepository::new(pool.clone());
    ok(user_repo.create(&user).await, "test user should be created").id
}

fn make_member(room_id: RoomId, user_id: UserId) -> RoomMember {
    RoomMember {
        room_id,
        user_id,
        role: RoomRole::Member,
        status: MemberStatus::Active,
        added_permissions: 0,
        removed_permissions: 0,
        admin_added_permissions: 0,
        admin_removed_permissions: 0,
        joined_at: chrono::Utc::now(),
        version: 0,
    }
}

fn make_room(creator_id: UserId) -> Room {
    Room {
        id: RoomId::new(),
        name: "Test Room".to_string(),
        description: "Test".to_string(),
        cover_file_reference_id: None,
        created_by: creator_id,
        status: synctv_core::models::RoomStatus::Active,
        is_banned: false,
        closed_at: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
        version: 0,
        last_activity_at: chrono::Utc::now(),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_deadlock_detection_opposite_lock_order() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator_id = create_test_user(pool, &UserId::new()).await;
    let room = make_room(creator_id);
    let room = ok(room_repo.create(&room).await, "room should be created");

    let user1 = create_test_user(pool, &UserId::new()).await;
    let user2 = create_test_user(pool, &UserId::new()).await;

    let member1 = make_member(room.id, user1);
    let member2 = make_member(room.id, user2);
    ok(
        member_repo.add(&member1).await,
        "first member should be added",
    );
    ok(
        member_repo.add(&member2).await,
        "second member should be added",
    );

    // Try to cause deadlock with opposite lock order
    let barrier = Arc::new(Barrier::new(2));

    let pool1 = pool.clone();
    let room_id1 = room.id;
    let user1_clone = user1;
    let user2_clone1 = user2;
    let barrier1 = barrier.clone();

    let handle1 = tokio::spawn(async move {
        barrier1.wait().await;

        let result: Result<(), sqlx::Error> = async {
            let mut tx = pool1.begin().await?;

            // Lock user1 first
            sqlx::query!(
                "UPDATE room_members SET added_permissions = $1
                 WHERE room_id = $2 AND user_id = $3",
                100_i64,
                room_id1.as_i64(),
                user1_clone.as_i64()
            )
            .execute(&mut *tx)
            .await?;

            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

            // Then lock user2
            sqlx::query!(
                "UPDATE room_members SET added_permissions = $1
                 WHERE room_id = $2 AND user_id = $3",
                200_i64,
                room_id1.as_i64(),
                user2_clone1.as_i64()
            )
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            Ok(())
        }
        .await;

        result
    });

    let pool2 = pool.clone();
    let room_id2 = room.id;
    let user1_clone2 = user1;
    let user2_clone2 = user2;
    let barrier2 = barrier.clone();

    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;

        let result: Result<(), sqlx::Error> = async {
            let mut tx = pool2.begin().await?;

            // Lock user2 first (opposite order)
            sqlx::query!(
                "UPDATE room_members SET added_permissions = $1
                 WHERE room_id = $2 AND user_id = $3",
                300_i64,
                room_id2.as_i64(),
                user2_clone2.as_i64()
            )
            .execute(&mut *tx)
            .await?;

            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

            // Then lock user1 (creates potential deadlock)
            sqlx::query!(
                "UPDATE room_members SET added_permissions = $1
                 WHERE room_id = $2 AND user_id = $3",
                400_i64,
                room_id2.as_i64(),
                user1_clone2.as_i64()
            )
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            Ok(())
        }
        .await;

        result
    });

    let result1 = ok(handle1.await, "first deadlock task should complete");
    let result2 = ok(handle2.await, "second deadlock task should complete");

    // At least one should fail with deadlock error, or both succeed
    let both_ok = result1.is_ok() && result2.is_ok();
    let has_deadlock = result1.is_err() || result2.is_err();

    if has_deadlock {
        let err1_str = format!("{result1:?}");
        let err2_str = format!("{result2:?}");
        let combined = format!("{err1_str} {err2_str}");

        // PostgreSQL deadlock error code is 40P01
        assert!(
            combined.contains("40P01") || combined.contains("deadlock"),
            "Expected deadlock error, got: {combined}"
        );
    } else {
        assert!(both_ok, "If no deadlock, both transactions should succeed");
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_deadlock_with_for_update_nowait() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator_id = create_test_user(pool, &UserId::new()).await;
    let room = make_room(creator_id);
    let room = ok(room_repo.create(&room).await, "room should be created");

    let user_id = create_test_user(pool, &UserId::new()).await;
    let member = make_member(room.id, user_id);
    ok(member_repo.add(&member).await, "member should be added");

    let barrier = Arc::new(Barrier::new(2));

    let pool1 = pool.clone();
    let room_id1 = room.id;
    let user_id1 = user_id;
    let barrier1 = barrier.clone();

    let handle1 = tokio::spawn(async move {
        barrier1.wait().await;

        let result: Result<(), sqlx::Error> = async {
            let mut tx = pool1.begin().await?;

            // Lock with FOR UPDATE
            sqlx::query!(
                r#"SELECT 1 AS "one!" FROM room_members
                 WHERE room_id = $1 AND user_id = $2
                 FOR UPDATE"#,
                room_id1.as_i64(),
                user_id1.as_i64()
            )
            .fetch_optional(&mut *tx)
            .await?;

            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            tx.commit().await?;
            Ok(())
        }
        .await;

        result
    });

    let pool2 = pool.clone();
    let room_id2 = room.id;
    let user_id2 = user_id;
    let barrier2 = barrier.clone();

    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        let result: Result<(), sqlx::Error> = async {
            let mut tx = pool2.begin().await?;

            // Try to lock with FOR UPDATE NOWAIT
            let result = sqlx::query!(
                r#"SELECT 1 AS "one!" FROM room_members
                 WHERE room_id = $1 AND user_id = $2
                 FOR UPDATE NOWAIT"#,
                room_id2.as_i64(),
                user_id2.as_i64()
            )
            .fetch_optional(&mut *tx)
            .await;

            match result {
                Ok(_) => {
                    tx.commit().await?;
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        .await;

        result
    });

    let result1 = ok(handle1.await, "first nowait task should complete");
    let result2 = ok(handle2.await, "second nowait task should complete");

    assert!(result1.is_ok(), "First transaction should succeed");

    // Second transaction should fail immediately due to NOWAIT
    if result2.is_err() {
        let err_msg = format!("{result2:?}");
        // Lock not available error (55P03)
        assert!(
            err_msg.contains("55P03") || err_msg.contains("could not obtain lock"),
            "Expected lock timeout error, got: {err_msg}"
        );
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_deadlock_avoidance_with_ordered_locks() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator_id = create_test_user(pool, &UserId::new()).await;
    let room = make_room(creator_id);
    let room = ok(room_repo.create(&room).await, "room should be created");

    let user1 = create_test_user(pool, &UserId::new()).await;
    let user2 = create_test_user(pool, &UserId::new()).await;

    // Ensure consistent ordering
    let (first_user, second_user) = if user1 < user2 {
        (user1, user2)
    } else {
        (user2, user1)
    };

    let member1 = make_member(room.id, first_user);
    let member2 = make_member(room.id, second_user);
    ok(
        member_repo.add(&member1).await,
        "first member should be added",
    );
    ok(
        member_repo.add(&member2).await,
        "second member should be added",
    );

    let barrier = Arc::new(Barrier::new(2));

    // Both transactions lock in the same order
    let pool1 = pool.clone();
    let room_id1 = room.id;
    let first_user1 = first_user;
    let second_user1 = second_user;
    let barrier1 = barrier.clone();

    let handle1 = tokio::spawn(async move {
        barrier1.wait().await;

        let result: Result<(), sqlx::Error> = async {
            let mut tx = pool1.begin().await?;

            // Lock in consistent order
            sqlx::query!(
                "UPDATE room_members SET added_permissions = $1
                 WHERE room_id = $2 AND user_id = $3",
                100_i64,
                room_id1.as_i64(),
                first_user1.as_i64()
            )
            .execute(&mut *tx)
            .await?;

            sqlx::query!(
                "UPDATE room_members SET added_permissions = $1
                 WHERE room_id = $2 AND user_id = $3",
                200_i64,
                room_id1.as_i64(),
                second_user1.as_i64()
            )
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            Ok(())
        }
        .await;

        result
    });

    let pool2 = pool.clone();
    let room_id2 = room.id;
    let first_user2 = first_user;
    let second_user2 = second_user;
    let barrier2 = barrier.clone();

    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;

        let result: Result<(), sqlx::Error> = async {
            let mut tx = pool2.begin().await?;

            // Lock in same consistent order
            sqlx::query!(
                "UPDATE room_members SET added_permissions = $1
                 WHERE room_id = $2 AND user_id = $3",
                300_i64,
                room_id2.as_i64(),
                first_user2.as_i64()
            )
            .execute(&mut *tx)
            .await?;

            sqlx::query!(
                "UPDATE room_members SET added_permissions = $1
                 WHERE room_id = $2 AND user_id = $3",
                400_i64,
                room_id2.as_i64(),
                second_user2.as_i64()
            )
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            Ok(())
        }
        .await;

        result
    });

    let result1 = ok(handle1.await, "first ordered-lock task should complete");
    let result2 = ok(handle2.await, "second ordered-lock task should complete");

    // With consistent ordering, no deadlock should occur
    // One will complete, then the other
    assert!(
        result1.is_ok() || result2.is_ok(),
        "At least one should succeed"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_transaction_timeout_prevents_indefinite_wait() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator_id = create_test_user(pool, &UserId::new()).await;
    let room = make_room(creator_id);
    let room = ok(room_repo.create(&room).await, "room should be created");

    let user_id = create_test_user(pool, &UserId::new()).await;
    let member = make_member(room.id, user_id);
    ok(member_repo.add(&member).await, "member should be added");

    let pool1 = pool.clone();
    let room_id1 = room.id;
    let user_id1 = user_id;

    let tx_handle = tokio::spawn(async move {
        let mut tx = pool1.begin().await?;

        // Lock the row
        sqlx::query!(
            r#"SELECT 1 AS "one!" FROM room_members
             WHERE room_id = $1 AND user_id = $2
             FOR UPDATE"#,
            room_id1.as_i64(),
            user_id1.as_i64()
        )
        .fetch_optional(&mut *tx)
        .await?;

        // Hold lock for 2 seconds
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        tx.commit().await?;
        Ok::<(), sqlx::Error>(())
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let pool2 = pool.clone();
    let room_id2 = room.id;
    let user_id2 = user_id;

    let timeout_result = tokio::time::timeout(tokio::time::Duration::from_secs(1), async move {
        let mut tx = pool2.begin().await?;

        sqlx::query!(
            r#"SELECT 1 AS "one!" FROM room_members
                 WHERE room_id = $1 AND user_id = $2
                 FOR UPDATE"#,
            room_id2.as_i64(),
            user_id2.as_i64()
        )
        .fetch_optional(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok::<(), sqlx::Error>(())
    })
    .await;

    // Should timeout waiting for lock
    assert!(timeout_result.is_err(), "Should timeout waiting for lock");

    let tx_result = ok(tx_handle.await, "first transaction task should complete");
    ok(tx_result, "first transaction should finish");
}
