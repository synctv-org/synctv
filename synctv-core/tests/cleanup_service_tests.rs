//! `CleanupService` tests (S12b)
//!
//! Tests zero retention skipping all tasks, and non-leader skipping cleanup.
//! These are unit-style tests that don't need a real database for the leader/config
//! checks (but use testcontainers for `run_all` verification).
//!

use std::sync::Arc;

use chrono::{Duration, Utc};
use sqlx::PgPool;
use synctv_core::models::{
    CreateFileUploadSession, FileReferenceTarget, FileUploadSessionCreateResult, NewStoredFile,
    Room, RoomId, RoomStatus, User, UserId, UserRole, UserStatus,
};
use synctv_core::repository::realtime_outbox::RealtimeOutboxStatus;
use synctv_core::repository::{RoomRepository, UserRepository};
use synctv_core::service::{
    AlwaysLeader, CleanupConfig, CleanupService, CleanupServiceOptions, FileStorageCleanupOrigin,
    FileStorageContext, FileStorageService, LeaderCheck,
};
use synctv_core::Error;
use synctv_core_testing::{
    create_test_pool, ensure_chat_partition_for, ok, TestOptionExt, TestResultExt,
};

/// A `LeaderCheck` that always returns false
struct NeverLeader;

impl LeaderCheck for NeverLeader {
    fn is_leader(&self) -> bool {
        false
    }
}

#[derive(Default)]
struct RecordingFileStorageService {
    deleted_object_keys: std::sync::Mutex<Vec<String>>,
    deleted_origins: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl FileStorageService for RecordingFileStorageService {
    fn backend_name(&self) -> &'static str {
        "test-storage"
    }

    async fn create_upload_session(
        &self,
        _request: CreateFileUploadSession,
    ) -> synctv_core::Result<FileUploadSessionCreateResult> {
        Err(Error::Internal("not used".to_string()))
    }

    async fn prepare_files(
        &self,
        _context: FileStorageContext<'_>,
        files: Vec<NewStoredFile>,
    ) -> synctv_core::Result<Vec<NewStoredFile>> {
        Ok(files)
    }

    async fn delete_files(
        &self,
        origin: FileStorageCleanupOrigin,
        files: &[FileReferenceTarget],
    ) -> synctv_core::Result<()> {
        let mut deleted = ok(
            self.deleted_object_keys.lock(),
            "deleted object key recorder lock should be acquired",
        );
        deleted.extend(files.iter().map(|file| file.object_key.clone()));
        let mut origins = ok(
            self.deleted_origins.lock(),
            "deleted origin recorder lock should be acquired",
        );
        origins.extend(files.iter().map(|_| origin.as_str().to_string()));
        Ok(())
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_zero_retention_skips_all_tasks() {
    let (_container, pool) = create_test_pool().await;

    // All zero retention values
    let config = CleanupConfig {
        soft_delete_retention_days: 0,
        room_soft_delete_retention_days: 0,
        resource_soft_delete_retention_days: 0,
        expired_token_retention_days: 0,
        expired_credential_buffer_hours: 0,
        notification_retention_days: 0,
        notification_max_retention_days: 0,
        chat_max_messages_per_room: 0,
        room_resource_event_retention_seconds: 0,
        chat_message_event_retention_seconds: 0,
        playback_progress_retention_days: 0,
        unreferenced_file_retention_seconds: 0,
        realtime_outbox_sent_retention_days: 0,
        realtime_outbox_dead_retention_days: 0,
    };

    let service = CleanupService::new(pool, config, Arc::new(AlwaysLeader));
    let result = service.run_all().await;

    // All counters should be 0 since all tasks are skipped
    assert_eq!(
        result.users_purged, 0,
        "Zero retention should skip user purge"
    );
    assert_eq!(
        result.rooms_purged, 0,
        "Zero retention should skip room purge"
    );
    assert_eq!(
        result.tokens_deleted, 0,
        "Zero retention should skip token cleanup"
    );
    assert_eq!(
        result.credentials_deleted, 0,
        "Zero retention should skip credential cleanup"
    );
    assert_eq!(
        result.notifications_deleted, 0,
        "Zero retention should skip notification cleanup"
    );
    assert_eq!(
        result.chat_messages_deleted, 0,
        "Zero retention should skip chat cleanup"
    );
    assert_eq!(result.chat_message_events_deleted, 0);
    assert_eq!(result.room_resource_events_deleted, 0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn chat_message_event_cleanup_uses_retention_window() {
    let (_container, pool) = create_test_pool().await;
    let user = create_test_user(&pool).await;
    let room = create_test_room(user.id, None);
    let room = ok(
        RoomRepository::new(pool.clone()).create(&room).await,
        "test room should be created",
    );
    let message_created_at = Utc::now();
    insert_chat_text_message(&pool, room.id, user.id, 10_101, message_created_at).await;
    let old_created_at = Utc::now() - Duration::days(91);
    let new_created_at = Utc::now() - Duration::days(1);
    insert_chat_event(
        &pool,
        &room.id,
        user.id,
        10_101,
        message_created_at,
        "old-event",
        old_created_at,
    )
    .await;
    insert_chat_event(
        &pool,
        &room.id,
        user.id,
        10_101,
        message_created_at,
        "new-event",
        new_created_at,
    )
    .await;

    let service = CleanupService::new(
        pool.clone(),
        CleanupConfig {
            soft_delete_retention_days: 0,
            room_soft_delete_retention_days: 0,
            resource_soft_delete_retention_days: 0,
            expired_token_retention_days: 0,
            expired_credential_buffer_hours: 0,
            notification_retention_days: 0,
            notification_max_retention_days: 0,
            chat_max_messages_per_room: 0,
            room_resource_event_retention_seconds: 0,
            chat_message_event_retention_seconds: 30 * 24 * 60 * 60,
            playback_progress_retention_days: 0,
            unreferenced_file_retention_seconds: 0,
            realtime_outbox_sent_retention_days: 0,
            realtime_outbox_dead_retention_days: 0,
        },
        Arc::new(AlwaysLeader),
    );

    let result = service.run_all().await;

    assert_eq!(result.chat_message_events_deleted, 1);
    let remaining = ok(
        sqlx::query_scalar!(
            r#"SELECT ARRAY_AGG(event_id ORDER BY event_id) AS "event_ids?: Vec<String>" FROM chat_message_events"#
        )
        .fetch_one(&pool)
        .await,
        "remaining chat event ids should load",
    )
    .unwrap_or_default();
    assert_eq!(remaining, vec!["new-event".to_string()]);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn active_room_cap_cleanup_progresses_past_first_room_batch() {
    let (_container, pool) = create_test_pool().await;
    let user = create_test_user(&pool).await;
    let room_repository = RoomRepository::new(pool.clone());
    let now = ok(
        sqlx::query_scalar!(r#"SELECT NOW() AS "now!: chrono::DateTime<Utc>""#)
            .fetch_one(&pool)
            .await,
        "database time should load",
    );
    let mut last_room_id = None;
    ensure_chat_partition_for(&pool, now).await;

    for room_index in 0..101_i64 {
        let room = create_test_room(user.id, None);
        let room = ok(
            room_repository.create(&room).await,
            "test room should be created",
        );
        last_room_id = Some(room.id);
        for message_index in 0..2_i64 {
            let created_at = now - Duration::seconds(10 - message_index);
            insert_chat_text_message_without_partition_setup(
                &pool,
                room.id,
                user.id,
                20_000 + room_index * 10 + message_index,
                created_at,
            )
            .await;
        }
    }

    let service = CleanupService::new(
        pool.clone(),
        CleanupConfig {
            soft_delete_retention_days: 0,
            room_soft_delete_retention_days: 0,
            resource_soft_delete_retention_days: 0,
            expired_token_retention_days: 0,
            expired_credential_buffer_hours: 0,
            notification_retention_days: 0,
            notification_max_retention_days: 0,
            chat_max_messages_per_room: 1,
            room_resource_event_retention_seconds: 0,
            chat_message_event_retention_seconds: 0,
            playback_progress_retention_days: 0,
            unreferenced_file_retention_seconds: 0,
            realtime_outbox_sent_retention_days: 0,
            realtime_outbox_dead_retention_days: 0,
        },
        Arc::new(AlwaysLeader),
    );

    let active_over_cap_count = ok(
        sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM (
                SELECT room_id
                FROM chat_messages
                WHERE created_at > NOW() - make_interval(days => $2)
                GROUP BY room_id
                HAVING COUNT(*) > $3
                   AND MAX(created_at) >= NOW() - make_interval(mins => $1)
            ) over_cap
            "#,
            24 * 60,
            90,
            1_i64,
        )
        .fetch_one(&pool)
        .await,
        "active over-cap room count should load",
    );
    assert_eq!(active_over_cap_count, 101);
    let last_room_id = last_room_id.checked("last room should be created");
    let deletable_in_last_room = ok(
        sqlx::query_scalar!(
            r#"
            WITH retained AS (
                SELECT id, created_at
                FROM chat_messages
                WHERE room_id = $1
                  AND created_at > NOW() - make_interval(days => $2)
                ORDER BY created_at DESC, id DESC
                LIMIT $3
            )
            SELECT COUNT(*) AS "count!"
            FROM chat_messages m
            WHERE m.room_id = $1
              AND m.created_at > NOW() - make_interval(days => $2)
              AND NOT EXISTS (
                  SELECT 1
                  FROM retained r
                  WHERE r.id = m.id
                    AND r.created_at = m.created_at
              )
            "#,
            last_room_id.as_i64(),
            90,
            1_i64,
        )
        .fetch_one(&pool)
        .await,
        "last room deletable count should load",
    );
    assert_eq!(deletable_in_last_room, 1);

    let result = service.run_all().await;

    assert_eq!(result.chat_messages_deleted, 101);
    let last_room_count = ok(
        sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM chat_messages
            WHERE room_id = $1
            "#,
            last_room_id.as_i64(),
        )
        .fetch_one(&pool)
        .await,
        "last room message count should load",
    );
    assert_eq!(last_room_count, 1);
}

// Note: start_periodic checks is_leader() inside the loop. We test that NeverLeader
// causes the service to skip. Since start_periodic is a background task, we verify
// the concept by calling run_all directly with NeverLeader not being meaningful at
// that level -- run_all is always called.
// The actual leader check happens in start_periodic. So we test
// the config-level skip and verify NeverLeader compiles and works.

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_non_leader_periodic_skips() {
    let (_container, pool) = create_test_pool().await;

    let config = CleanupConfig::default();
    let service = CleanupService::new(pool, config, Arc::new(NeverLeader));

    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancel.clone();

    let handle = service.start_periodic(1, cancel_clone);

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    cancel.cancel();

    handle
        .await
        .checked("cleanup background task should finish");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_run_all_on_empty_database() {
    let (_container, pool) = create_test_pool().await;

    let config = CleanupConfig::default();
    let service = CleanupService::new(pool, config, Arc::new(AlwaysLeader));
    let result = service.run_all().await;

    // Empty database means nothing to clean
    assert_eq!(result.users_purged, 0);
    assert_eq!(result.rooms_purged, 0);
    assert_eq!(result.tokens_deleted, 0);
    assert_eq!(result.notifications_deleted, 0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_partial_config_only_some_tasks_enabled() {
    let (_container, pool) = create_test_pool().await;

    // Only user and room purge enabled, everything else disabled
    let config = CleanupConfig {
        soft_delete_retention_days: 30,
        room_soft_delete_retention_days: 30,
        resource_soft_delete_retention_days: 0,
        expired_token_retention_days: 0,
        expired_credential_buffer_hours: 0,
        notification_retention_days: 0,
        notification_max_retention_days: 0,
        chat_max_messages_per_room: 0,
        room_resource_event_retention_seconds: 0,
        chat_message_event_retention_seconds: 0,
        playback_progress_retention_days: 0,
        unreferenced_file_retention_seconds: 0,
        realtime_outbox_sent_retention_days: 0,
        realtime_outbox_dead_retention_days: 0,
    };

    let service = CleanupService::new(pool, config, Arc::new(AlwaysLeader));
    let result = service.run_all().await;

    // Token/credential/notification/chat should be 0 since disabled
    assert_eq!(result.tokens_deleted, 0, "Disabled tasks should return 0");
    assert_eq!(
        result.credentials_deleted, 0,
        "Disabled tasks should return 0"
    );
    assert_eq!(
        result.notifications_deleted, 0,
        "Disabled tasks should return 0"
    );
    assert_eq!(
        result.chat_messages_deleted, 0,
        "Disabled tasks should return 0"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_realtime_outbox_cleanup_retains_actionable_rows() {
    let (_container, pool) = create_test_pool().await;
    let now = Utc::now();
    let old_sent_at = now - Duration::days(10);
    let new_sent_at = now - Duration::days(1);
    let old_dead_at = now - Duration::days(40);
    let old_pending_at = now - Duration::days(40);

    for (id, status, created_at, dispatched_at) in [
        (
            "outbox-sent-old",
            RealtimeOutboxStatus::Sent,
            old_sent_at,
            Some(old_sent_at),
        ),
        (
            "outbox-sent-new",
            RealtimeOutboxStatus::Sent,
            new_sent_at,
            Some(new_sent_at),
        ),
        (
            "outbox-dead-old",
            RealtimeOutboxStatus::Dead,
            old_dead_at,
            None,
        ),
        (
            "outbox-pending-old",
            RealtimeOutboxStatus::Pending,
            old_pending_at,
            None,
        ),
    ] {
        ok(
            sqlx::query!(
                r#"
                INSERT INTO realtime_outbox (
                    id, aggregate_type, aggregate_id, event_type, payload, status,
                    next_retry_at, created_at, dispatched_at
                )
                VALUES ($1, 'room', '1', 'room_updated', '{}'::jsonb, $2, $3, $4, $5)
                "#,
                id,
                status.as_i16(),
                created_at,
                created_at,
                dispatched_at,
            )
            .execute(&pool)
            .await,
            "outbox fixture row should be inserted",
        );
    }

    let service = CleanupService::new(
        pool.clone(),
        CleanupConfig {
            soft_delete_retention_days: 0,
            room_soft_delete_retention_days: 0,
            resource_soft_delete_retention_days: 0,
            expired_token_retention_days: 0,
            expired_credential_buffer_hours: 0,
            notification_retention_days: 0,
            notification_max_retention_days: 0,
            chat_max_messages_per_room: 0,
            room_resource_event_retention_seconds: 0,
            chat_message_event_retention_seconds: 0,
            playback_progress_retention_days: 0,
            unreferenced_file_retention_seconds: 0,
            realtime_outbox_sent_retention_days: 7,
            realtime_outbox_dead_retention_days: 30,
        },
        Arc::new(AlwaysLeader),
    );

    let result = service.run_all().await;
    assert_eq!(result.realtime_outbox_deleted, 2);

    let remaining_ids = ok(
        sqlx::query_scalar!(
            r#"
            SELECT id
            FROM realtime_outbox
            ORDER BY id
            "#
        )
        .fetch_all(&pool)
        .await,
        "remaining outbox rows should be listed",
    );
    assert_eq!(
        remaining_ids,
        vec![
            "outbox-pending-old".to_string(),
            "outbox-sent-new".to_string()
        ]
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_run_all_purges_soft_deleted_user_after_room_and_membership_cleanup() {
    let (_container, pool) = create_test_pool().await;

    let room_owner = create_test_user(&pool).await;
    let deleted_user = create_test_user(&pool).await;

    let deleted_owned_room = create_test_room(deleted_user.id, None);
    let surviving_room = create_test_room(room_owner.id, None);

    let room_repo = RoomRepository::new(pool.clone());
    let deleted_owned_room = ok(
        room_repo.create(&deleted_owned_room).await,
        "deleted user's owned room should be created",
    );
    let surviving_room = ok(
        room_repo.create(&surviving_room).await,
        "surviving room should be created",
    );

    ok(
        sqlx::query!(
            r#"
            INSERT INTO room_resource_events (
                event_id, scope_type, room_id, aggregate_type, aggregate_id,
                resource_type, resource_id, event_type, event_version,
                payload, summary, occurred_at
            ) VALUES ($1, 1, $2, 'room', $3, 'room', $3, 'room_deleted', 1,
                      NULL, '{}'::JSONB, CURRENT_TIMESTAMP)
            "#,
            format!("deleted-room-resource-event-{}", deleted_owned_room.id),
            deleted_owned_room.id.as_i64(),
            deleted_owned_room.id.to_string(),
        )
        .execute(&pool)
        .await,
        "deleted room resource event should be inserted",
    );

    let room_cover_reference_id = ok(
        sqlx::query_scalar!(
            r#"WITH object AS (
                   INSERT INTO file_objects (
                       storage_backend, object_key, mime_type, size_bytes,
                       content_manifest_sha256, metadata, validated_at
                   )
                   VALUES ('local', 'cleanup-room-cover', 'image/png', 1,
                           repeat('a', 64), '{}'::JSONB, CURRENT_TIMESTAMP)
               )
               INSERT INTO file_references (
                   storage_backend, object_key, reference_kind, reference_id, metadata
               )
               VALUES ('local', 'cleanup-room-cover', 'room_cover', 'cleanup-room', '{}'::JSONB)
               RETURNING id"#,
        )
        .fetch_one(&pool)
        .await,
        "room cover reference should be created",
    );
    let avatar_reference_id = ok(
        sqlx::query_scalar!(
            r#"WITH object AS (
                   INSERT INTO file_objects (
                       storage_backend, object_key, mime_type, size_bytes,
                       content_manifest_sha256, metadata, validated_at
                   )
                   VALUES ('local', 'cleanup-user-avatar', 'image/png', 1,
                           repeat('b', 64), '{}'::JSONB, CURRENT_TIMESTAMP)
               )
               INSERT INTO file_references (
                   storage_backend, object_key, reference_kind, reference_id, metadata
               )
               VALUES ('local', 'cleanup-user-avatar', 'user_avatar', 'cleanup-user', '{}'::JSONB)
               RETURNING id"#,
        )
        .fetch_one(&pool)
        .await,
        "avatar reference should be created",
    );
    ok(
        sqlx::query!(
            "UPDATE rooms SET cover_file_reference_id = $2 WHERE id = $1",
            deleted_owned_room.id.as_i64(),
            room_cover_reference_id,
        )
        .execute(&pool)
        .await,
        "room cover should be attached",
    );
    ok(
        sqlx::query!(
            "UPDATE users SET avatar_file_reference_id = $2 WHERE id = $1",
            deleted_user.id.as_i64(),
            avatar_reference_id,
        )
        .execute(&pool)
        .await,
        "user avatar should be attached",
    );
    let room_attachment_created_at = Utc::now() - Duration::days(2);
    insert_chat_message_with_image(
        &pool,
        deleted_owned_room.id,
        deleted_user.id,
        91_001,
        room_attachment_created_at,
        "room-image",
        "cleanup-room-chat-object",
    )
    .await;
    let room_attachment_reference_id = format!(
        "{}:{}:{}:{}",
        deleted_owned_room.id.as_i64(),
        91_001_i64,
        room_attachment_created_at.timestamp_micros(),
        "room-image"
    );
    insert_file_reference(
        &pool,
        "cleanup-room-chat-object",
        "chat_message_attachment",
        &room_attachment_reference_id,
    )
    .await;
    ok(
        sqlx::query!(
            "INSERT INTO room_join_requests (room_id, user_id, status) VALUES ($1, $2, 1)",
            deleted_owned_room.id.as_i64(),
            room_owner.id.as_i64(),
        )
        .execute(&pool)
        .await,
        "room join request should be created",
    );
    ok(
        sqlx::query!(
            "INSERT INTO room_bans (room_id, banned_by, reason) VALUES ($1, $2, 'test')",
            deleted_owned_room.id.as_i64(),
            room_owner.id.as_i64(),
        )
        .execute(&pool)
        .await,
        "room ban should be created",
    );

    let forty_days_ago = Utc::now() - Duration::days(40);
    let nested_creator = create_test_user(&pool).await;
    let retained_parent_playlist_id = ok(
        sqlx::query_scalar!(
            r#"INSERT INTO playlists (
                   room_id, creator_id, name, position, deleted_at, deletion_source
               )
               VALUES ($1, $2, 'retained-user-parent', 1.0, $3, 'user')
               RETURNING id"#,
            surviving_room.id.as_i64(),
            deleted_user.id.as_i64(),
            forty_days_ago,
        )
        .fetch_one(&pool)
        .await,
        "retained parent playlist should be inserted",
    );
    let retained_child_playlist_id = ok(
        sqlx::query_scalar!(
            r#"INSERT INTO playlists (
                   room_id, creator_id, name, parent_id, position,
                   deleted_at, deletion_source
               )
               VALUES ($1, $2, 'retained-other-user-child', $3, 1.0, $4, 'user')
               RETURNING id"#,
            surviving_room.id.as_i64(),
            nested_creator.id.as_i64(),
            retained_parent_playlist_id,
            forty_days_ago,
        )
        .fetch_one(&pool)
        .await,
        "retained child playlist should be inserted",
    );
    let retained_media_id = ok(
        sqlx::query_scalar!(
            r#"INSERT INTO media (
                   playlist_id, room_id, creator_id, name, position,
                   source_provider, source_config, deleted_at, deletion_source
               )
               VALUES ($1, $2, $3, 'retained-child-media', 1.0,
                       1, '{}'::JSONB, $4, 'user')
               RETURNING id"#,
            retained_child_playlist_id,
            surviving_room.id.as_i64(),
            nested_creator.id.as_i64(),
            forty_days_ago,
        )
        .fetch_one(&pool)
        .await,
        "retained child media should be inserted",
    );
    ok(
        sqlx::query!(
            r#"INSERT INTO room_playback_state (
                   room_id, playing_media_id, playing_playlist_id,
                   speed, is_playing, playback_generation, version
               )
               VALUES ($1, $2, $3, 1.0, TRUE, 0, 0)"#,
            surviving_room.id.as_i64(),
            retained_media_id,
            retained_child_playlist_id,
        )
        .execute(&pool)
        .await,
        "retained child resource playback state should be inserted",
    );
    ok(
        sqlx::query!(
            "UPDATE users
         SET deleted_at = $2, updated_at = $2, deletion_source = 'account'
         WHERE id = $1",
            deleted_user.id.as_i64(),
            forty_days_ago,
        )
        .execute(&pool)
        .await,
        "test user should be soft-deleted",
    );

    ok(
        sqlx::query!(
            "UPDATE rooms
         SET deleted_at = $2, updated_at = $2,
             deletion_source = 'account', deleted_owner_id = $3
         WHERE id = $1",
            deleted_owned_room.id.as_i64(),
            forty_days_ago,
            deleted_user.id.as_i64(),
        )
        .execute(&pool)
        .await,
        "owned room should be soft-deleted",
    );

    ok(
        sqlx::query!(
            "INSERT INTO room_members (room_id, user_id, role, joined_at, version)
         VALUES ($1, $2, $3, $4, 0)",
            surviving_room.id.as_i64(),
            deleted_user.id.as_i64(),
            3_i16,
            forty_days_ago,
        )
        .execute(&pool)
        .await,
        "historical room membership should be inserted",
    );

    let service = CleanupService::new(
        pool.clone(),
        CleanupConfig {
            soft_delete_retention_days: 30,
            room_soft_delete_retention_days: 30,
            resource_soft_delete_retention_days: 0,
            expired_token_retention_days: 0,
            expired_credential_buffer_hours: 0,
            notification_retention_days: 0,
            notification_max_retention_days: 0,
            chat_max_messages_per_room: 0,
            room_resource_event_retention_seconds: 0,
            chat_message_event_retention_seconds: 0,
            playback_progress_retention_days: 0,
            unreferenced_file_retention_seconds: 0,
            realtime_outbox_sent_retention_days: 0,
            realtime_outbox_dead_retention_days: 0,
        },
        Arc::new(AlwaysLeader),
    );

    let result = service.run_all().await;

    assert_eq!(
        result.rooms_purged, 1,
        "Cleanup should purge the user's soft-deleted owned room first"
    );
    let room_attachment_reference = ok(
        sqlx::query_scalar!(
            "SELECT expires_at FROM file_references WHERE reference_kind = 'chat_message_attachment' AND reference_id = $1",
            room_attachment_reference_id,
        )
        .fetch_optional(&pool)
        .await,
        "hard-purged room attachment reference should be queryable",
    );
    assert!(
        room_attachment_reference.flatten().is_some(),
        "hard-purged room chat attachments must retain an expired reference"
    );
    assert_eq!(
        result.users_purged, 1,
        "Cleanup should purge the soft-deleted user in the same run"
    );

    let user_still_exists = ok(
        sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)",
            deleted_user.id.as_i64()
        )
        .fetch_one(&pool)
        .await,
        "deleted user existence query should succeed",
    )
    .unwrap_or(false);
    assert!(
        !user_still_exists,
        "Soft-deleted user should be hard-deleted"
    );
    let retained_resource_count = ok(
        sqlx::query_scalar!(
            r#"SELECT (
                   (SELECT COUNT(*) FROM playlists WHERE id = ANY($1))
                   + (SELECT COUNT(*) FROM media WHERE id = $2)
               ) AS "count!""#,
            &[retained_parent_playlist_id, retained_child_playlist_id],
            retained_media_id,
        )
        .fetch_one(&pool)
        .await,
        "retained resource existence should be queryable",
    );
    assert_eq!(retained_resource_count, 0);
    let surviving_playback = ok(
        sqlx::query!(
            r#"SELECT playing_media_id, playing_playlist_id, is_playing AS "is_playing!"
               FROM room_playback_state
               WHERE room_id = $1"#,
            surviving_room.id.as_i64(),
        )
        .fetch_one(&pool)
        .await,
        "surviving room playback should be queryable",
    );
    assert!(surviving_playback.playing_media_id.is_none());
    assert!(surviving_playback.playing_playlist_id.is_none());
    assert!(!surviving_playback.is_playing);

    let membership_still_exists = ok(
        sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM room_members WHERE user_id = $1)",
            deleted_user.id.as_i64()
        )
        .fetch_one(&pool)
        .await,
        "historical membership existence query should succeed",
    )
    .unwrap_or(false);
    assert!(
        !membership_still_exists,
        "Historical room_members rows must not block hard deletion of soft-deleted users"
    );

    let expired_references = ok(
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!"
               FROM file_references
               WHERE id = ANY($1)
                 AND expires_at IS NOT NULL
                 AND released_at IS NULL"#,
            &[room_cover_reference_id, avatar_reference_id],
        )
        .fetch_one(&pool)
        .await,
        "retained file references should be queryable",
    );
    assert_eq!(expired_references, 2);
    assert!(!ok(
        sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM room_resource_events WHERE room_id = $1) AS \"exists!\"",
            deleted_owned_room.id.as_i64(),
        )
        .fetch_one(&pool)
        .await,
        "deleted room resource events should be purged",
    ));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn account_owned_rooms_follow_the_account_recovery_window() {
    let (_container, pool) = create_test_pool().await;
    let deleted_user = create_test_user(&pool).await;
    let room = ok(
        RoomRepository::new(pool.clone())
            .create(&create_test_room(deleted_user.id, None))
            .await,
        "account room should be created",
    );
    let forty_days_ago = Utc::now() - Duration::days(40);
    ok(
        sqlx::query!(
            "UPDATE users SET deleted_at = $2, deletion_source = 'account' WHERE id = $1",
            deleted_user.id.as_i64(),
            forty_days_ago,
        )
        .execute(&pool)
        .await,
        "account should enter recovery window",
    );
    ok(
        sqlx::query!(
            "UPDATE rooms SET deleted_at = $2, deletion_source = 'account', deleted_owner_id = $1 WHERE id = $3",
            deleted_user.id.as_i64(),
            forty_days_ago,
            room.id.as_i64(),
        )
        .execute(&pool)
        .await,
        "room should enter account recovery window",
    );

    let service = CleanupService::new(
        pool.clone(),
        CleanupConfig {
            soft_delete_retention_days: 90,
            room_soft_delete_retention_days: 30,
            resource_soft_delete_retention_days: 0,
            expired_token_retention_days: 0,
            expired_credential_buffer_hours: 0,
            notification_retention_days: 0,
            notification_max_retention_days: 0,
            chat_max_messages_per_room: 0,
            room_resource_event_retention_seconds: 0,
            chat_message_event_retention_seconds: 0,
            playback_progress_retention_days: 0,
            unreferenced_file_retention_seconds: 0,
            realtime_outbox_sent_retention_days: 0,
            realtime_outbox_dead_retention_days: 0,
        },
        Arc::new(AlwaysLeader),
    );
    let protected = service.run_all().await;
    assert_eq!(protected.rooms_purged, 0);
    assert_eq!(protected.users_purged, 0);
    assert!(ok(
        sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM rooms WHERE id = $1)",
            room.id.as_i64()
        )
        .fetch_one(&pool)
        .await,
        "protected room existence query should succeed",
    )
    .unwrap_or(false));

    let one_hundred_days_ago = Utc::now() - Duration::days(100);
    ok(
        sqlx::query!(
            "UPDATE users SET deleted_at = $2 WHERE id = $1",
            deleted_user.id.as_i64(),
            one_hundred_days_ago,
        )
        .execute(&pool)
        .await,
        "account recovery window should expire",
    );
    let expired = service.run_all().await;
    assert_eq!(expired.rooms_purged, 1);
    assert_eq!(expired.users_purged, 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn hard_purge_removes_deleted_user_chat_and_resource_events() {
    let (_container, pool) = create_test_pool().await;
    let deleted_user = create_test_user(&pool).await;
    let room_owner = create_test_user(&pool).await;
    let room = ok(
        RoomRepository::new(pool.clone())
            .create(&create_test_room(room_owner.id, None))
            .await,
        "surviving event room should be created",
    );
    let message_created_at = Utc::now() - Duration::days(2);
    insert_chat_text_message(&pool, room.id, deleted_user.id, 92_001, message_created_at).await;
    let attachment_created_at = Utc::now() - Duration::days(2);
    insert_chat_message_with_image(
        &pool,
        room.id,
        deleted_user.id,
        92_002,
        attachment_created_at,
        "purge-image",
        "purge-object",
    )
    .await;
    let attachment_reference_id = format!(
        "{}:{}:{}:{}",
        room.id.as_i64(),
        92_002_i64,
        attachment_created_at.timestamp_micros(),
        "purge-image"
    );
    insert_file_reference(
        &pool,
        "purge-object",
        "chat_message_attachment",
        &attachment_reference_id,
    )
    .await;
    ok(
        sqlx::query!(
            r#"
            INSERT INTO chat_message_mentions (
                room_id, message_id, message_created_at, mentioned_user_id,
                start_char, length_chars
            ) VALUES ($1, $2, $3, $4, 0, 1)
            "#,
            room.id.as_i64(),
            92_001_i64,
            message_created_at,
            deleted_user.id.as_i64(),
        )
        .execute(&pool)
        .await,
        "deleted user mention should be inserted",
    );
    insert_chat_event(
        &pool,
        &room.id,
        deleted_user.id,
        92_001,
        message_created_at,
        "deleted-user-chat-event",
        Utc::now(),
    )
    .await;
    insert_chat_event(
        &pool,
        &room.id,
        room_owner.id,
        92_001,
        message_created_at,
        "mentioned-deleted-user-chat-event",
        Utc::now(),
    )
    .await;
    ok(
        sqlx::query!(
            r#"
            INSERT INTO room_resource_events (
                event_id, scope_type, room_id, aggregate_type, aggregate_id,
                resource_type, resource_id, event_type, event_version,
                actor_user_id, payload, summary, occurred_at
            ) VALUES ('deleted-user-chat-pin-event', 1, $1, 'chat_message', $2,
                      'chat_pins', $2, 'chat_pin_pinned', 1, $3,
                      NULL, '{}'::JSONB, CURRENT_TIMESTAMP)
            "#,
            room.id.as_i64(),
            92_001_i64.to_string(),
            room_owner.id.as_i64(),
        )
        .execute(&pool)
        .await,
        "deleted user chat pin event should be inserted",
    );

    let deleted_at = Utc::now() - Duration::days(40);
    ok(
        sqlx::query!(
            "UPDATE users SET deleted_at = $2, deletion_source = 'account' WHERE id = $1",
            deleted_user.id.as_i64(),
            deleted_at,
        )
        .execute(&pool)
        .await,
        "user should be soft-deleted",
    );

    let service = CleanupService::new(
        pool.clone(),
        CleanupConfig {
            soft_delete_retention_days: 30,
            room_soft_delete_retention_days: 0,
            resource_soft_delete_retention_days: 0,
            expired_token_retention_days: 0,
            expired_credential_buffer_hours: 0,
            notification_retention_days: 0,
            notification_max_retention_days: 0,
            chat_max_messages_per_room: 0,
            room_resource_event_retention_seconds: 0,
            chat_message_event_retention_seconds: 0,
            playback_progress_retention_days: 0,
            unreferenced_file_retention_seconds: 0,
            realtime_outbox_sent_retention_days: 0,
            realtime_outbox_dead_retention_days: 0,
        },
        Arc::new(AlwaysLeader),
    );
    let result = service.run_all().await;
    assert_eq!(result.users_purged, 1);
    assert_eq!(
        ok(
            sqlx::query_scalar!(
                "SELECT COUNT(*) AS \"count!\" FROM chat_message_events WHERE event_id IN ('deleted-user-chat-event', 'mentioned-deleted-user-chat-event')"
            )
            .fetch_one(&pool)
            .await,
            "remaining chat event count should load",
        ),
        0
    );
    assert_eq!(
        ok(
            sqlx::query_scalar!(
                "SELECT COUNT(*) AS \"count!\" FROM room_resource_events WHERE event_id = 'deleted-user-chat-pin-event'"
            )
            .fetch_one(&pool)
            .await,
            "remaining room resource event count should load",
        ),
        0
    );
    let attachment_reference = ok(
        sqlx::query_scalar!(
            "SELECT expires_at FROM file_references WHERE reference_kind = 'chat_message_attachment' AND reference_id = $1",
            attachment_reference_id,
        )
        .fetch_optional(&pool)
        .await,
        "purged chat attachment reference should be queryable",
    );
    assert!(
        attachment_reference.flatten().is_some(),
        "hard-purged chat attachments must retain an expired reference for storage cleanup"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn resource_retention_purges_only_expired_user_deleted_rows() {
    let (_container, pool) = create_test_pool().await;
    let storage = Arc::new(RecordingFileStorageService::default());
    let user = create_test_user(&pool).await;
    let room = ok(
        RoomRepository::new(pool.clone())
            .create(&create_test_room(user.id, None))
            .await,
        "resource lifecycle room should be created",
    );
    let old_deleted_at = Utc::now() - Duration::days(40);
    let recent_deleted_at = Utc::now() - Duration::days(10);
    let recent_identity_user = create_test_user(&pool).await;
    let account_identity_user = create_test_user(&pool).await;
    ok(
        sqlx::query!(
            r#"INSERT INTO auth_email_identities (
                   user_id, email, deleted_at, deletion_source
               )
               VALUES
                   ($1, 'expired-unbound@example.com', $4, 'user'),
                   ($2, 'recent-unbound@example.com', $5, 'user'),
                   ($3, 'account-retained@example.com', $4, 'account')"#,
            user.id.as_i64(),
            recent_identity_user.id.as_i64(),
            account_identity_user.id.as_i64(),
            old_deleted_at,
            recent_deleted_at,
        )
        .execute(&pool)
        .await,
        "email identity lifecycle fixtures should be inserted",
    );
    ok(
        sqlx::query!(
            r#"INSERT INTO auth_oauth2_identities (
                   provider_type, provider_instance_name, provider_user_id,
                   user_id, username, deleted_at, deletion_source
               )
               VALUES
                   (2, 'github', 'expired-unbound', $1, 'expired', $2, 'user'),
                   (2, 'github', 'recent-unbound', $1, 'recent', $3, 'user'),
                   (2, 'github', 'account-retained', $1, 'account', $2, 'account')"#,
            user.id.as_i64(),
            old_deleted_at,
            recent_deleted_at,
        )
        .execute(&pool)
        .await,
        "OAuth2 identity lifecycle fixtures should be inserted",
    );

    let media_cover_reference_id = insert_file_reference(
        &pool,
        "resource-retention-media-cover",
        "media_cover",
        "resource-retention-media",
    )
    .await;
    let playlist_cover_reference_id = insert_file_reference(
        &pool,
        "resource-retention-playlist-cover",
        "playlist_cover",
        "resource-retention-playlist",
    )
    .await;

    let expired_parent_playlist_id = ok(
        sqlx::query_scalar!(
            r#"INSERT INTO playlists (
                   room_id, creator_id, name, cover_file_reference_id, position,
                   deleted_at, deletion_source
               )
               VALUES ($1, $2, 'expired-parent', $3, 1.0, $4, 'user')
               RETURNING id"#,
            room.id.as_i64(),
            user.id.as_i64(),
            playlist_cover_reference_id,
            old_deleted_at,
        )
        .fetch_one(&pool)
        .await,
        "expired parent playlist should be inserted",
    );
    let expired_child_playlist_id = ok(
        sqlx::query_scalar!(
            r#"INSERT INTO playlists (
                   room_id, creator_id, name, parent_id, position,
                   deleted_at, deletion_source
               )
               VALUES ($1, $2, 'expired-child', $3, 1.0, $4, 'user')
               RETURNING id"#,
            room.id.as_i64(),
            user.id.as_i64(),
            expired_parent_playlist_id,
            old_deleted_at,
        )
        .fetch_one(&pool)
        .await,
        "expired child playlist should be inserted",
    );
    let recent_playlist_id = ok(
        sqlx::query_scalar!(
            r#"INSERT INTO playlists (
                   room_id, creator_id, name, position, deleted_at, deletion_source
               )
               VALUES ($1, $2, 'recent-user-delete', 2.0, $3, 'user')
               RETURNING id"#,
            room.id.as_i64(),
            user.id.as_i64(),
            recent_deleted_at,
        )
        .fetch_one(&pool)
        .await,
        "recently deleted playlist should be inserted",
    );
    let account_playlist_id = ok(
        sqlx::query_scalar!(
            r#"INSERT INTO playlists (
                   room_id, creator_id, name, position, deleted_at,
                   deletion_source, deleted_owner_id
               )
               VALUES ($1, $2, 'account-delete', 3.0, $3, 'account', $2)
               RETURNING id"#,
            room.id.as_i64(),
            user.id.as_i64(),
            old_deleted_at,
        )
        .fetch_one(&pool)
        .await,
        "account-propagated playlist should be inserted",
    );

    let expired_media_id = ok(
        sqlx::query_scalar!(
            r#"INSERT INTO media (
                   playlist_id, room_id, creator_id, name, position,
                   source_provider, source_config, cover_file_reference_id,
                   deleted_at, deletion_source
               )
               VALUES ($1, $2, $3, 'expired-media', 1.0, 1, '{}'::JSONB, $4, $5, 'user')
               RETURNING id"#,
            expired_child_playlist_id,
            room.id.as_i64(),
            user.id.as_i64(),
            media_cover_reference_id,
            old_deleted_at,
        )
        .fetch_one(&pool)
        .await,
        "expired media should be inserted",
    );
    let recent_media_id = ok(
        sqlx::query_scalar!(
            r#"INSERT INTO media (
                   room_id, creator_id, name, position, source_provider, source_config,
                   deleted_at, deletion_source
               )
               VALUES ($1, $2, 'recent-media', 2.0, 1, '{}'::JSONB, $3, 'user')
               RETURNING id"#,
            room.id.as_i64(),
            user.id.as_i64(),
            recent_deleted_at,
        )
        .fetch_one(&pool)
        .await,
        "recently deleted media should be inserted",
    );
    let room_media_id = ok(
        sqlx::query_scalar!(
            r#"INSERT INTO media (
                   room_id, creator_id, name, position, source_provider, source_config,
                   deleted_at, deletion_source
               )
               VALUES ($1, $2, 'room-media', 3.0, 1, '{}'::JSONB, $3, 'room')
               RETURNING id"#,
            room.id.as_i64(),
            user.id.as_i64(),
            old_deleted_at,
        )
        .fetch_one(&pool)
        .await,
        "room-propagated media should be inserted",
    );

    let message_created_at = Utc::now() - Duration::days(100);
    insert_chat_message_with_image(
        &pool,
        room.id,
        user.id,
        91_001,
        message_created_at,
        "resource-retention-chat-image",
        "normalized/raw/resource-retention-chat.webp",
    )
    .await;
    insert_chat_text_message(&pool, room.id, user.id, 91_002, message_created_at).await;
    insert_chat_text_message(&pool, room.id, user.id, 91_003, message_created_at).await;
    for (message_id, deleted_at, source, deleted_owner_id) in [
        (91_001_i64, old_deleted_at, "user", None),
        (91_002_i64, recent_deleted_at, "user", None),
        (
            91_003_i64,
            old_deleted_at,
            "account",
            Some(user.id.as_i64()),
        ),
    ] {
        ok(
            sqlx::query!(
                r#"UPDATE chat_messages
                   SET deleted_at = $2, deletion_source = $3, deleted_owner_id = $4
                   WHERE id = $1 AND created_at = $5"#,
                message_id,
                deleted_at,
                source,
                deleted_owner_id,
                message_created_at,
            )
            .execute(&pool)
            .await,
            "chat message lifecycle metadata should be updated",
        );
    }

    let service = CleanupService::new_with_options(
        pool.clone(),
        CleanupConfig {
            soft_delete_retention_days: 0,
            room_soft_delete_retention_days: 0,
            resource_soft_delete_retention_days: 30,
            expired_token_retention_days: 0,
            expired_credential_buffer_hours: 0,
            notification_retention_days: 0,
            notification_max_retention_days: 0,
            chat_max_messages_per_room: 0,
            room_resource_event_retention_seconds: 0,
            chat_message_event_retention_seconds: 0,
            playback_progress_retention_days: 0,
            unreferenced_file_retention_seconds: 0,
            realtime_outbox_sent_retention_days: 0,
            realtime_outbox_dead_retention_days: 0,
        },
        Arc::new(AlwaysLeader),
        CleanupServiceOptions {
            file_storage_service: Some(storage.clone()),
            ..CleanupServiceOptions::default()
        },
    );

    let result = service.run_all().await;

    assert_eq!(result.media_purged, 1);
    assert_eq!(result.playlists_purged, 2);
    assert_eq!(result.chat_messages_purged, 1);
    assert_eq!(result.email_identities_purged, 1);
    assert_eq!(result.oauth2_identities_purged, 1);
    for (table, id) in [
        ("playlists", recent_playlist_id),
        ("playlists", account_playlist_id),
        ("media", recent_media_id),
        ("media", room_media_id),
    ] {
        let exists = ok(
            sqlx::query_scalar!(
                r#"SELECT CASE
                       WHEN $1 = 'media' THEN EXISTS(SELECT 1 FROM media WHERE id = $2)
                       ELSE EXISTS(SELECT 1 FROM playlists WHERE id = $2)
                   END AS "exists!""#,
                table,
                id,
            )
            .fetch_one(&pool)
            .await,
            "protected resource existence should be queryable",
        );
        assert!(exists, "{table} row {id} should remain recoverable");
    }
    assert!(!ok(
        sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM media WHERE id = $1) AS \"exists!\"",
            expired_media_id,
        )
        .fetch_one(&pool)
        .await,
        "expired media existence should be queryable",
    ));
    assert!(!ok(
        sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM playlists WHERE id = $1) AS \"exists!\"",
            expired_parent_playlist_id,
        )
        .fetch_one(&pool)
        .await,
        "expired playlist existence should be queryable",
    ));
    let remaining_message_ids = ok(
        sqlx::query_scalar!(
            r#"SELECT ARRAY_AGG(id ORDER BY id) AS "ids?: Vec<i64>"
               FROM chat_messages
               WHERE id = ANY($1)"#,
            &[91_001_i64, 91_002, 91_003],
        )
        .fetch_one(&pool)
        .await,
        "remaining lifecycle chat messages should be queryable",
    )
    .unwrap_or_default();
    assert_eq!(remaining_message_ids, vec![91_002, 91_003]);
    let remaining_email_identities = ok(
        sqlx::query_scalar!("SELECT email FROM auth_email_identities ORDER BY email",)
            .fetch_all(&pool)
            .await,
        "remaining email identities should be queryable",
    );
    assert_eq!(
        remaining_email_identities,
        vec![
            "account-retained@example.com".to_string(),
            "recent-unbound@example.com".to_string(),
        ]
    );
    let remaining_oauth2_subjects = ok(
        sqlx::query_scalar!(
            "SELECT provider_user_id FROM auth_oauth2_identities ORDER BY provider_user_id",
        )
        .fetch_all(&pool)
        .await,
        "remaining OAuth2 identities should be queryable",
    );
    assert_eq!(
        remaining_oauth2_subjects,
        vec!["account-retained".to_string(), "recent-unbound".to_string(),]
    );

    let expired_file_references = ok(
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!"
               FROM file_references
               WHERE id = ANY($1) AND expires_at IS NOT NULL"#,
            &[media_cover_reference_id, playlist_cover_reference_id],
        )
        .fetch_one(&pool)
        .await,
        "expired resource file references should be queryable",
    );
    assert_eq!(expired_file_references, 2);
    let deleted_object_keys = ok(
        storage.deleted_object_keys.lock(),
        "deleted object key recorder lock should be acquired",
    )
    .clone();
    assert!(deleted_object_keys.contains(&"resource-retention-media-cover".to_string()));
    assert!(deleted_object_keys.contains(&"resource-retention-playlist-cover".to_string()));
    assert!(
        deleted_object_keys.contains(&"normalized/raw/resource-retention-chat.webp".to_string())
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_message_cap_cleanup_deletes_image_objects() {
    let (_container, pool) = create_test_pool().await;
    let storage = Arc::new(RecordingFileStorageService::default());

    let user = create_test_user(&pool).await;
    let room = create_test_room(user.id, None);
    let room = ok(
        RoomRepository::new(pool.clone()).create(&room).await,
        "test room should be created",
    );
    let older_at = Utc::now() - Duration::minutes(10);
    let newer_at = Utc::now() - Duration::minutes(1);

    insert_chat_message_with_image(
        &pool,
        room.id,
        user.id,
        9_101,
        older_at,
        "cleanup-old-image",
        "normalized/raw/cleanup-old.webp",
    )
    .await;
    insert_chat_message_with_image(
        &pool,
        room.id,
        user.id,
        9_102,
        newer_at,
        "cleanup-kept-image",
        "normalized/raw/cleanup-kept.webp",
    )
    .await;

    let service = CleanupService::new_with_options(
        pool.clone(),
        CleanupConfig {
            soft_delete_retention_days: 0,
            room_soft_delete_retention_days: 0,
            resource_soft_delete_retention_days: 0,
            expired_token_retention_days: 0,
            expired_credential_buffer_hours: 0,
            notification_retention_days: 0,
            notification_max_retention_days: 0,
            chat_max_messages_per_room: 1,
            room_resource_event_retention_seconds: 0,
            chat_message_event_retention_seconds: 0,
            playback_progress_retention_days: 0,
            unreferenced_file_retention_seconds: 0,
            realtime_outbox_sent_retention_days: 0,
            realtime_outbox_dead_retention_days: 0,
        },
        Arc::new(AlwaysLeader),
        CleanupServiceOptions {
            file_storage_service: Some(storage.clone()),
            ..CleanupServiceOptions::default()
        },
    );

    let result = service.run_all().await;

    assert_eq!(result.chat_messages_deleted, 1);
    let deleted_object_keys = ok(
        storage.deleted_object_keys.lock(),
        "deleted object key recorder lock should be acquired",
    )
    .clone();
    assert_eq!(
        deleted_object_keys,
        vec!["normalized/raw/cleanup-old.webp".to_string()]
    );
    let deleted_origins = ok(
        storage.deleted_origins.lock(),
        "deleted origin recorder lock should be acquired",
    )
    .clone();
    assert_eq!(deleted_origins, vec!["reference_cap_exceeded".to_string()]);

    let old_exists = ok(
        sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE id = $1)",
            9_101_i64
        )
        .fetch_one(&pool)
        .await,
        "old message existence query should succeed",
    )
    .unwrap_or(false);
    let kept_exists = ok(
        sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE id = $1)",
            9_102_i64
        )
        .fetch_one(&pool)
        .await,
        "kept message existence query should succeed",
    )
    .unwrap_or(false);
    assert!(!old_exists);
    assert!(kept_exists);
}

/// Helper to create a test user in the database
async fn create_test_user(pool: &PgPool) -> User {
    let now = Utc::now();
    let user_id = UserId::new();
    let user = User {
        id: user_id,
        username: format!("test_user_{}", synctv_common::snanoid!(8)),
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
    };
    ok(
        UserRepository::new(pool.clone()).create(&user).await,
        "test user should be created",
    )
}

async fn insert_file_reference(
    pool: &PgPool,
    object_key: &str,
    reference_kind: &str,
    reference_id: &str,
) -> i64 {
    ok(
        sqlx::query_scalar!(
            r#"WITH object AS (
                   INSERT INTO file_objects (
                       storage_backend, object_key, mime_type, size_bytes,
                       content_manifest_sha256, metadata, validated_at
                   )
                   VALUES ('test-storage', $1, 'image/webp', 1,
                           repeat('c', 64), '{}'::JSONB, CURRENT_TIMESTAMP)
               )
               INSERT INTO file_references (
                   storage_backend, object_key, reference_kind, reference_id, metadata
               )
               VALUES ('test-storage', $1, $2, $3, '{}'::JSONB)
               RETURNING id"#,
            object_key,
            reference_kind,
            reference_id,
        )
        .fetch_one(pool)
        .await,
        "file reference fixture should be inserted",
    )
}

async fn insert_chat_message_with_image(
    pool: &PgPool,
    room_id: RoomId,
    user_id: UserId,
    message_id: i64,
    created_at: chrono::DateTime<Utc>,
    image_id: &str,
    object_key: &str,
) {
    ensure_chat_partition_for(pool, created_at).await;

    ok(
        sqlx::query!(
            r#"
        INSERT INTO chat_messages (
            id, room_id, user_id, client_message_id, content, message_type, status, version,
            reply_to_message_id, metadata, edited_at, deleted_at, deleted_by, delete_reason,
            created_at
        ) VALUES (
            $1, $2, $3, NULL, $4, $5, $6, $7,
            NULL, $8, NULL, NULL, NULL, NULL,
            $9
        )
        "#,
            message_id,
            room_id.as_i64(),
            user_id.as_i64(),
            "attachment message",
            4_i16,
            1_i16,
            1_i64,
            serde_json::Value::Object(Default::default()),
            created_at,
        )
        .execute(pool)
        .await,
        "chat message fixture should be inserted",
    );

    ok(
        sqlx::query!(
            r#"
        INSERT INTO chat_message_attachments (
            id, kind, room_id, message_id, message_created_at, filename, storage_backend, object_key, url,
            mime_type, size_bytes, width, height, metadata, created_at
        ) VALUES (
            $1, 2, $2, $3, $4, NULL, $5, $6, NULL, NULL, NULL, NULL, NULL, $7, $8
        )
        "#,
            image_id,
            room_id.as_i64(),
            message_id,
            created_at,
            "test-storage",
            object_key,
            serde_json::Value::Object(Default::default()),
            created_at,
        )
        .execute(pool)
        .await,
        "chat attachment fixture should be inserted",
    );
}

async fn insert_chat_text_message(
    pool: &PgPool,
    room_id: RoomId,
    user_id: UserId,
    message_id: i64,
    created_at: chrono::DateTime<Utc>,
) {
    ensure_chat_partition_for(pool, created_at).await;
    insert_chat_text_message_without_partition_setup(
        pool, room_id, user_id, message_id, created_at,
    )
    .await;
}

async fn insert_chat_text_message_without_partition_setup(
    pool: &PgPool,
    room_id: RoomId,
    user_id: UserId,
    message_id: i64,
    created_at: chrono::DateTime<Utc>,
) {
    ok(
        sqlx::query!(
            r#"
            INSERT INTO chat_messages (
                id, room_id, user_id, client_message_id, content, message_type, status, version,
                reply_to_message_id, metadata, edited_at, deleted_at, deleted_by, delete_reason,
                created_at
            ) VALUES (
                $1, $2, $3, NULL, $4, 1, 1, 1,
                NULL, '{}'::jsonb, NULL, NULL, NULL, NULL,
                $5
            )
            "#,
            message_id,
            room_id.as_i64(),
            user_id.as_i64(),
            "cleanup text message",
            created_at,
        )
        .execute(pool)
        .await,
        "chat text message fixture should be inserted",
    );
}

async fn insert_chat_event(
    pool: &PgPool,
    room_id: &RoomId,
    user_id: UserId,
    message_id: i64,
    message_created_at: chrono::DateTime<Utc>,
    event_id: &str,
    created_at: chrono::DateTime<Utc>,
) {
    ok(
        sqlx::query!(
            r#"
            INSERT INTO chat_message_events (
                event_id, room_id, actor_user_id, message_id, message_created_at,
                event_type, event_version, message_version, payload, summary, occurred_at, created_at
            ) VALUES (
                $1, $2, $3, $4, $5,
                'chat_message_created', 1, 1, '{}'::jsonb, '{}'::jsonb, $6, $7
            )
            "#,
            event_id,
            room_id.as_i64(),
            user_id.as_i64(),
            message_id,
            message_created_at,
            created_at,
            created_at,
        )
        .execute(pool)
        .await,
        "chat event fixture should be inserted",
    );
}

/// Helper to create a test room with optional custom timestamps
fn create_test_room(created_by: UserId, updated_at: Option<chrono::DateTime<Utc>>) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: "Test Room".to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        category: None,
        labels: Vec::new(),
        created_by,
        status: RoomStatus::Active,
        is_banned: false,
        closed_at: None,
        created_at: now,
        updated_at: updated_at.unwrap_or(now),
        deleted_at: None,
        version: 0,
        last_activity_at: updated_at.unwrap_or(now),
    }
}
