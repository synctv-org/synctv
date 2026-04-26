//! Partition management integration tests
//!
//! Verifies that `chat_messages` DEFAULT partition routes messages correctly,
//! and that `create_chat_message_partitions()` creates future partitions.
//! Also verifies `audit_logs` DEFAULT partition.
//!
//! Run with: cargo test --test `partition_management_tests`
#![allow(clippy::unwrap_used)]

use chrono::{Duration, Utc};
use synctv_core::{
    models::{ChatMessage, Room, RoomId, RoomStatus, User, UserId, UserRole, UserStatus},
    repository::{ChatRepository, RoomRepository, UserRepository},
};
use synctv_core_testing::create_test_pool;
/// Default `PostgreSQL` version for test containers
fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_message_default_partition_routing() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let chat_repo = ChatRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("partition_owner_1"))
        .await
        .unwrap();
    let room = room_repo
        .create(&{
            let now = Utc::now();
            Room {
                id: RoomId::new(),
                name: "Partition Test Room".to_string(),
                description: String::new(),
                created_by: owner.id.clone(),
                status: RoomStatus::Active,
                is_banned: false,
                closed_at: None,
                created_at: now,
                updated_at: now,
                deleted_at: None,
                version: 0,
                last_activity_at: now,
            }
        })
        .await
        .unwrap();

    // Insert a message with a far-future date (no specific partition exists)
    // This should route to the DEFAULT partition instead of failing
    let far_future = Utc::now() + Duration::days(365);
    let msg = ChatMessage {
        id: synctv_common::snanoid!(12),
        room_id: room.id.clone(),
        user_id: Some(owner.id.clone()),
        content: "Future message".to_string(),
        message_type: 1,
        created_at: far_future,
    };

    let created = chat_repo.create(&msg).await;
    assert!(
        created.is_ok(),
        "Message with far-future date should insert into DEFAULT partition, got: {:?}",
        created.err()
    );

    // Verify we can retrieve it
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_messages WHERE id = $1")
        .bind(&msg.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_chat_message_partitions_function() {
    let (_container, pool) = create_test_pool().await;

    // Call the partition creation function for 5 days ahead
    let result: String = sqlx::query_scalar("SELECT create_chat_message_partitions(5)::TEXT")
        .fetch_one(&pool)
        .await
        .unwrap();

    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap_or_default();
    assert_eq!(parsed["status"], "completed");

    // Verify partitions exist
    let partition_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_tables
         WHERE schemaname = 'public'
           AND tablename LIKE 'chat_messages_%'
           AND tablename ~ '^chat_messages_[0-9]{4}_[0-9]{2}_[0-9]{2}$'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    // Should have at least some partitions (migrations create 31, plus our 6)
    assert!(partition_count > 0, "Should have chat message partitions");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_logs_default_partition_routing() {
    let (_container, pool) = create_test_pool().await;

    // Insert an audit log entry with a far-future date
    let far_future = Utc::now() + Duration::days(365 * 2);
    let result =
        sqlx::query("INSERT INTO audit_logs (actor_id, action, created_at) VALUES ($1, $2, $3)")
            .bind("test_actor_1")
            .bind("test_action")
            .bind(far_future)
            .execute(&pool)
            .await;

    assert!(
        result.is_ok(),
        "Audit log with far-future date should insert into DEFAULT partition, got: {:?}",
        result.err()
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_check_chat_message_partitions_health() {
    let (_container, pool) = create_test_pool().await;

    // Call the health check function
    let result: String = sqlx::query_scalar("SELECT check_chat_message_partitions(7)::TEXT")
        .fetch_one(&pool)
        .await
        .unwrap();

    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(parsed["status"], "checked");
    // After migrations, all partitions for the next 30 days should be present
    // so missing_count for 7 days ahead should be 0
    assert_eq!(parsed["health_status"], "healthy");
}
