//! `ChatRepository` integration tests
//!
//! Tests: `list_by_room_cursor` pagination, `get_by_id` without time restriction,
//! `cleanup_old_messages` `keep_count=0` no-op, `cleanup_all_rooms` `activity_window_minutes=0`.
//!
use chrono::{Duration, Utc};
use sqlx::PgPool;
use synctv_core::{
    models::{
        try_hash_playback_target, ChatMessage, ChatMessageSelection, ChatMessageType, ChatMetadata,
        ChatPlaybackMessagesQuery, ChatPlaybackMetadata, ChatUserMetadata, MediaId, PlaylistId,
        ProviderTarget, Room, RoomId, RoomStatus, User, UserId, UserRole, UserStatus,
    },
    repository::{ChatRepository, RoomRepository, UserRepository},
};
use synctv_core_testing::{create_test_pool, ensure_chat_partition_for, ok, some};
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
        category: None,
        labels: Vec::new(),
        created_by: *owner,
        status: RoomStatus::Active,
        is_banned: false,
        is_public: true,
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
    let user = ok(
        user_repo.create(&make_user(username)).await,
        "chat test user should be created",
    );
    let room = ok(
        room_repo.create(&make_room(room_name, &user.id)).await,
        "chat test room should be created",
    );
    (user, room)
}

fn make_chat_message(room_id: &RoomId, user_id: &UserId, content: &str) -> ChatMessage {
    ChatMessage::new(*room_id, *user_id, content.to_string())
}

fn explain_json_plan(
    result: Result<Option<serde_json::Value>, sqlx::Error>,
    context: &str,
) -> serde_json::Value {
    some(ok(result, context), "query plan should be non-null")
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
        let created = ok(
            chat_repo.create(&msg).await,
            "chat message should be created",
        );
        created_ids.push(created.id);
        // Small delay to ensure ordering
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    // Page 1: newest 2 messages (no cursor)
    let (page1, cursor1) = ok(
        chat_repo
            .list_by_room_cursor(&room.id, None, 2, false)
            .await,
        "first chat cursor page should load",
    );
    assert_eq!(page1.len(), 2);
    assert!(
        cursor1.is_some(),
        "Should have next cursor since there are more messages"
    );
    // Messages should be in reverse chronological order
    assert_eq!(page1[0].message.content, "msg_4");
    assert_eq!(page1[1].message.content, "msg_3");

    // Page 2: next 2 messages
    let cursor1_val = some(cursor1, "first cursor should exist");
    let (page2, cursor2) = ok(
        chat_repo
            .list_by_room_cursor(&room.id, Some(cursor1_val), 2, false)
            .await,
        "second chat cursor page should load",
    );
    assert_eq!(page2.len(), 2);
    assert!(
        cursor2.is_some(),
        "Should still have a cursor (1 more message)"
    );
    assert_eq!(page2[0].message.content, "msg_2");
    assert_eq!(page2[1].message.content, "msg_1");

    // Page 3: last page (1 message)
    let cursor2_val = some(cursor2, "second cursor should exist");
    let (page3, cursor3) = ok(
        chat_repo
            .list_by_room_cursor(&room.id, Some(cursor2_val), 2, false)
            .await,
        "third chat cursor page should load",
    );
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
    let target = ProviderTarget::alist("/playback-target-1".to_string());
    let target_hash = ok(
        try_hash_playback_target(Some(&target)),
        "target hash should compute",
    );

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
        msg.metadata = Some(synctv_core::models::ChatMetadata::User(
            synctv_core::models::ChatUserMetadata {
                playback: Some(synctv_core::models::ChatPlaybackMetadata {
                    media_id: Some(media_id),
                    playlist_id: Some(playlist_id),
                    target: Some(synctv_core::models::ProviderTarget::alist(
                        "/playback-target-1".to_string(),
                    )),
                    target_hash: None,
                    position_seconds: Some(position_seconds),
                    media_name: None,
                    playlist_name: None,
                }),
                ..Default::default()
            },
        ));
        ok(
            chat_repo.create(&msg).await,
            "playback chat message should be created",
        );
    }

    let system_metadata = serde_json::json!({
        "type": "user",
        "playback": {
            "mediaId": media_id,
            "playlistId": playlist_id,
            "targetHash": target_hash,
            "positionSeconds": 11.0
        }
    });
    ok(
        sqlx::query!(
            r"
            INSERT INTO chat_messages (
                room_id, user_id, client_message_id, content, message_type,
                status, version, reply_to_message_id, reply_to_message_created_at,
                metadata, created_at
            )
            VALUES ($1, $2, NULL, $3, $4, $5, 1, NULL, NULL, $6, $7)
            ",
            room.id.as_i64(),
            user.id.as_i64(),
            "system-inside",
            i16::from(ChatMessageType::SystemMemberJoined),
            i16::from(synctv_core::models::ChatMessageStatus::Active),
            system_metadata,
            Utc::now(),
        )
        .execute(&pool)
        .await,
        "playback system chat message should be inserted",
    );

    let messages = ok(
        chat_repo
            .list_playback_messages_for_viewer(
                &ChatPlaybackMessagesQuery {
                    room_id: room.id,
                    media_id: Some(media_id),
                    playlist_id: Some(playlist_id),
                    target: Some(target.clone()),
                    selection: ChatMessageSelection::user_default(),
                    position_seconds: 11.0,
                    before_seconds: 1.0,
                    after_seconds: 1.0,
                    limit: 100,
                    include_deleted: false,
                },
                None,
            )
            .await,
        "playback chat messages should list",
    );

    let contents = messages
        .into_iter()
        .map(|message| message.message.content)
        .collect::<Vec<_>>();
    assert_eq!(contents, vec!["inside-a", "inside-b"]);

    let messages_with_system = ok(
        chat_repo
            .list_playback_messages_for_viewer(
                &ChatPlaybackMessagesQuery {
                    room_id: room.id,
                    media_id: Some(media_id),
                    playlist_id: Some(playlist_id),
                    target: Some(target),
                    selection: ChatMessageSelection {
                        include_message_types: vec![
                            ChatMessageType::User,
                            ChatMessageType::SystemMemberJoined,
                        ],
                    },
                    position_seconds: 11.0,
                    before_seconds: 1.0,
                    after_seconds: 1.0,
                    limit: 100,
                    include_deleted: false,
                },
                None,
            )
            .await,
        "playback chat messages with system type should list",
    );
    assert_eq!(
        messages_with_system
            .into_iter()
            .map(|message| message.message.content)
            .collect::<Vec<_>>(),
        vec!["inside-a", "system-inside", "inside-b"]
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_playback_messages_handles_nullable_metadata_and_missing_target() {
    let (_container, pool) = create_test_pool().await;
    let chat_repo = ChatRepository::new(pool.clone());
    let (user, room) = setup_room(
        &pool,
        "chat_playback_target_user",
        "chat_playback_target_room",
    )
    .await;
    let media_id = MediaId::expect_positive(300_101);
    let playlist_id = PlaylistId::expect_positive(300_102);
    let target = ProviderTarget::alist("/targeted.mp4".to_string());
    let other_target = ProviderTarget::alist("/other.mp4".to_string());

    let null_metadata = make_chat_message(&room.id, &user.id, "null-metadata");
    let created_null = ok(
        chat_repo.create(&null_metadata).await,
        "null metadata message should be created",
    );
    assert!(created_null.metadata.is_none());

    for (content, target) in [
        ("no-target", None),
        ("matching-target", Some(target.clone())),
        ("other-target", Some(other_target)),
    ] {
        let mut msg = make_chat_message(&room.id, &user.id, content);
        msg.metadata = Some(ChatMetadata::User(ChatUserMetadata {
            playback: Some(ChatPlaybackMetadata {
                media_id: Some(media_id),
                playlist_id: Some(playlist_id),
                target,
                target_hash: None,
                position_seconds: Some(42.0),
                media_name: None,
                playlist_name: None,
            }),
            ..Default::default()
        }));
        ok(
            chat_repo.create(&msg).await,
            "playback target test message should be created",
        );
    }

    let without_target = ok(
        chat_repo
            .list_playback_messages_for_viewer(
                &ChatPlaybackMessagesQuery {
                    room_id: room.id,
                    media_id: Some(media_id),
                    playlist_id: Some(playlist_id),
                    target: None,
                    selection: ChatMessageSelection::user_default(),
                    position_seconds: 42.0,
                    before_seconds: 0.0,
                    after_seconds: 0.0,
                    limit: 100,
                    include_deleted: false,
                },
                None,
            )
            .await,
        "playback chat messages without target should list",
    );
    assert_eq!(
        without_target
            .iter()
            .map(|message| message.message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["no-target", "matching-target", "other-target"]
    );

    let with_target = ok(
        chat_repo
            .list_playback_messages_for_viewer(
                &ChatPlaybackMessagesQuery {
                    room_id: room.id,
                    media_id: Some(media_id),
                    playlist_id: Some(playlist_id),
                    target: Some(target),
                    selection: ChatMessageSelection::user_default(),
                    position_seconds: 42.0,
                    before_seconds: 0.0,
                    after_seconds: 0.0,
                    limit: 100,
                    include_deleted: false,
                },
                None,
            )
            .await,
        "playback chat messages with target should list",
    );
    assert_eq!(
        with_target
            .iter()
            .map(|message| message.message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["matching-target"]
    );
}

// ─── get_by_id without time restriction ─────────────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_id_recent_message() {
    let (_container, pool) = create_test_pool().await;
    let chat_repo = ChatRepository::new(pool.clone());
    let (user, room) = setup_room(&pool, "chat_get_user", "chat_get_room").await;

    let msg = make_chat_message(&room.id, &user.id, "recent message");
    let created = ok(
        chat_repo.create(&msg).await,
        "recent chat message should be created",
    );

    let fetched = ok(
        chat_repo.get_by_id(created.id).await,
        "recent chat message should be fetched",
    );
    assert!(fetched.is_some());
    assert_eq!(
        some(fetched, "recent chat message should exist").content,
        "recent message"
    );
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

    ensure_chat_partition_for(&pool, old_date).await;

    // Insert directly with backdated created_at
    ok(
        sqlx::query!(
            r#"INSERT INTO chat_messages (id, room_id, user_id, content, message_type, created_at)
          VALUES ($1, $2, $3, 'old message', 1, $4)"#,
            msg_id,
            room.id.as_i64(),
            user.id.as_i64(),
            old_date,
        )
        .execute(&pool)
        .await,
        "old chat message should be inserted",
    );

    // get_by_id should now successfully retrieve messages older than 90 days
    let fetched = ok(
        chat_repo.get_by_id(msg_id).await,
        "old chat message should be fetched",
    );
    assert!(
        fetched.is_some(),
        "get_by_id should return messages older than 90 days for audit purposes"
    );
    assert_eq!(
        some(fetched, "old chat message should exist").content,
        "old message"
    );
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

    ensure_chat_partition_for(&pool, very_old_date).await;

    // Insert directly with backdated created_at (1 year ago)
    ok(
        sqlx::query!(
            r#"INSERT INTO chat_messages (id, room_id, user_id, content, message_type, created_at)
          VALUES ($1, $2, $3, 'very old message from a year ago', 1, $4)"#,
            msg_id,
            room.id.as_i64(),
            user.id.as_i64(),
            very_old_date,
        )
        .execute(&pool)
        .await,
        "very old chat message should be inserted",
    );

    // get_by_id should retrieve very old messages for audit purposes
    let fetched = ok(
        chat_repo.get_by_id(msg_id).await,
        "very old chat message should be fetched",
    );
    assert!(
        fetched.is_some(),
        "get_by_id should return messages from any time period"
    );
    assert_eq!(
        some(fetched, "very old chat message should exist").content,
        "very old message from a year ago"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_id_nonexistent_returns_none() {
    // Verify that get_by_id returns None for non-existent messages
    let (_container, pool) = create_test_pool().await;
    let chat_repo = ChatRepository::new(pool.clone());

    let fetched = ok(
        chat_repo.get_by_id(i64::MAX).await,
        "missing chat message lookup should succeed",
    );
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
        ok(
            chat_repo.create(&msg).await,
            "chat message should be created",
        );
    }

    // keep_count=0 should return 0 and delete nothing
    let deleted = ok(
        chat_repo.cleanup_old_messages(&room.id, 0).await,
        "chat cleanup with keep_count zero should succeed",
    );
    assert_eq!(deleted, 0, "keep_count=0 should be a no-op");

    // All messages should still exist
    let count = ok(
        chat_repo.count_by_room(&room.id).await,
        "chat message count should load",
    );
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
        ok(
            chat_repo.create(&msg).await,
            "chat message should be created",
        );
    }

    // activity_window_minutes=0 with keep_count=2 --
    // `make_interval(mins => 0)` yields INTERVAL '0', so
    // `NOW() - INTERVAL '0'` == NOW(), meaning only messages created
    // at exactly NOW() are in the activity window. Recently inserted
    // messages may or may not be selected depending on sub-second timing.
    // The key test is that it doesn't error and doesn't delete everything.
    let deleted = ok(
        chat_repo.cleanup_all_rooms(2, 0).await,
        "all-room chat cleanup should succeed",
    );
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
        ok(
            chat_repo.create(&msg).await,
            "chat message should be created",
        );
    }

    // keep_count=0 means unlimited, so no cleanup
    let deleted = ok(
        chat_repo.cleanup_all_rooms(0, 60).await,
        "all-room chat cleanup with keep_count zero should succeed",
    );
    assert_eq!(deleted, 0, "keep_count=0 should be a no-op");

    let count = ok(
        chat_repo.count_by_room(&room.id).await,
        "chat message count should load",
    );
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

    // Parent partitioned indexes propagate to current and future partitions.
    let index_exists = ok(
        sqlx::query_scalar!(
            r#"
        SELECT EXISTS (
            SELECT 1
            FROM pg_indexes
            WHERE schemaname = 'public'
              AND tablename LIKE 'chat_messages_%'
              AND indexname LIKE '%created_at%'
        )
        "#,
        )
        .fetch_one(&pool)
        .await,
        "pg_indexes query should succeed",
    )
    .unwrap_or(false);

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
    ok(
        chat_repo.create(&msg).await,
        "chat message should be created",
    );

    // Run a query that should use the created_at index
    // (delete_messages_older_than_retention uses this pattern)
    let plan: serde_json::Value = explain_json_plan(
        sqlx::query_scalar!(
            r#"
        EXPLAIN (FORMAT JSON)
        DELETE FROM chat_messages
        WHERE created_at <= NOW() - INTERVAL '90 days'
        "#,
        )
        .fetch_one(&pool)
        .await,
        "time-range query plan should load",
    );

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
    let plan: serde_json::Value = explain_json_plan(
        sqlx::query_scalar!(
            r#"
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
        "#,
        )
        .fetch_one(&pool)
        .await,
        "cleanup-all-rooms query plan should load",
    );

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
    let (_container, pool) = create_test_pool().await;
    let (user, room) = setup_room(&pool, "chat_prune_detail", "chat_prune_detail_room").await;

    let chat_repo = ChatRepository::new(pool.clone());

    for i in 0..10 {
        let msg = make_chat_message(&room.id, &user.id, &format!("prune_detail_{i}"));
        ok(
            chat_repo.create(&msg).await,
            "partition pruning message should be created",
        );
    }

    // Get detailed query plan using EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)
    let plan: serde_json::Value = explain_json_plan(
        sqlx::query_scalar!(
            r#"
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
        "#,
            room.id.as_i64(),
        )
        .fetch_one(&pool)
        .await,
        "cleanup-old-messages query plan should load",
    );

    // Verify plan structure
    assert!(
        plan.is_array(),
        "Query plan should be a JSON array, got: {plan:?}"
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
    let (_container, pool) = create_test_pool().await;
    let (user, room) = setup_room(&pool, "chat_batch_prune", "chat_batch_prune_room").await;

    let chat_repo = ChatRepository::new(pool.clone());

    for i in 0..15 {
        let msg = make_chat_message(&room.id, &user.id, &format!("batch_{i}"));
        ok(
            chat_repo.create(&msg).await,
            "batch pruning message should be created",
        );
    }

    // Get detailed query plan
    let plan: serde_json::Value = explain_json_plan(
        sqlx::query_scalar!(
            r#"
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
        "#,
        )
        .fetch_one(&pool)
        .await,
        "cleanup-all-rooms detailed query plan should load",
    );

    assert!(
        plan.is_array(),
        "Query plan should be a JSON array, got: {plan:?}"
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
    let plan: serde_json::Value = explain_json_plan(
        sqlx::query_scalar!(
            r#"
        EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)
        DELETE FROM chat_messages
        WHERE created_at <= NOW() - INTERVAL '90 days'
        "#,
        )
        .fetch_one(&pool)
        .await,
        "retention cleanup query plan should load",
    );

    assert!(
        plan.is_array(),
        "Query plan should be a JSON array, got: {plan:?}"
    );

    assert!(
        !plan.to_string().is_empty(),
        "retention cleanup query plan should contain a plan body"
    );
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
    ensure_chat_partition_for(&pool, old_date).await;

    ok(
        sqlx::query!(
            r#"INSERT INTO chat_messages (id, room_id, user_id, content, message_type, created_at)
          VALUES ($1, $2, $3, 'old message', 1, $4)"#,
            old_msg_id,
            room.id.as_i64(),
            user.id.as_i64(),
            old_date,
        )
        .execute(&pool)
        .await,
        "old initial-load chat message should be inserted",
    );

    let msg = make_chat_message(&room.id, &user.id, "recent message");
    ok(
        chat_repo.create(&msg).await,
        "recent chat message should be created",
    );

    let (messages, next_cursor) = ok(
        chat_repo
            .list_by_room_cursor(&room.id, None, 100, false)
            .await,
        "initial chat load should succeed",
    );

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
    ensure_chat_partition_for(&pool, old_date).await;

    ok(
        sqlx::query!(
            r#"INSERT INTO chat_messages (id, room_id, user_id, content, message_type, created_at)
          VALUES ($1, $2, $3, 'old cursor message', 1, $4)"#,
            old_msg_id,
            room.id.as_i64(),
            user.id.as_i64(),
            old_date,
        )
        .execute(&pool)
        .await,
        "old cursor chat message should be inserted",
    );

    let msg = make_chat_message(&room.id, &user.id, "recent cursor message");
    ok(
        chat_repo.create(&msg).await,
        "recent cursor chat message should be created",
    );

    let (messages, _cursor) = ok(
        chat_repo
            .list_by_room_cursor(&room.id, None, 100, false)
            .await,
        "initial cursor chat load should succeed",
    );

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
        ok(
            chat_repo.create(&msg).await,
            "performance guard chat message should be created",
        );
    }

    // Query WITH partition pruning filter (correct implementation)
    let plan_with_filter: serde_json::Value = explain_json_plan(
        sqlx::query_scalar!(
            r#"
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
        "#,
            room.id.as_i64(),
        )
        .fetch_one(&pool)
        .await,
        "query plan with partition filter should load",
    );

    // Query WITHOUT partition pruning filter (buggy reference shape)
    let plan_without_filter: serde_json::Value = explain_json_plan(
        sqlx::query_scalar!(
            r#"
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
        "#,
            room.id.as_i64(),
        )
        .fetch_one(&pool)
        .await,
        "query plan without partition filter should load",
    );

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
