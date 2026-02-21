//! RoomPlaybackStateRepository integration tests
//!
//! Tests create_or_get idempotency and update optimistic locking.
//!
//! Run with: cargo test --test playback_repository_tests

use synctv_core::{
    models::{
        Room, RoomId, RoomStatus, UserId, User, UserRole, UserStatus,
    },
    repository::{RoomRepository, RoomPlaybackStateRepository, UserRepository},
    Error,
};
use chrono::Utc;
use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

const POSTGRES_VERSION: &str = "16-alpine";

async fn create_test_pool() -> (ContainerAsync<Postgres>, PgPool) {
    let postgres = Postgres::default()
        .with_db_name("synctv_test")
        .with_user("synctv")
        .with_password("synctv_test")
        .with_tag(POSTGRES_VERSION)
        .start()
        .await
        .expect("Failed to start Postgres container");

    let connection_string = format!(
        "postgresql://synctv:synctv_test@127.0.0.1:{}/synctv_test",
        postgres.get_host_port_ipv4(5432).await.expect("Failed to get port")
    );

    let pool = PgPool::connect(&connection_string)
        .await
        .expect("Failed to create pool");

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (postgres, pool)
}

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{}@test.com", username)),
        password_hash: "hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: None,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

fn make_room(name: &str, owner: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: "test".to_string(),
        created_by: owner.clone(),
        status: RoomStatus::Active,
        is_banned: false,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

// ========== create_or_get idempotency ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_or_get_idempotent() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("owner_pb_idem")).await.unwrap();
    let room = room_repo.create(&make_room("Room PB Idem", &owner.id)).await.unwrap();

    // First call creates the state
    let state1 = playback_repo.create_or_get(&room.id).await.unwrap();
    assert_eq!(state1.room_id, room.id);
    assert_eq!(state1.current_time, 0.0);
    assert_eq!(state1.speed, 1.0);
    assert!(!state1.is_playing);

    // Second call returns the same version (no new insert or update)
    let state2 = playback_repo.create_or_get(&room.id).await.unwrap();
    assert_eq!(state2.version, state1.version);
    assert_eq!(state2.room_id, state1.room_id);
}

// ========== update optimistic lock ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_optimistic_lock_conflict() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("owner_pb_lock")).await.unwrap();
    let room = room_repo.create(&make_room("Room PB Lock", &owner.id)).await.unwrap();

    let state = playback_repo.create_or_get(&room.id).await.unwrap();

    // Task 1 updates with correct version
    let mut state_t1 = state.clone();
    state_t1.current_time = 42.0;

    // Task 2 also has the same version (stale read)
    let mut state_t2 = state.clone();
    state_t2.current_time = 99.0;

    // Task 1 succeeds
    let updated = playback_repo.update(&state_t1).await.unwrap();
    assert_eq!(updated.current_time, 42.0);
    assert_eq!(updated.version, state.version + 1);

    // Task 2 uses stale version -> OptimisticLockConflict
    let err = playback_repo.update(&state_t2).await.unwrap_err();
    assert!(matches!(err, Error::OptimisticLockConflict));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_concurrent_tasks_one_gets_conflict() {
    use std::sync::Arc;
    use tokio::sync::Barrier;

    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playback_repo = Arc::new(RoomPlaybackStateRepository::new(pool.clone()));

    let owner = user_repo.create(&make_user("owner_pb_conc")).await.unwrap();
    let room = room_repo.create(&make_room("Room PB Conc", &owner.id)).await.unwrap();

    let state = playback_repo.create_or_get(&room.id).await.unwrap();

    let barrier = Arc::new(Barrier::new(2));

    // Spawn two concurrent tasks both trying to update with the same version
    let repo1 = playback_repo.clone();
    let state1 = state.clone();
    let barrier1 = barrier.clone();
    let handle1 = tokio::spawn(async move {
        barrier1.wait().await;
        let mut s = state1;
        s.current_time = 10.0;
        repo1.update(&s).await
    });

    let repo2 = playback_repo.clone();
    let state2 = state.clone();
    let barrier2 = barrier.clone();
    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;
        let mut s = state2;
        s.current_time = 20.0;
        repo2.update(&s).await
    });

    let r1 = handle1.await.unwrap();
    let r2 = handle2.await.unwrap();

    // Exactly one should succeed and one should fail
    let (successes, failures) = match (&r1, &r2) {
        (Ok(_), Ok(_)) => (2, 0),
        (Ok(_), Err(_)) | (Err(_), Ok(_)) => (1, 1),
        (Err(_), Err(_)) => (0, 2),
    };

    assert_eq!(successes, 1, "Exactly one update should succeed");
    assert_eq!(failures, 1, "Exactly one update should fail with OptimisticLockConflict");

    // Verify the failure is OptimisticLockConflict
    let err = if r1.is_err() { r1.unwrap_err() } else { r2.unwrap_err() };
    assert!(matches!(err, Error::OptimisticLockConflict));
}
