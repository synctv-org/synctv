//! Partition management integration tests
//!
//! Verifies partition managers create writable future partitions and
//! out-of-range writes fail fast when partition maintenance misses a range.

use chrono::{Duration, Utc};
use std::sync::Arc;
use synctv_core::{
    models::{
        AuditAction, ChatMessage, ChatMessageType, Room, RoomId, RoomStatus, User, UserId,
        UserRole, UserStatus,
    },
    repository::{ChatRepository, RoomRepository, UserRepository},
    service::{AlwaysLeader, AuditPartitionManager, ChatPartitionManager},
};
use synctv_core_testing::{create_test_pool, ok};
/// Default `PostgreSQL` version for test containers
fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
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
async fn test_chat_message_future_partition_created_by_manager_is_writable() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let chat_repo = ChatRepository::new(pool.clone());
    let manager = ChatPartitionManager::new(pool.clone(), Arc::new(AlwaysLeader));

    let owner = ok(
        user_repo.create(&make_user("partition_owner_1")).await,
        "partition owner should be created",
    );
    let room = ok(
        room_repo
            .create(&{
                let now = Utc::now();
                Room {
                    id: RoomId::new(),
                    name: "Partition Test Room".to_string(),
                    description: String::new(),
                    cover_file_reference_id: None,
                    category: None,
                    labels: Vec::new(),
                    created_by: owner.id,
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
            .await,
        "partition test room should be created",
    );

    let future = Utc::now() + Duration::days(365);
    ok(
        manager.ensure_future_partitions(366).await,
        "chat manager should create the future message partition",
    );
    let msg = ChatMessage {
        id: synctv_core::models::generate_id(),
        room_id: room.id,
        user_id: Some(owner.id),
        client_message_id: None,
        content: "Future message".to_string(),
        message_type: ChatMessageType::Text,
        status: synctv_core::models::ChatMessageStatus::Active,
        version: 1,
        reply_to_message_id: None,
        reply_to_message_created_at: None,
        metadata: synctv_core::models::ChatMetadata::default(),
        edited_at: None,
        deleted_at: None,
        deleted_by: None,
        delete_reason: None,
        created_at: future,
    };

    let created = chat_repo.create(&msg).await;
    assert!(
        created.is_ok(),
        "message with an explicitly managed future partition should insert, got: {:?}",
        created.err()
    );
    let created = ok(created, "message should be returned");

    // Verify we can retrieve it
    let count: i64 = ok(
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!" FROM chat_messages WHERE id = $1"#,
            created.id
        )
        .fetch_one(&pool)
        .await,
        "chat message count query should succeed",
    );
    assert_eq!(count, 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_message_without_matching_partition_fails_fast() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let chat_repo = ChatRepository::new(pool.clone());

    let owner = ok(
        user_repo
            .create(&make_user("partition_missing_owner"))
            .await,
        "partition missing owner should be created",
    );
    let room = ok(
        room_repo
            .create(&{
                let now = Utc::now();
                Room {
                    id: RoomId::new(),
                    name: "Missing Partition Room".to_string(),
                    description: String::new(),
                    cover_file_reference_id: None,
                    category: None,
                    labels: Vec::new(),
                    created_by: owner.id,
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
            .await,
        "partition missing test room should be created",
    );

    let unmanaged_future = Utc::now() + Duration::days(3650);
    let msg = ChatMessage {
        id: synctv_core::models::generate_id(),
        room_id: room.id,
        user_id: Some(owner.id),
        client_message_id: None,
        content: "Unmanaged future message".to_string(),
        message_type: ChatMessageType::Text,
        status: synctv_core::models::ChatMessageStatus::Active,
        version: 1,
        reply_to_message_id: None,
        reply_to_message_created_at: None,
        metadata: synctv_core::models::ChatMetadata::default(),
        edited_at: None,
        deleted_at: None,
        deleted_by: None,
        delete_reason: None,
        created_at: unmanaged_future,
    };

    let err = chat_repo
        .create(&msg)
        .await
        .expect_err("unmanaged future chat message should fail without a matching partition");
    assert!(
        err.to_string()
            .contains("Partition range is not initialized"),
        "unexpected partition error: {err}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_partition_manager_creates_future_partitions() {
    let (_container, pool) = create_test_pool().await;
    let manager = ChatPartitionManager::new(pool.clone(), Arc::new(AlwaysLeader));

    let created_count = ok(
        manager.ensure_future_partitions(5).await,
        "chat partitions should be created",
    );
    assert_eq!(created_count, 6);

    // Verify partitions exist
    let partition_count: i64 = ok(
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!" FROM pg_tables
         WHERE schemaname = 'public'
           AND tablename LIKE 'chat_messages_%'
           AND tablename ~ '^chat_messages_[0-9]{4}_[0-9]{2}_[0-9]{2}$'"#,
        )
        .fetch_one(&pool)
        .await,
        "chat partition count query should succeed",
    );

    // Should have at least some partitions (migrations create 31, plus our 6)
    assert!(partition_count > 0, "Should have chat message partitions");

    let created_at_index_attached = ok(
        sqlx::query_scalar!(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM pg_inherits index_inherits
                JOIN pg_class child_index
                  ON child_index.oid = index_inherits.inhrelid
                JOIN pg_class parent_index
                  ON parent_index.oid = index_inherits.inhparent
                JOIN pg_index child_pg_index
                  ON child_pg_index.indexrelid = child_index.oid
                JOIN pg_class child_table
                  ON child_table.oid = child_pg_index.indrelid
                WHERE parent_index.relname = 'idx_chat_messages_created_at'
                  AND child_table.relname ~ '^chat_messages_[0-9]{4}_[0-9]{2}_[0-9]{2}$'
            )
            "#,
        )
        .fetch_one(&pool)
        .await,
        "chat partition index inheritance query should succeed",
    )
    .unwrap_or(false);
    assert!(
        created_at_index_attached,
        "new chat partitions should inherit parent partitioned indexes"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_logs_future_partition_created_by_manager_is_writable() {
    let (_container, pool) = create_test_pool().await;
    let manager = AuditPartitionManager::new(pool.clone(), Arc::new(AlwaysLeader));

    let far_future = Utc::now() + Duration::days(365 * 2);
    ok(
        manager.ensure_future_partitions(24).await,
        "audit manager should create the future audit partition",
    );
    let result = sqlx::query!(
        "INSERT INTO audit_logs (actor_id, action, created_at) VALUES ($1, $2, $3)",
        UserId::expect_positive(1).as_i64(),
        i16::from(AuditAction::SettingsUpdated),
        far_future,
    )
    .execute(&pool)
    .await;

    assert!(
        result.is_ok(),
        "audit log with an explicitly managed future partition should insert, got: {:?}",
        result.err()
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_check_chat_message_partitions_health() {
    let (_container, pool) = create_test_pool().await;
    let manager = ChatPartitionManager::new(pool, Arc::new(AlwaysLeader));

    ok(
        manager.ensure_future_partitions(7).await,
        "chat partitions should be created before health check",
    );
    let health = ok(
        manager.check_health(7).await,
        "chat partition health check should succeed",
    );
    assert_eq!(health.health_status, "healthy");
}
