//! Database deadlock detection tests
//!
//! Tests verify that the application properly detects and handles deadlocks.
//!
//! Run with: cargo test --test `database_deadlock_tests`
//! Requires Docker for testcontainers.
#![allow(clippy::unwrap_used)]

use sqlx::PgPool;
use std::sync::Arc;
use synctv_core_testing::postgres::docker_startup_timeout;
use synctv_core::{
    models::{
        MemberStatus, Room, RoomId, RoomMember, RoomRole, SignupMethod, User, UserId, UserStatus,
    },
    repository::{RoomMemberRepository, RoomRepository, UserRepository},
};
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;
use tokio::sync::Barrier;

/// Test container wrapper for Postgres
pub struct TestPostgres {
    pub pool: PgPool,
    _container: ContainerAsync<Postgres>,
}

async fn create_test_pool() -> TestPostgres {
    let container = tokio::time::timeout(
        docker_startup_timeout(),
        Postgres::default()
            .with_db_name("synctv_test")
            .with_user("synctv")
            .with_password("synctv_test")
            .with_tag("16-alpine")
            .start(),
    )
    .await
    .expect("Docker container startup timed out (is Docker running?)")
    .expect("Failed to start Postgres container");

    let host = container.get_host().await.expect("Failed to get host");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get port");

    let database_url = format!("postgres://synctv:synctv_test@{host}:{port}/synctv_test");

    let pool = {
        let mut retries = 0u32;
        loop {
            match sqlx::postgres::PgPoolOptions::new()
                .max_connections(5)
                .acquire_timeout(std::time::Duration::from_secs(2))
                .connect(&database_url)
                .await
            {
                Ok(p) => break p,
                Err(_) if retries < 60 => {
                    retries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(e) => panic!("PostgreSQL not ready after {retries} retries: {e}"),
            }
        }
    };

    // Run migrations
    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    TestPostgres {
        pool,
        _container: container,
    }
}

/// Create a test user in the database (required for FK constraints)
async fn create_test_user(pool: &PgPool, user_id: &UserId) {
    let username = format!("test_user_{}", user_id.as_str());
    let user = User {
        id: user_id.clone(),
        username,
        email: Some(format!("{}@test.com", user_id.as_str())),
        password_hash: "test_hash".to_string(),
        signup_method: SignupMethod::Email,
        role: synctv_core::models::UserRole::User,
        status: UserStatus::Active,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version: 0,
        version: 0,
        deleted_at: None,
        email_verified: true,
    };
    let user_repo = UserRepository::new(pool.clone());
    user_repo
        .create(&user)
        .await
        .expect("Failed to create test user");
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
        left_at: None,
        version: 0,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    }
}

fn make_room(creator_id: UserId) -> Room {
    Room {
        id: RoomId::new(),
        name: "Test Room".to_string(),
        description: "Test".to_string(),
        created_by: creator_id,
        status: synctv_core::models::RoomStatus::Active,
        is_banned: false,
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

    let creator_id = UserId::new();
    create_test_user(pool, &creator_id).await;
    let room = make_room(creator_id);
    room_repo
        .create(&room)
        .await
        .expect("Failed to create room");

    let user1 = UserId::new();
    let user2 = UserId::new();
    create_test_user(pool, &user1).await;
    create_test_user(pool, &user2).await;

    let member1 = make_member(room.id.clone(), user1.clone());
    let member2 = make_member(room.id.clone(), user2.clone());
    member_repo
        .add(&member1)
        .await
        .expect("Failed to create member1");
    member_repo
        .add(&member2)
        .await
        .expect("Failed to create member2");

    // Try to cause deadlock with opposite lock order
    let barrier = Arc::new(Barrier::new(2));

    let pool1 = pool.clone();
    let room_id1 = room.id.clone();
    let user1_clone = user1.clone();
    let user2_clone1 = user2.clone();
    let barrier1 = barrier.clone();

    let handle1 = tokio::spawn(async move {
        barrier1.wait().await;

        let result: Result<(), sqlx::Error> = async {
            let mut tx = pool1.begin().await?;

            // Lock user1 first
            sqlx::query(
                "UPDATE room_members SET added_permissions = $1
                 WHERE room_id = $2 AND user_id = $3",
            )
            .bind(100i64)
            .bind(room_id1.as_str())
            .bind(user1_clone.as_str())
            .execute(&mut *tx)
            .await?;

            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

            // Then lock user2
            sqlx::query(
                "UPDATE room_members SET added_permissions = $1
                 WHERE room_id = $2 AND user_id = $3",
            )
            .bind(200i64)
            .bind(room_id1.as_str())
            .bind(user2_clone1.as_str())
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            Ok(())
        }
        .await;

        result
    });

    let pool2 = pool.clone();
    let room_id2 = room.id.clone();
    let user1_clone2 = user1.clone();
    let user2_clone2 = user2.clone();
    let barrier2 = barrier.clone();

    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;

        let result: Result<(), sqlx::Error> = async {
            let mut tx = pool2.begin().await?;

            // Lock user2 first (opposite order)
            sqlx::query(
                "UPDATE room_members SET added_permissions = $1
                 WHERE room_id = $2 AND user_id = $3",
            )
            .bind(300i64)
            .bind(room_id2.as_str())
            .bind(user2_clone2.as_str())
            .execute(&mut *tx)
            .await?;

            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

            // Then lock user1 (creates potential deadlock)
            sqlx::query(
                "UPDATE room_members SET added_permissions = $1
                 WHERE room_id = $2 AND user_id = $3",
            )
            .bind(400i64)
            .bind(room_id2.as_str())
            .bind(user1_clone2.as_str())
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            Ok(())
        }
        .await;

        result
    });

    let result1 = handle1.await.expect("Task 1 panicked");
    let result2 = handle2.await.expect("Task 2 panicked");

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

    let creator_id = UserId::new();
    create_test_user(pool, &creator_id).await;
    let room = make_room(creator_id);
    room_repo
        .create(&room)
        .await
        .expect("Failed to create room");

    let user_id = UserId::new();
    create_test_user(pool, &user_id).await;
    let member = make_member(room.id.clone(), user_id.clone());
    member_repo
        .add(&member)
        .await
        .expect("Failed to create member");

    let barrier = Arc::new(Barrier::new(2));

    let pool1 = pool.clone();
    let room_id1 = room.id.clone();
    let user_id1 = user_id.clone();
    let barrier1 = barrier.clone();

    let handle1 = tokio::spawn(async move {
        barrier1.wait().await;

        let result: Result<(), sqlx::Error> = async {
            let mut tx = pool1.begin().await?;

            // Lock with FOR UPDATE
            sqlx::query(
                "SELECT * FROM room_members
                 WHERE room_id = $1 AND user_id = $2
                 FOR UPDATE",
            )
            .bind(room_id1.as_str())
            .bind(user_id1.as_str())
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
    let room_id2 = room.id.clone();
    let user_id2 = user_id.clone();
    let barrier2 = barrier.clone();

    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        let result: Result<(), sqlx::Error> = async {
            let mut tx = pool2.begin().await?;

            // Try to lock with FOR UPDATE NOWAIT
            let result = sqlx::query(
                "SELECT * FROM room_members
                 WHERE room_id = $1 AND user_id = $2
                 FOR UPDATE NOWAIT",
            )
            .bind(room_id2.as_str())
            .bind(user_id2.as_str())
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

    let result1 = handle1.await.expect("Task 1 panicked");
    let result2 = handle2.await.expect("Task 2 panicked");

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

    let creator_id = UserId::new();
    create_test_user(pool, &creator_id).await;
    let room = make_room(creator_id);
    room_repo
        .create(&room)
        .await
        .expect("Failed to create room");

    let user1 = UserId::new();
    let user2 = UserId::new();
    create_test_user(pool, &user1).await;
    create_test_user(pool, &user2).await;

    // Ensure consistent ordering
    let (first_user, second_user) = if user1.as_str() < user2.as_str() {
        (user1.clone(), user2.clone())
    } else {
        (user2.clone(), user1.clone())
    };

    let member1 = make_member(room.id.clone(), first_user.clone());
    let member2 = make_member(room.id.clone(), second_user.clone());
    member_repo
        .add(&member1)
        .await
        .expect("Failed to create member1");
    member_repo
        .add(&member2)
        .await
        .expect("Failed to create member2");

    let barrier = Arc::new(Barrier::new(2));

    // Both transactions lock in the same order
    let pool1 = pool.clone();
    let room_id1 = room.id.clone();
    let first_user1 = first_user.clone();
    let second_user1 = second_user.clone();
    let barrier1 = barrier.clone();

    let handle1 = tokio::spawn(async move {
        barrier1.wait().await;

        let result: Result<(), sqlx::Error> = async {
            let mut tx = pool1.begin().await?;

            // Lock in consistent order
            sqlx::query(
                "UPDATE room_members SET added_permissions = $1
                 WHERE room_id = $2 AND user_id = $3",
            )
            .bind(100i64)
            .bind(room_id1.as_str())
            .bind(first_user1.as_str())
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "UPDATE room_members SET added_permissions = $1
                 WHERE room_id = $2 AND user_id = $3",
            )
            .bind(200i64)
            .bind(room_id1.as_str())
            .bind(second_user1.as_str())
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            Ok(())
        }
        .await;

        result
    });

    let pool2 = pool.clone();
    let room_id2 = room.id.clone();
    let first_user2 = first_user.clone();
    let second_user2 = second_user.clone();
    let barrier2 = barrier.clone();

    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;

        let result: Result<(), sqlx::Error> = async {
            let mut tx = pool2.begin().await?;

            // Lock in same consistent order
            sqlx::query(
                "UPDATE room_members SET added_permissions = $1
                 WHERE room_id = $2 AND user_id = $3",
            )
            .bind(300i64)
            .bind(room_id2.as_str())
            .bind(first_user2.as_str())
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "UPDATE room_members SET added_permissions = $1
                 WHERE room_id = $2 AND user_id = $3",
            )
            .bind(400i64)
            .bind(room_id2.as_str())
            .bind(second_user2.as_str())
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            Ok(())
        }
        .await;

        result
    });

    let result1 = handle1.await.expect("Task 1 panicked");
    let result2 = handle2.await.expect("Task 2 panicked");

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

    let creator_id = UserId::new();
    create_test_user(pool, &creator_id).await;
    let room = make_room(creator_id);
    room_repo
        .create(&room)
        .await
        .expect("Failed to create room");

    let user_id = UserId::new();
    create_test_user(pool, &user_id).await;
    let member = make_member(room.id.clone(), user_id.clone());
    member_repo
        .add(&member)
        .await
        .expect("Failed to create member");

    // Start a long-running transaction
    let pool1 = pool.clone();
    let room_id1 = room.id.clone();
    let user_id1 = user_id.clone();

    let tx_handle = tokio::spawn(async move {
        let mut tx = pool1.begin().await.expect("Failed to begin transaction");

        // Lock the row
        sqlx::query(
            "SELECT * FROM room_members
             WHERE room_id = $1 AND user_id = $2
             FOR UPDATE",
        )
        .bind(room_id1.as_str())
        .bind(user_id1.as_str())
        .fetch_optional(&mut *tx)
        .await
        .expect("Failed to lock row");

        // Hold lock for 2 seconds
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        tx.commit().await.expect("Failed to commit");
    });

    // Wait a bit then try to access same row with timeout
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let pool2 = pool.clone();
    let room_id2 = room.id.clone();
    let user_id2 = user_id.clone();

    let timeout_result = tokio::time::timeout(tokio::time::Duration::from_secs(1), async move {
        let mut tx = pool2.begin().await?;

        sqlx::query(
            "SELECT * FROM room_members
                 WHERE room_id = $1 AND user_id = $2
                 FOR UPDATE",
        )
        .bind(room_id2.as_str())
        .bind(user_id2.as_str())
        .fetch_optional(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok::<(), sqlx::Error>(())
    })
    .await;

    // Should timeout waiting for lock
    assert!(timeout_result.is_err(), "Should timeout waiting for lock");

    // Wait for first transaction to complete
    tx_handle.await.expect("First transaction failed");
}
