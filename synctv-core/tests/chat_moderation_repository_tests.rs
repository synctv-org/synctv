//! Chat moderation job repository integration tests.
//!
//! These tests require Docker-backed PostgreSQL and are ignored by default,
//! matching the other database integration tests in this package.

use chrono::Utc;
use synctv_core::{
    models::{ChatMessage, ChatMessageStatus, DeletionSource, Room, SignupMethod, User},
    repository::{
        ChatModerationJobPhase, ChatModerationJobRepository, ChatModerationJobStatus,
        ChatModerationProgress, ChatRepository, DeleteChatMessageEventRequest,
        DeleteChatReactionsPageRequest, NewChatModerationJob, RoomRepository, UserRepository,
    },
};
use synctv_core_testing::create_test_pool;

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn moderation_job_releases_each_incomplete_page_and_keeps_actor_snapshot() {
    let (_container, pool) = create_test_pool().await;
    let user_repository = UserRepository::new(pool.clone());
    let room_repository = RoomRepository::new(pool.clone());
    let actor = user_repository
        .create(&User::new(
            "moderation_actor".to_string(),
            SignupMethod::Email,
        ))
        .await
        .expect("actor should be created");
    let room = room_repository
        .create(&Room::new("moderation_room".to_string(), actor.id))
        .await
        .expect("room should be created");

    let repository = ChatModerationJobRepository::new(pool);
    let job = repository
        .insert(&NewChatModerationJob {
            id: "moderation-state-machine".to_string(),
            room_id: room.id,
            target_user_id: actor.id,
            actor_user_id: actor.id,
            actor_username: "actor-before-account-removal".to_string(),
            actor_role: synctv_core::models::UserRole::Admin,
            message_id: Some(42),
            ban_user: true,
            delete_all_messages: true,
            delete_all_reactions: true,
            reason: Some("test".to_string()),
            snapshot_at: Utc::now(),
        })
        .await
        .expect("job should be inserted");
    assert_eq!(job.actor_username, "actor-before-account-removal");
    assert_eq!(job.message_id, Some(42));
    assert!(job.ban_user);

    let claimed = repository
        .claim_batch("worker-1", 1)
        .await
        .expect("job should be claimed");
    assert_eq!(claimed.len(), 1);
    let claimed = &claimed[0];
    assert_eq!(claimed.status, ChatModerationJobStatus::Processing);

    let cursor = Some((Utc::now(), 1_i64));
    let mut progress = claimed.clone();
    progress.message_cursor = cursor;
    progress.deleted_messages = 100;
    progress.deleted_reactions = 100;
    assert!(repository
        .update_progress(&progress, "worker-1")
        .await
        .expect("incomplete progress should be persisted"));

    let pending = repository
        .get("moderation-state-machine")
        .await
        .expect("job should be readable")
        .expect("job should still exist");
    assert_eq!(pending.status, ChatModerationJobStatus::Pending);
    assert_eq!(pending.actor_username, "actor-before-account-removal");

    let reclaimed = repository
        .claim_batch("worker-2", 1)
        .await
        .expect("next worker should claim the next page");
    assert_eq!(reclaimed.len(), 1);
    let reclaimed = &reclaimed[0];
    assert_eq!(
        reclaimed.message_cursor.map(|(_, id)| id),
        cursor.map(|(_, id)| id)
    );
    assert_eq!(
        reclaimed
            .message_cursor
            .map(|(at, _)| at.timestamp_micros()),
        cursor.map(|(at, _)| at.timestamp_micros())
    );

    let mut completed_progress = reclaimed.clone();
    completed_progress.phase = ChatModerationJobPhase::Done;
    completed_progress.snapshot_at = reclaimed.snapshot_at + chrono::Duration::seconds(1);
    completed_progress.explicit_message_done = true;
    completed_progress.ban_done = true;
    assert!(repository
        .update_progress(&completed_progress, "worker-2")
        .await
        .expect("completion progress should be persisted"));
    let persisted = repository
        .get("moderation-state-machine")
        .await
        .expect("job should be readable")
        .expect("job should exist");
    assert_eq!(persisted.snapshot_at, completed_progress.snapshot_at);
    completed_progress.lock_version += 1;
    assert!(repository
        .mark_completed(&completed_progress, "worker-2", 100, 100)
        .await
        .expect("job should complete"));
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn moderation_delete_persists_job_counts_in_the_same_transaction() {
    let (_container, pool) = create_test_pool().await;
    let user_repository = UserRepository::new(pool.clone());
    let room_repository = RoomRepository::new(pool.clone());
    let actor = user_repository
        .create(&User::new(
            "moderation_atomic_actor".to_string(),
            SignupMethod::Email,
        ))
        .await
        .expect("actor should be created");
    let room = room_repository
        .create(&Room::new("moderation_atomic_room".to_string(), actor.id))
        .await
        .expect("room should be created");
    let chat_repository = ChatRepository::new(pool.clone());
    let message = chat_repository
        .create(&ChatMessage::new(
            room.id,
            actor.id,
            "message to moderate".to_string(),
        ))
        .await
        .expect("message should be created");
    sqlx::query!(
        r#"
        INSERT INTO chat_message_reactions (
            room_id, message_id, message_created_at, user_id, reaction_key
        )
        VALUES ($1, $2, $3, $4, $5)
        "#,
        room.id.as_i64(),
        message.id,
        message.created_at,
        actor.id.as_i64(),
        "heart",
    )
    .execute(&pool)
    .await
    .expect("reaction should be created");

    let job_repository = ChatModerationJobRepository::new(pool.clone());
    job_repository
        .insert(&NewChatModerationJob {
            id: "moderation-atomic-delete".to_string(),
            room_id: room.id,
            target_user_id: actor.id,
            actor_user_id: actor.id,
            actor_username: actor.username.clone(),
            actor_role: synctv_core::models::UserRole::Admin,
            message_id: None,
            ban_user: false,
            delete_all_messages: true,
            delete_all_reactions: false,
            reason: Some("test".to_string()),
            snapshot_at: Utc::now(),
        })
        .await
        .expect("job should be inserted");
    let claimed = job_repository
        .claim_batch("worker-atomic", 1)
        .await
        .expect("job should be claimed")
        .pop()
        .expect("one job should be claimed");

    let progress = ChatModerationProgress {
        job_id: &claimed.id,
        worker_id: "worker-atomic",
        lock_version: claimed.lock_version,
    };
    let outcome = chat_repository
        .soft_delete_with_event(DeleteChatMessageEventRequest {
            room_id: &room.id,
            message_id: message.id,
            message_created_at: message.created_at,
            deleted_by: &actor.id,
            reason: Some("test"),
            expected_version: None,
            event_id: "moderation-atomic-event",
            occurred_at: Utc::now(),
            operation: None,
            reaction_user_id: Some(&actor.id),
            moderation_progress: Some(progress),
            deletion_source: DeletionSource::Admin,
        })
        .await
        .expect("message deletion should commit")
        .expect("message should be inserted as deleted");
    assert_eq!(outcome.deleted_reactions, 1);

    let stored_job = job_repository
        .get(&claimed.id)
        .await
        .expect("job should be readable")
        .expect("job should exist");
    assert_eq!(stored_job.deleted_messages, 1);
    assert_eq!(stored_job.deleted_reactions, 1);
    let reaction_count = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "count!"
        FROM chat_message_reactions
        WHERE room_id = $1 AND message_id = $2 AND message_created_at = $3
        "#,
        room.id.as_i64(),
        message.id,
        message.created_at,
    )
    .fetch_one(&pool)
    .await
    .expect("reaction count should be readable");
    assert_eq!(reaction_count, 0);
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn moderation_delete_rolls_back_when_worker_lease_is_invalid() {
    let (_container, pool) = create_test_pool().await;
    let user_repository = UserRepository::new(pool.clone());
    let room_repository = RoomRepository::new(pool.clone());
    let actor = user_repository
        .create(&User::new(
            "moderation_lease_actor".to_string(),
            SignupMethod::Email,
        ))
        .await
        .expect("actor should be created");
    let room = room_repository
        .create(&Room::new("moderation_lease_room".to_string(), actor.id))
        .await
        .expect("room should be created");
    let chat_repository = ChatRepository::new(pool.clone());
    let message = chat_repository
        .create(&ChatMessage::new(
            room.id,
            actor.id,
            "message with expired lease".to_string(),
        ))
        .await
        .expect("message should be created");
    sqlx::query!(
        r#"
        INSERT INTO chat_message_reactions (
            room_id, message_id, message_created_at, user_id, reaction_key
        )
        VALUES ($1, $2, $3, $4, $5)
        "#,
        room.id.as_i64(),
        message.id,
        message.created_at,
        actor.id.as_i64(),
        "heart",
    )
    .execute(&pool)
    .await
    .expect("reaction should be created");

    let job_repository = ChatModerationJobRepository::new(pool.clone());
    job_repository
        .insert(&NewChatModerationJob {
            id: "moderation-expired-lease".to_string(),
            room_id: room.id,
            target_user_id: actor.id,
            actor_user_id: actor.id,
            actor_username: actor.username.clone(),
            actor_role: synctv_core::models::UserRole::Admin,
            message_id: None,
            ban_user: false,
            delete_all_messages: true,
            delete_all_reactions: false,
            reason: Some("test".to_string()),
            snapshot_at: Utc::now(),
        })
        .await
        .expect("job should be inserted");
    let claimed = job_repository
        .claim_batch("worker-expired", 1)
        .await
        .expect("job should be claimed")
        .pop()
        .expect("one job should be claimed");
    sqlx::query!(
        r#"
        UPDATE chat_moderation_jobs
        SET locked_at = NOW() - INTERVAL '1 hour'
        WHERE id = $1
        "#,
        &claimed.id,
    )
    .execute(&pool)
    .await
    .expect("lease should be expired");
    assert_eq!(
        job_repository
            .requeue_stale_processing(1)
            .await
            .expect("stale job should be requeued"),
        1
    );

    let Err(error) = chat_repository
        .soft_delete_with_event(DeleteChatMessageEventRequest {
            room_id: &room.id,
            message_id: message.id,
            message_created_at: message.created_at,
            deleted_by: &actor.id,
            reason: Some("test"),
            expected_version: None,
            event_id: "moderation-expired-event",
            occurred_at: Utc::now(),
            operation: None,
            reaction_user_id: Some(&actor.id),
            moderation_progress: Some(ChatModerationProgress {
                job_id: &claimed.id,
                worker_id: "worker-expired",
                lock_version: claimed.lock_version,
            }),
            deletion_source: DeletionSource::Admin,
        })
        .await
    else {
        panic!("expired worker must not delete chat data");
    };
    assert!(matches!(error, synctv_core::Error::LockConflict(_)));

    let message_status = sqlx::query_scalar!(
        r#"
        SELECT status AS "status!"
        FROM chat_messages
        WHERE room_id = $1 AND id = $2 AND created_at = $3
        "#,
        room.id.as_i64(),
        message.id,
        message.created_at,
    )
    .fetch_one(&pool)
    .await
    .expect("message status should be readable");
    assert_eq!(message_status, i16::from(ChatMessageStatus::Active));
    let reaction_count = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "count!"
        FROM chat_message_reactions
        WHERE room_id = $1 AND message_id = $2 AND message_created_at = $3
        "#,
        room.id.as_i64(),
        message.id,
        message.created_at,
    )
    .fetch_one(&pool)
    .await
    .expect("reaction count should be readable");
    assert_eq!(reaction_count, 1);
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn reaction_cleanup_preserves_reactions_created_after_the_snapshot() {
    let (_container, pool) = create_test_pool().await;
    let user_repository = UserRepository::new(pool.clone());
    let room_repository = RoomRepository::new(pool.clone());
    let user = user_repository
        .create(&User::new(
            "reaction_snapshot_user".to_string(),
            SignupMethod::Email,
        ))
        .await
        .expect("user should be created");
    let room = room_repository
        .create(&Room::new("reaction_snapshot_room".to_string(), user.id))
        .await
        .expect("room should be created");
    let chat_repository = ChatRepository::new(pool.clone());
    let message = chat_repository
        .create(&ChatMessage::new(
            room.id,
            user.id,
            "reaction snapshot message".to_string(),
        ))
        .await
        .expect("message should be created");
    let snapshot_at = Utc::now();
    sqlx::query!(
        r#"
        INSERT INTO chat_message_reactions (
            room_id, message_id, message_created_at, user_id, reaction_key, created_at
        )
        VALUES ($1, $2, $3, $4, $5, $6),
               ($1, $2, $3, $4, $7, $8)
        "#,
        room.id.as_i64(),
        message.id,
        message.created_at,
        user.id.as_i64(),
        "heart",
        snapshot_at - chrono::Duration::seconds(1),
        "fire",
        snapshot_at + chrono::Duration::seconds(1),
    )
    .execute(&pool)
    .await
    .expect("reactions should be created");

    let (deleted, _, _, _) = chat_repository
        .delete_reactions_by_user_with_events_page(DeleteChatReactionsPageRequest {
            room_id: &room.id,
            user_id: &user.id,
            actor_user_id: &user.id,
            occurred_at: Utc::now(),
            created_before: snapshot_at,
            cursor: None,
            limit: 100,
            moderation_progress: None,
        })
        .await
        .expect("reaction cleanup should succeed");
    assert_eq!(deleted, 1);
    let remaining = sqlx::query_scalar!(
        r#"
        SELECT reaction_key
        FROM chat_message_reactions
        WHERE room_id = $1
          AND message_id = $2
          AND message_created_at = $3
          AND user_id = $4
        "#,
        room.id.as_i64(),
        message.id,
        message.created_at,
        user.id.as_i64(),
    )
    .fetch_all(&pool)
    .await
    .expect("remaining reactions should be readable");
    assert_eq!(remaining, vec!["fire".to_string()]);
}

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn terminal_job_cleanup_preserves_recent_jobs() {
    let (_container, pool) = create_test_pool().await;
    let user_repository = UserRepository::new(pool.clone());
    let room_repository = RoomRepository::new(pool.clone());
    let actor = user_repository
        .create(&User::new(
            "moderation_cleanup_actor".to_string(),
            SignupMethod::Email,
        ))
        .await
        .expect("actor should be created");
    let room = room_repository
        .create(&Room::new("moderation_cleanup_room".to_string(), actor.id))
        .await
        .expect("room should be created");
    let repository = ChatModerationJobRepository::new(pool.clone());
    for id in ["expired-terminal-job", "recent-terminal-job"] {
        repository
            .insert(&NewChatModerationJob {
                id: id.to_string(),
                room_id: room.id,
                target_user_id: actor.id,
                actor_user_id: actor.id,
                actor_username: actor.username.clone(),
                actor_role: synctv_core::models::UserRole::Admin,
                message_id: None,
                ban_user: false,
                delete_all_messages: true,
                delete_all_reactions: false,
                reason: Some("test".to_string()),
                snapshot_at: Utc::now(),
            })
            .await
            .expect("job should be inserted");
    }
    sqlx::query!(
        r#"
        UPDATE chat_moderation_jobs
        SET status = 3,
            phase = 3,
            updated_at = CASE
                WHEN id = 'expired-terminal-job' THEN NOW() - INTERVAL '2 hours'
                ELSE NOW()
            END,
            completed_at = NOW()
        WHERE id IN ('expired-terminal-job', 'recent-terminal-job')
        "#,
    )
    .execute(&pool)
    .await
    .expect("jobs should be completed");

    assert_eq!(
        repository
            .delete_terminal_before(60 * 60)
            .await
            .expect("expired jobs should be deleted"),
        1
    );
    assert!(repository
        .get("expired-terminal-job")
        .await
        .expect("expired job lookup should succeed")
        .is_none());
    assert!(repository
        .get("recent-terminal-job")
        .await
        .expect("recent job lookup should succeed")
        .is_some());
}
