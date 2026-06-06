//! Audit service tests
//!
//! Tests audit service buffering, unbuffered writes, and query logic.
//!
//! Run Docker tests: cargo test --test `audit_service_tests` -- --ignored
#![allow(clippy::unwrap_used)]

use synctv_core::models::{AuditAction, AuditTargetType};
use synctv_core::service::{AuditEventParams, AuditService, StreamKickAuditRequest};
use synctv_core_testing::create_test_pool;

// Unbuffered write tests (require Docker)

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_unbuffered_writes_immediately() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    // Write an audit event
    service
        .log(AuditEventParams {
            actor_id: "100001".to_string(),
            actor_username: "admin".to_string(),
            action: AuditAction::UserCreated,
            target_type: AuditTargetType::User,
            target_id: Some("target_user_1".to_string()),
            details: serde_json::json!({"reason": "test"}),
            ip_address: Some("127.0.0.1".to_string()),
            user_agent: Some("TestAgent/1.0".to_string()),
        })
        .await
        .expect("Unbuffered write should succeed");

    // Query immediately -- should be visible since unbuffered writes go directly to DB
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM audit_logs WHERE actor_id = '100001'")
        .fetch_one(&pool)
        .await
        .expect("Query should succeed");

    assert_eq!(row.0, 1, "Unbuffered write should be immediately visible");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_query_filter_by_action() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    // Write events with different actions
    service
        .log(AuditEventParams {
            actor_id: "100002".to_string(),
            actor_username: "admin".to_string(),
            action: AuditAction::UserBanned,
            target_type: AuditTargetType::User,
            target_id: Some("user_1".to_string()),
            details: serde_json::json!({}),
            ip_address: None,
            user_agent: None,
        })
        .await
        .unwrap();

    service
        .log(AuditEventParams {
            actor_id: "100002".to_string(),
            actor_username: "admin".to_string(),
            action: AuditAction::RoomCreated,
            target_type: AuditTargetType::Room,
            target_id: Some("room_1".to_string()),
            details: serde_json::json!({}),
            ip_address: None,
            user_agent: None,
        })
        .await
        .unwrap();

    service
        .log(AuditEventParams {
            actor_id: "100002".to_string(),
            actor_username: "admin".to_string(),
            action: AuditAction::UserBanned,
            target_type: AuditTargetType::User,
            target_id: Some("user_2".to_string()),
            details: serde_json::json!({}),
            ip_address: None,
            user_agent: None,
        })
        .await
        .unwrap();

    // Query filtered by action
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT target_id FROM audit_logs WHERE actor_id = '100002' AND action = $1 ORDER BY created_at",
    )
    .bind(i16::from(AuditAction::UserBanned))
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(rows.len(), 2, "Should find 2 user_banned events");
    assert_eq!(rows[0].0, "user_1");
    assert_eq!(rows[1].0, "user_2");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_query_date_range() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    // Write an event
    service
        .log(AuditEventParams {
            actor_id: "100003".to_string(),
            actor_username: "admin".to_string(),
            action: AuditAction::SettingsUpdated,
            target_type: AuditTargetType::Settings,
            target_id: None,
            details: serde_json::json!({"key": "value"}),
            ip_address: None,
            user_agent: None,
        })
        .await
        .unwrap();

    // Query with date range that includes now
    let now = chrono::Utc::now();
    let one_hour_ago = now - chrono::Duration::hours(1);
    let one_hour_later = now + chrono::Duration::hours(1);

    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT COUNT(*) FROM audit_logs WHERE actor_id = '100003' AND created_at >= $1 AND created_at <= $2",
    )
    .bind(one_hour_ago)
    .bind(one_hour_later)
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(rows[0].0, 1, "Event should be within the date range");

    // Query with past date range
    let two_hours_ago = now - chrono::Duration::hours(2);
    let one_and_half_hours_ago = now - chrono::Duration::minutes(90);

    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT COUNT(*) FROM audit_logs WHERE actor_id = '100003' AND created_at >= $1 AND created_at <= $2",
    )
    .bind(two_hours_ago)
    .bind(one_and_half_hours_ago)
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(rows[0].0, 0, "Event should not be in the past date range");
}

// Buffered service tests (no DB needed)

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_buffered_service_enqueues_without_error() {
    let pool = sqlx::PgPool::connect_lazy("postgresql://fake").unwrap();
    let (service, _handle) = AuditService::new(pool);

    // Events should be buffered without error even with a fake pool
    let result = service
        .log(AuditEventParams {
            actor_id: "actor".to_string(),
            actor_username: "admin".to_string(),
            action: AuditAction::UserCreated,
            target_type: AuditTargetType::User,
            target_id: Some("user1".to_string()),
            details: serde_json::json!({}),
            ip_address: None,
            user_agent: None,
        })
        .await;

    assert!(result.is_ok());
    assert_eq!(service.dropped_count(), 0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_unbuffered_service_dropped_count_zero() {
    let pool = sqlx::PgPool::connect_lazy("postgresql://fake").unwrap();
    let service = AuditService::new_unbuffered(pool);
    assert_eq!(service.dropped_count(), 0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_log_stream_kicked_writes_audit_log() {
    #[derive(sqlx::FromRow)]
    struct StreamKickAuditRow {
        action: i16,
        target_type: Option<i16>,
        target_id: Option<String>,
        ip_address: Option<String>,
        user_agent: Option<String>,
        details: Option<String>,
    }

    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    // Log a stream kick event
    service
        .log_stream_kicked(StreamKickAuditRequest {
            actor_id: "100004".to_string(),
            actor_username: "superadmin".to_string(),
            room_id: "room_abc123".to_string(),
            media_id: "media_xyz789".to_string(),
            reason: Some("Inappropriate content".to_string()),
            ip_address: Some("192.168.1.100".to_string()),
            user_agent: Some("Mozilla/5.0 AdminPanel/1.0".to_string()),
        })
        .await
        .expect("log_stream_kicked should succeed");

    let row: StreamKickAuditRow = sqlx::query_as(
        r"
        SELECT action, target_type, target_id, ip_address, user_agent, details::text
        FROM audit_logs
        WHERE actor_id = '100004' AND action = $1
        ",
    )
    .bind(i16::from(AuditAction::StreamKicked))
    .fetch_one(&pool)
    .await
    .expect("Query should succeed");

    assert_eq!(
        AuditAction::try_from(row.action).unwrap(),
        AuditAction::StreamKicked,
        "Action should be stream_kicked"
    );
    assert_eq!(
        row.target_type
            .map(|value| AuditTargetType::try_from(value).unwrap()),
        Some(AuditTargetType::Stream),
        "Target type should be stream"
    );
    assert_eq!(
        row.target_id,
        Some("room_abc123:media_xyz789".to_string()),
        "Target ID should be room_id:media_id"
    );
    assert_eq!(
        row.ip_address,
        Some("192.168.1.100".to_string()),
        "IP address should be recorded"
    );
    assert_eq!(
        row.user_agent,
        Some("Mozilla/5.0 AdminPanel/1.0".to_string()),
        "User-Agent should be recorded"
    );

    // Verify details JSON contains room_id, media_id, and reason
    let details: serde_json::Value =
        serde_json::from_str(&row.details.unwrap_or_default()).unwrap();
    assert_eq!(details["room_id"], "room_abc123");
    assert_eq!(details["media_id"], "media_xyz789");
    assert_eq!(details["reason"], "Inappropriate content");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_log_stream_kicked_without_reason() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    // Log a stream kick event without a reason
    service
        .log_stream_kicked(StreamKickAuditRequest {
                    actor_id: "100005".to_string(),
                    actor_username: "moderator".to_string(),
                    room_id: "room_def456".to_string(),
                    media_id: "media_uvw321".to_string(),
                    reason: None,
                    ip_address: // No reason provided
            None,
                    user_agent: None,
                })
        .await
        .expect("log_stream_kicked should succeed");

    // Verify the audit log was written correctly
    let row: (i16, Option<String>) = sqlx::query_as(
        r"
        SELECT action, details::text
        FROM audit_logs
        WHERE actor_id = '100005' AND action = $1
        ",
    )
    .bind(i16::from(AuditAction::StreamKicked))
    .fetch_one(&pool)
    .await
    .expect("Query should succeed");

    assert_eq!(
        AuditAction::try_from(row.0).unwrap(),
        AuditAction::StreamKicked,
        "Action should be stream_kicked"
    );

    // Verify details JSON contains empty reason
    let details: serde_json::Value = serde_json::from_str(&row.1.unwrap_or_default()).unwrap();
    assert_eq!(details["room_id"], "room_def456");
    assert_eq!(details["media_id"], "media_uvw321");
    assert_eq!(
        details["reason"], "",
        "Reason should be empty string when None is provided"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_log_stream_kicked_records_actor_username() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    // Log a stream kick event
    service
        .log_stream_kicked(StreamKickAuditRequest {
            actor_id: "100006".to_string(),
            actor_username: "test_admin_user".to_string(),
            room_id: "room_test".to_string(),
            media_id: "media_test".to_string(),
            reason: Some("Test reason".to_string()),
            ip_address: None,
            user_agent: None,
        })
        .await
        .expect("log_stream_kicked should succeed");

    // Verify actor_username was recorded
    let row: (String,) = sqlx::query_as(
        "SELECT actor_username FROM audit_logs WHERE actor_id = '100006' AND action = $1",
    )
    .bind(i16::from(AuditAction::StreamKicked))
    .fetch_one(&pool)
    .await
    .expect("Query should succeed");

    assert_eq!(
        row.0, "test_admin_user",
        "Actor username should be recorded"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_log_stream_kicked_multiple_kicks_are_logged_separately() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    // Log multiple stream kick events
    service
        .log_stream_kicked(StreamKickAuditRequest {
            actor_id: "100007".to_string(),
            actor_username: "admin".to_string(),
            room_id: "room_1".to_string(),
            media_id: "media_1".to_string(),
            reason: Some("Reason 1".to_string()),
            ip_address: None,
            user_agent: None,
        })
        .await
        .expect("First log_stream_kicked should succeed");

    service
        .log_stream_kicked(StreamKickAuditRequest {
            actor_id: "100007".to_string(),
            actor_username: "admin".to_string(),
            room_id: "room_2".to_string(),
            media_id: "media_2".to_string(),
            reason: Some("Reason 2".to_string()),
            ip_address: None,
            user_agent: None,
        })
        .await
        .expect("Second log_stream_kicked should succeed");

    // Verify both events were logged
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM audit_logs WHERE actor_id = '100007' AND action = $1")
            .bind(i16::from(AuditAction::StreamKicked))
            .fetch_one(&pool)
            .await
            .expect("Query should succeed");

    assert_eq!(count.0, 2, "Both stream kick events should be logged");

    // Verify they have different target_ids
    let targets: Vec<(String,)> = sqlx::query_as(
        "SELECT target_id FROM audit_logs WHERE actor_id = '100007' AND action = $1 ORDER BY target_id",
    )
    .bind(i16::from(AuditAction::StreamKicked))
    .fetch_all(&pool)
    .await
    .expect("Query should succeed");

    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0].0, "room_1:media_1");
    assert_eq!(targets[1].0, "room_2:media_2");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_buffer_full_increments_dropped_count() {
    // The fake pool means the background flush task will fail to write to DB,
    // and when the buffer is full, try_send will fail and the fallback sync write
    // also fails (fake pool), so dropped_count should be incremented.
    let pool = sqlx::PgPool::connect_lazy("postgresql://fake").unwrap();
    let (service, _handle) = AuditService::new_with_capacity(pool, 2);

    // The background task starts consuming from the channel, but because the pool
    // is fake, the flush will fail. We need to fill the channel faster than it drains.
    // With capacity 2, we need to send enough events to overflow.
    // The flush task accumulates events every 5 seconds or 100 events, so the
    // channel should back up quickly.

    let mut error_count = 0;
    for i in 0..100 {
        let result = service
            .log(AuditEventParams {
                actor_id: format!("actor_{i}"),
                actor_username: "admin".to_string(),
                action: AuditAction::UserCreated,
                target_type: AuditTargetType::User,
                target_id: Some(format!("user_{i}")),
                details: serde_json::json!({}),
                ip_address: None,
                user_agent: None,
            })
            .await;

        // Once buffer is full, the fallback sync write to fake pool will fail,
        // incrementing dropped_count
        if result.is_ok() {
            // Event was either buffered or the sync fallback succeeded (unlikely with fake pool)
        } else {
            error_count += 1;
        }
    }

    // Give the background task a moment to process (and fail)
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Either events were dropped due to buffer overflow + sync fallback failure,
    // or the background task processed some but failed to flush to fake DB
    let dropped = service.dropped_count();

    // With capacity 2 and 100 rapid sends, we expect at least some drops
    // The exact number depends on timing, but should be > 0
    assert!(
        dropped > 0 || error_count > 0,
        "With capacity 2 and 100 events, should have dropped events or errors. dropped={dropped}, errors={error_count}"
    );
}
