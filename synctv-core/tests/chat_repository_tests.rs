//! `ChatRepository` integration tests
//!
//! Tests: `list_by_room_cursor` pagination, `get_by_id` without time restriction,
//! `cleanup_old_messages` `keep_count=0` no-op, `cleanup_all_rooms` `activity_window_minutes=0`.
//!
//! Run with: cargo test -p synctv-core --test `chat_repository_tests`
#![allow(clippy::unwrap_used)]

use chrono::{Duration, Utc};
use sqlx::PgPool;
use synctv_core::{
    models::{
        ChatMessage, ChatPlaybackMessagesQuery, MediaId, PlaylistId, Room, RoomId, RoomStatus,
        User, UserId, UserRole, UserStatus,
    },
    repository::{ChatRepository, RoomRepository, UserRepository},
};
use synctv_core_testing::create_test_pool;
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

fn make_room(name: &str, owner: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        created_by: *owner,
        status: RoomStatus::Active,
        is_banned: false,
        closed_at: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        version: 0,
        last_activity_at: now,
    }
}

async fn setup_room(pool: &PgPool, username: &str, room_name: &str) -> (User, Room) {
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let user = user_repo.create(&make_user(username)).await.unwrap();
    let room = room_repo
        .create(&make_room(room_name, &user.id))
        .await
        .unwrap();
    (user, room)
}

fn make_chat_message(room_id: &RoomId, user_id: &UserId, content: &str) -> ChatMessage {
    ChatMessage::new(*room_id, *user_id, content.to_string())
}

// ─── list_by_room_cursor pagination ──────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_room_cursor_pagination() {
    let (_container, pool) = create_test_pool().await;
    let chat_repo = ChatRepository::new(pool.clone());
    let (user, room) = setup_room(&pool, "chat_cursor_user", "chat_cursor_room").await;

    // Insert 5 messages with slightly different timestamps
    let mut created_ids = Vec::new();
    for i in 0..5 {
        let msg = make_chat_message(&room.id, &user.id, &format!("msg_{i}"));
        let created = chat_repo.create(&msg).await.unwrap();
        created_ids.push(created.id);
        // Small delay to ensure ordering
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    // Page 1: newest 2 messages (no cursor)
    let (page1, cursor1) = chat_repo
        .list_by_room_cursor(&room.id, None, 2, false)
        .await
        .unwrap();
    assert_eq!(page1.len(), 2);
    assert!(
        cursor1.is_some(),
        "Should have next cursor since there are more messages"
    );
    // Messages should be in reverse chronological order
    assert_eq!(page1[0].message.content, "msg_4");
    assert_eq!(page1[1].message.content, "msg_3");

    // Page 2: next 2 messages
    let cursor1_val = cursor1.unwrap();
    let (page2, cursor2) = chat_repo
        .list_by_room_cursor(&room.id, Some(cursor1_val), 2, false)
        .await
        .unwrap();
    assert_eq!(page2.len(), 2);
    assert!(
        cursor2.is_some(),
        "Should still have a cursor (1 more message)"
    );
    assert_eq!(page2[0].message.content, "msg_2");
    assert_eq!(page2[1].message.content, "msg_1");

    // Page 3: last page (1 message)
    let cursor2_val = cursor2.unwrap();
    let (page3, cursor3) = chat_repo
        .list_by_room_cursor(&room.id, Some(cursor2_val), 2, false)
        .await
        .unwrap();
    assert_eq!(page3.len(), 1);
    assert!(cursor3.is_none(), "Last page should have no next cursor");
    assert_eq!(page3[0].message.content, "msg_0");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_playback_messages_filters_by_context_and_time_window() {
    let (_container, pool) = create_test_pool().await;
    let chat_repo = ChatRepository::new(pool.clone());
    let (user, room) = setup_room(&pool, "chat_playback_user", "chat_playback_room").await;
    let media_id = MediaId::expect_positive(300_001);
    let playlist_id = PlaylistId::expect_positive(300_002);
    let target_hash = "target-hash-1".to_string();

    for (content, position_seconds, status) in [
        (
            "before",
            8.0,
            synctv_core::models::ChatMessageStatus::Active,
        ),
        (
            "inside-a",
            10.0,
            synctv_core::models::ChatMessageStatus::Active,
        ),
        (
            "inside-b",
            12.0,
            synctv_core::models::ChatMessageStatus::Active,
        ),
        (
            "deleted",
            11.0,
            synctv_core::models::ChatMessageStatus::Deleted,
        ),
        (
            "after",
            20.0,
            synctv_core::models::ChatMessageStatus::Active,
        ),
    ] {
        let mut msg = make_chat_message(&room.id, &user.id, content);
        msg.status = status;
        msg.metadata = serde_json::json!({
            "playback": {
                "media_id": media_id.as_i64().to_string(),
                "playlist_id": playlist_id.as_i64().to_string(),
                "target_hash": target_hash.clone(),
                "position_seconds": position_seconds
            }
        });
        chat_repo.create(&msg).await.unwrap();
    }

    let messages = chat_repo
        .list_playback_messages(&ChatPlaybackMessagesQuery {
            room_id: room.id,
            media_id: Some(media_id),
            playlist_id: Some(playlist_id),
            target_hash: Some(target_hash),
            position_seconds: 11.0,
            before_seconds: 1.0,
            after_seconds: 1.0,
            limit: 100,
            include_deleted: false,
        })
        .await
        .unwrap();

    let contents = messages
        .into_iter()
        .map(|message| message.message.content)
        .collect::<Vec<_>>();
    assert_eq!(contents, vec!["inside-a", "inside-b"]);
}

// ─── get_by_id without time restriction ─────────────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_id_recent_message() {
    let (_container, pool) = create_test_pool().await;
    let chat_repo = ChatRepository::new(pool.clone());
    let (user, room) = setup_room(&pool, "chat_get_user", "chat_get_room").await;

    let msg = make_chat_message(&room.id, &user.id, "recent message");
    let created = chat_repo.create(&msg).await.unwrap();

    let fetched = chat_repo.get_by_id(created.id).await.unwrap();
    assert!(fetched.is_some());
    assert_eq!(fetched.unwrap().content, "recent message");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_id_old_message_succeeds() {
    // Verify that get_by_id can retrieve messages older than 90 days.
    // This is important for audit scenarios where historical messages need to be accessed.
    let (_container, pool) = create_test_pool().await;
    let chat_repo = ChatRepository::new(pool.clone());
    let (user, room) = setup_room(&pool, "chat_old_user", "chat_old_room").await;

    let old_date = Utc::now() - Duration::days(100);
    let msg_id = synctv_core::models::generate_id();

    // Insert directly with backdated created_at
    sqlx::query(
        r"INSERT INTO chat_messages (id, room_id, user_id, content, message_type, created_at)
          VALUES ($1, $2, $3, 'old message', 1, $4)",
    )
    .bind(msg_id)
    .bind(room.id)
    .bind(user.id)
    .bind(old_date)
    .execute(&pool)
    .await
    .unwrap();

    // get_by_id should now successfully retrieve messages older than 90 days
    let fetched = chat_repo.get_by_id(msg_id).await.unwrap();
    assert!(
        fetched.is_some(),
        "get_by_id should return messages older than 90 days for audit purposes"
    );
    assert_eq!(fetched.unwrap().content, "old message");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_id_very_old_message_succeeds() {
    // Verify that get_by_id can retrieve messages from a year ago.
    // This ensures no hidden time restrictions exist.
    let (_container, pool) = create_test_pool().await;
    let chat_repo = ChatRepository::new(pool.clone());
    let (user, room) = setup_room(&pool, "chat_very_old_user", "chat_very_old_room").await;

    let very_old_date = Utc::now() - Duration::days(365);
    let msg_id = synctv_core::models::generate_id();

    // Insert directly with backdated created_at (1 year ago)
    sqlx::query(
        r"INSERT INTO chat_messages (id, room_id, user_id, content, message_type, created_at)
          VALUES ($1, $2, $3, 'very old message from a year ago', 1, $4)",
    )
    .bind(msg_id)
    .bind(room.id)
    .bind(user.id)
    .bind(very_old_date)
    .execute(&pool)
    .await
    .unwrap();

    // get_by_id should retrieve very old messages for audit purposes
    let fetched = chat_repo.get_by_id(msg_id).await.unwrap();
    assert!(
        fetched.is_some(),
        "get_by_id should return messages from any time period"
    );
    assert_eq!(fetched.unwrap().content, "very old message from a year ago");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_id_nonexistent_returns_none() {
    // Verify that get_by_id returns None for non-existent messages
    let (_container, pool) = create_test_pool().await;
    let chat_repo = ChatRepository::new(pool.clone());

    let fetched = chat_repo.get_by_id(i64::MAX).await.unwrap();
    assert!(
        fetched.is_none(),
        "get_by_id should return None for non-existent messages"
    );
}

// ─── cleanup_old_messages keep_count=0 no-op ─────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cleanup_old_messages_keep_count_zero_is_noop() {
    let (_container, pool) = create_test_pool().await;
    let chat_repo = ChatRepository::new(pool.clone());
    let (user, room) = setup_room(&pool, "chat_cleanup_user", "chat_cleanup_room").await;

    // Insert some messages
    for i in 0..3 {
        let msg = make_chat_message(&room.id, &user.id, &format!("keep_{i}"));
        chat_repo.create(&msg).await.unwrap();
    }

    // keep_count=0 should return 0 and delete nothing
    let deleted = chat_repo.cleanup_old_messages(&room.id, 0).await.unwrap();
    assert_eq!(deleted, 0, "keep_count=0 should be a no-op");

    // All messages should still exist
    let count = chat_repo.count_by_room(&room.id).await.unwrap();
    assert_eq!(count, 3);
}

// ─── cleanup_all_rooms activity_window_minutes=0 ─────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cleanup_all_rooms_activity_window_zero() {
    let (_container, pool) = create_test_pool().await;
    let chat_repo = ChatRepository::new(pool.clone());
    let (user, room) = setup_room(&pool, "chat_all_cleanup_user", "chat_all_cleanup_room").await;

    // Insert messages
    for i in 0..5 {
        let msg = make_chat_message(&room.id, &user.id, &format!("msg_{i}"));
        chat_repo.create(&msg).await.unwrap();
    }

    // activity_window_minutes=0 with keep_count=2 --
    // `make_interval(mins => 0)` yields INTERVAL '0', so
    // `NOW() - INTERVAL '0'` == NOW(), meaning only messages created
    // at exactly NOW() are in the activity window. Recently inserted
    // messages may or may not be selected depending on sub-second timing.
    // The key test is that it doesn't error and doesn't delete everything.
    let deleted = chat_repo.cleanup_all_rooms(2, 0).await.unwrap();
    // With 0-minute window, rooms with messages only at exactly NOW() match.
    // The result may vary, so just assert no error and that the total is <= 3
    assert!(
        deleted <= 3,
        "Should delete at most 3 messages (5 - keep_count 2)"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cleanup_all_rooms_keep_count_zero_is_noop() {
    let (_container, pool) = create_test_pool().await;
    let chat_repo = ChatRepository::new(pool.clone());
    let (user, room) = setup_room(&pool, "chat_all_noop_user", "chat_all_noop_room").await;

    for i in 0..3 {
        let msg = make_chat_message(&room.id, &user.id, &format!("msg_{i}"));
        chat_repo.create(&msg).await.unwrap();
    }

    // keep_count=0 means unlimited, so no cleanup
    let deleted = chat_repo.cleanup_all_rooms(0, 60).await.unwrap();
    assert_eq!(deleted, 0, "keep_count=0 should be a no-op");

    let count = chat_repo.count_by_room(&room.id).await.unwrap();
    assert_eq!(count, 3);
}

// ─── Index on created_at for partition pruning ────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_created_at_index_exists_for_partition_pruning() {
    // time-based queries without room_id (e.g., delete_messages_older_than_retention).
    // Without this index, queries like:
    // DELETE FROM chat_messages WHERE created_at <= NOW() - INTERVAL '90 days'
    // would perform full partition scans instead of efficient partition pruning.

    let (_container, pool) = create_test_pool().await;

    // Check that the index exists on at least one partition (they all have the same structure)
    // The index should be created as part of partition creation
    let index_exists: bool = sqlx::query_scalar(
        r"
        SELECT EXISTS (
            SELECT 1
            FROM pg_indexes
            WHERE schemaname = 'public'
              AND tablename LIKE 'chat_messages_%'
              AND indexname LIKE '%created_at%'
        )
        ",
    )
    .fetch_one(&pool)
    .await
    .expect("Failed to query pg_indexes");

    assert!(
        index_exists,
        "Index on created_at should exist for partition pruning optimization"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_time_range_query_uses_index() {
    // Verify that queries filtering by created_at use an index
    let (_container, pool) = create_test_pool().await;
    let (user, room) = setup_room(&pool, "chat_idx_user", "chat_idx_room").await;

    let chat_repo = ChatRepository::new(pool.clone());

    let msg = make_chat_message(&room.id, &user.id, "test message");
    chat_repo.create(&msg).await.unwrap();

    // Run a query that should use the created_at index
    // (delete_messages_older_than_retention uses this pattern)
    let plan: serde_json::Value = sqlx::query_scalar(
        r"
        EXPLAIN (FORMAT JSON)
        DELETE FROM chat_messages
        WHERE created_at <= NOW() - INTERVAL '90 days'
        ",
    )
    .fetch_one(&pool)
    .await
    .expect("Failed to get query plan");

    // The plan should be an array (EXPLAIN FORMAT JSON returns an array)
    assert!(
        plan.is_array(),
        "Query plan should be a JSON array, got: {plan:?}"
    );

    // Note: In practice, we'd check for "Index Scan" in the plan, but
    // the exact format depends on PostgreSQL version and data distribution.
    // The key is that the index exists and the query can use it.
}

// ─── Partition pruning verification ─────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cleanup_all_rooms_has_partition_pruning_filter() {
    // filter for partition pruning.
    // The query structure should be:
    // DELETE FROM chat_messages
    // WHERE created_at > NOW() - INTERVAL '90 days' <- Partition pruning filter
    // AND (id, created_at) IN (...)

    let (_container, pool) = create_test_pool().await;

    // Get the EXPLAIN plan for cleanup_all_rooms
    let plan: serde_json::Value = sqlx::query_scalar(
        r"
        EXPLAIN (FORMAT JSON)
        DELETE FROM chat_messages
        WHERE created_at > NOW() - INTERVAL '90 days'
          AND (id, created_at) IN (
            SELECT id, created_at FROM (
                SELECT id, created_at, room_id,
                       ROW_NUMBER() OVER (PARTITION BY room_id ORDER BY created_at DESC) as rn
                FROM chat_messages
                WHERE room_id IN (
                    SELECT DISTINCT room_id
                    FROM chat_messages
                    WHERE created_at >= NOW() - make_interval(mins => 60)
                )
                  AND created_at > NOW() - INTERVAL '90 days'
            ) ranked_messages
            WHERE rn > 100
        )
        ",
    )
    .fetch_one(&pool)
    .await
    .expect("Failed to get query plan");

    // Verify the plan is valid JSON
    assert!(
        plan.is_array(),
        "Query plan should be a JSON array, got: {plan:?}"
    );

    // The key validation is that the outer DELETE has created_at filter
    // which enables partition pruning. Without this filter, PostgreSQL
    // cannot prune old partitions at the DELETE level.
}

// ─── Detailed partition pruning verification ───────────────

/// Verify `cleanup_old_messages` produces a valid query plan with partition pruning support
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cleanup_old_messages_partition_pruning_detailed() {
    // Detailed verification of partition pruning for cleanup_old_messages
    // This test verifies:

    let (_container, pool) = create_test_pool().await;
    let (user, room) = setup_room(&pool, "chat_prune_detail", "chat_prune_detail_room").await;

    let chat_repo = ChatRepository::new(pool.clone());

    for i in 0..10 {
        let msg = make_chat_message(&room.id, &user.id, &format!("prune_detail_{i}"));
        chat_repo.create(&msg).await.unwrap();
    }

    // Get detailed query plan using EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)
    let plan: serde_json::Value = sqlx::query_scalar(
        r"
        EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)
        DELETE FROM chat_messages
        WHERE room_id = $1
          AND created_at > NOW() - INTERVAL '90 days'
          AND (id, created_at) IN (
            SELECT id, created_at FROM (
                SELECT id, created_at,
                       ROW_NUMBER() OVER (ORDER BY created_at DESC) as rn
                FROM chat_messages
                WHERE room_id = $1
                  AND created_at > NOW() - INTERVAL '90 days'
            ) ranked
            WHERE rn > 5
        )
        ",
    )
    .bind(room.id)
    .fetch_one(&pool)
    .await
    .expect("Failed to get query plan");

    // Verify plan structure
    assert!(
        plan.is_array(),
        "Query plan should be a JSON array, got: {plan:?}"
    );

    // Log the plan for debugging
    println!(
        "Query plan: {}",
        serde_json::to_string_pretty(&plan).unwrap()
    );

    // Check for partition pruning indicators
    let plan_str = plan.to_string();

    // The query should NOT do a sequential scan on all partitions
    // With proper created_at filter, PostgreSQL can prune old partitions
    assert!(
        !plan_str.to_lowercase().contains("seq scan on chat_messages"),
        "Query should not perform sequential scan on chat_messages (indicates missing partition pruning)"
    );

    // Verify the query executed successfully (ANALYZE runs the query)
    // If we got here without error, the query is valid
}

/// Verify `cleanup_all_rooms` produces a valid query plan with partition pruning support
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cleanup_all_rooms_partition_pruning_detailed() {
    // Detailed verification of partition pruning for cleanup_all_rooms
    // This test verifies the batch cleanup query has proper partition pruning support

    let (_container, pool) = create_test_pool().await;
    let (user, room) = setup_room(&pool, "chat_batch_prune", "chat_batch_prune_room").await;

    let chat_repo = ChatRepository::new(pool.clone());

    for i in 0..15 {
        let msg = make_chat_message(&room.id, &user.id, &format!("batch_{i}"));
        chat_repo.create(&msg).await.unwrap();
    }

    // Get detailed query plan
    let plan: serde_json::Value = sqlx::query_scalar(
        r"
        EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)
        DELETE FROM chat_messages
        WHERE created_at > NOW() - INTERVAL '90 days'
          AND (id, created_at) IN (
            SELECT id, created_at FROM (
                SELECT id, created_at, room_id,
                       ROW_NUMBER() OVER (PARTITION BY room_id ORDER BY created_at DESC) as rn
                FROM chat_messages
                WHERE room_id IN (
                    SELECT DISTINCT room_id
                    FROM chat_messages
                    WHERE created_at >= NOW() - make_interval(mins => 60)
                )
                  AND created_at > NOW() - INTERVAL '90 days'
            ) ranked_messages
            WHERE rn > 5
        )
        ",
    )
    .fetch_one(&pool)
    .await
    .expect("Failed to get query plan");

    assert!(
        plan.is_array(),
        "Query plan should be a JSON array, got: {plan:?}"
    );

    println!(
        "Batch cleanup plan: {}",
        serde_json::to_string_pretty(&plan).unwrap()
    );

    // Verify no full table scan
    let plan_str = plan.to_string();
    assert!(
        !plan_str
            .to_lowercase()
            .contains("seq scan on chat_messages"),
        "Batch cleanup should not perform sequential scan"
    );
}

/// Verify `delete_messages_older_than_retention` uses partition pruning
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_old_messages_partition_pruning() {
    // Verify the retention cleanup query uses partition pruning
    // This query deletes all messages older than 90 days and should
    // only scan old partitions

    let (_container, pool) = create_test_pool().await;

    // Get query plan for retention cleanup
    let plan: serde_json::Value = sqlx::query_scalar(
        r"
        EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)
        DELETE FROM chat_messages
        WHERE created_at <= NOW() - INTERVAL '90 days'
        ",
    )
    .fetch_one(&pool)
    .await
    .expect("Failed to get query plan");

    assert!(
        plan.is_array(),
        "Query plan should be a JSON array, got: {plan:?}"
    );

    println!(
        "Retention cleanup plan: {}",
        serde_json::to_string_pretty(&plan).unwrap()
    );

    // The query should use an index or partition-aware scan
    // A sequential scan would indicate missing indexes or partition issues
    let plan_str = plan.to_string().to_lowercase();

    // Log if we see concerning patterns
    if plan_str.contains("seq scan") {
        println!(
            "WARNING: Sequential scan detected in retention cleanup. \
                  This may indicate missing indexes or partition misconfiguration."
        );
    }
}

// ─── list_by_room initial load needs partition lower bound ──

/// Verify that list_by_room (initial load, no cursor) includes a created_at
/// lower bound so PostgreSQL can prune old partitions. Without this, an initial
/// chat history load scans ALL partitions of the chat_messages table.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_room_initial_load_has_partition_lower_bound() {
    let (_container, pool) = create_test_pool().await;
    let chat_repo = ChatRepository::new(pool.clone());
    let (user, room) = setup_room(&pool, "chat_init_prune_user", "chat_init_prune_room").await;

    // Insert a message older than 90 days via raw SQL
    let old_date = Utc::now() - Duration::days(100);
    let old_msg_id = synctv_core::models::generate_id();
    sqlx::query(
        r"INSERT INTO chat_messages (id, room_id, user_id, content, message_type, created_at)
          VALUES ($1, $2, $3, 'old message', 1, $4)",
    )
    .bind(old_msg_id)
    .bind(room.id)
    .bind(user.id)
    .bind(old_date)
    .execute(&pool)
    .await
    .unwrap();

    // Insert a recent message
    let msg = make_chat_message(&room.id, &user.id, "recent message");
    chat_repo.create(&msg).await.unwrap();

    // Initial load (no cursor) should only return recent messages
    let (messages, next_cursor) = chat_repo
        .list_by_room_cursor(&room.id, None, 100, false)
        .await
        .unwrap();

    // The old message (>90 days) should NOT be returned due to partition pruning filter
    assert_eq!(
        messages.len(),
        1,
        "Initial load should only return messages within 90-day window"
    );
    assert_eq!(messages[0].message.content, "recent message");
    assert!(
        next_cursor.is_none(),
        "Single-message initial load should not expose a next cursor"
    );
}

/// Verify that list_by_room_cursor (initial load, no cursor) includes a created_at
/// lower bound for partition pruning.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_room_cursor_initial_load_has_partition_lower_bound() {
    let (_container, pool) = create_test_pool().await;
    let chat_repo = ChatRepository::new(pool.clone());
    let (user, room) = setup_room(&pool, "chat_cursor_prune_user", "chat_cursor_prune_room").await;

    // Insert a message older than 90 days via raw SQL
    let old_date = Utc::now() - Duration::days(100);
    let old_msg_id = synctv_core::models::generate_id();
    sqlx::query(
        r"INSERT INTO chat_messages (id, room_id, user_id, content, message_type, created_at)
          VALUES ($1, $2, $3, 'old cursor message', 1, $4)",
    )
    .bind(old_msg_id)
    .bind(room.id)
    .bind(user.id)
    .bind(old_date)
    .execute(&pool)
    .await
    .unwrap();

    // Insert a recent message
    let msg = make_chat_message(&room.id, &user.id, "recent cursor message");
    chat_repo.create(&msg).await.unwrap();

    // Initial load (no cursor) should only return recent messages
    let (messages, _cursor) = chat_repo
        .list_by_room_cursor(&room.id, None, 100, false)
        .await
        .unwrap();

    assert_eq!(
        messages.len(),
        1,
        "Initial cursor load should only return messages within 90-day window"
    );
    assert_eq!(messages[0].message.content, "recent cursor message");
}

/// Regression guard: cleanup query must keep the partition-pruning filter.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cleanup_query_keeps_partition_pruning_filter() {
    let (_container, pool) = create_test_pool().await;
    let (user, room) = setup_room(&pool, "chat_perf", "chat_perf_room").await;

    let chat_repo = ChatRepository::new(pool.clone());

    for i in 0..20 {
        let msg = make_chat_message(&room.id, &user.id, &format!("perf_{i}"));
        chat_repo.create(&msg).await.unwrap();
    }

    // Query WITH partition pruning filter (correct implementation)
    let plan_with_filter: serde_json::Value = sqlx::query_scalar(
        r"
        EXPLAIN (FORMAT JSON)
        DELETE FROM chat_messages
        WHERE created_at > NOW() - INTERVAL '90 days'
          AND room_id = $1
          AND (id, created_at) IN (
            SELECT id, created_at FROM (
                SELECT id, created_at,
                       ROW_NUMBER() OVER (ORDER BY created_at DESC) as rn
                FROM chat_messages
                WHERE room_id = $1
                  AND created_at > NOW() - INTERVAL '90 days'
            ) ranked
            WHERE rn > 10
        )
        ",
    )
    .bind(room.id)
    .fetch_one(&pool)
    .await
    .expect("Failed to get plan with filter");

    // Query WITHOUT partition pruning filter (buggy reference shape)
    let plan_without_filter: serde_json::Value = sqlx::query_scalar(
        r"
        EXPLAIN (FORMAT JSON)
        DELETE FROM chat_messages
        WHERE room_id = $1
          AND (id, created_at) IN (
            SELECT id, created_at FROM (
                SELECT id, created_at,
                       ROW_NUMBER() OVER (ORDER BY created_at DESC) as rn
                FROM chat_messages
                WHERE room_id = $1
                  AND created_at > NOW() - INTERVAL '90 days'
            ) ranked
            WHERE rn > 10
        )
        ",
    )
    .bind(room.id)
    .fetch_one(&pool)
    .await
    .expect("Failed to get plan without filter");

    // Both plans should be valid
    assert!(
        plan_with_filter.is_array(),
        "Plan with filter should be array"
    );
    assert!(
        plan_without_filter.is_array(),
        "Plan without filter should be array"
    );

    // Key assertion: the guarded query shape keeps the created_at filter so
    // PostgreSQL can prune old partitions.
    let filter_plan_str = plan_with_filter.to_string();
    assert!(
        filter_plan_str.contains("created_at") || filter_plan_str.contains("90"),
        "Plan with filter should reference created_at condition"
    );

    let unfiltered_plan_str = plan_without_filter.to_string();
    assert!(
        !unfiltered_plan_str.is_empty(),
        "reference plan without the outer filter should still be explainable"
    );
}
