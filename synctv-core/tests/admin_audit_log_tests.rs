//! Admin audit log tests
//!
//! Tests audit log functionality for admin operations including:
//! - Audit log integrity
//! - Buffer-full fallback behavior
//! - Graceful degradation when database fails
//! - Async write verification
//!
//! Run with: cargo test -p synctv-core --test `admin_audit_log_tests` -- --nocapture
//! Docker tests: cargo test -p synctv-core --test `admin_audit_log_tests` -- --ignored --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use synctv_core::models::{AuditAction, AuditTargetType};
use synctv_core::service::AuditService;
use synctv_core_testing::create_test_pool;
// Test Infrastructure

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_log_integrity_all_fields() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    // Log a complete audit event
    service
        .log(
            "100101".to_string(),
            "test_admin".to_string(),
            AuditAction::UserBanned,
            AuditTargetType::User,
            Some("target_user_002".to_string()),
            serde_json::json!({
                "reason": "Policy violation",
                "duration": "permanent"
            }),
            Some("192.168.1.100".to_string()),
            Some("Mozilla/5.0 TestAgent/1.0".to_string()),
        )
        .await
        .expect("Log should succeed");

    // Verify all fields are stored correctly
    #[allow(clippy::type_complexity)]
    let row: (
        i64,
        i64,
        String,
        i16,
        Option<i16>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        r"
        SELECT id, actor_id, actor_username, action, target_type, target_id, ip_address, user_agent
        FROM audit_logs
        WHERE actor_id = 100101
        ",
    )
    .fetch_one(&pool)
    .await
    .expect("Query should succeed");

    assert!(row.0 > 0, "ID should be generated");
    assert_eq!(row.1, 100_101, "Actor ID should match");
    assert_eq!(row.2, "test_admin", "Actor username should match");
    assert_eq!(
        AuditAction::try_from(row.3).unwrap(),
        AuditAction::UserBanned,
        "Action should match"
    );
    assert_eq!(
        row.4.map(|value| AuditTargetType::try_from(value).unwrap()),
        Some(AuditTargetType::User),
        "Target type should match"
    );
    assert_eq!(
        row.5,
        Some("target_user_002".to_string()),
        "Target ID should match"
    );
    assert_eq!(
        row.6,
        Some("192.168.1.100".to_string()),
        "IP address should match"
    );
    assert_eq!(
        row.7,
        Some("Mozilla/5.0 TestAgent/1.0".to_string()),
        "User agent should match"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_log_details_json_integrity() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    // Log with complex JSON details
    let details = serde_json::json!({
        "nested": {
            "deeply": {
                "value": 42
            }
        },
        "array": [1, 2, 3],
        "string": "test",
        "boolean": true,
        "null": null
    });

    service
        .log(
            "100102".to_string(),
            "json_tester".to_string(),
            AuditAction::SettingsUpdated,
            AuditTargetType::Settings,
            None,
            details.clone(),
            None,
            None,
        )
        .await
        .expect("Log should succeed");

    // Verify JSON is stored and retrieved correctly
    let row: (serde_json::Value,) =
        sqlx::query_as("SELECT details FROM audit_logs WHERE actor_id = '100102'")
            .fetch_one(&pool)
            .await
            .expect("Query should succeed");

    assert_eq!(row.0["nested"]["deeply"]["value"], 42);
    assert_eq!(row.0["array"], serde_json::json!([1, 2, 3]));
    assert_eq!(row.0["string"], "test");
    assert_eq!(row.0["boolean"], true);
    assert!(row.0["null"].is_null());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_log_created_at_timestamp() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    let before = chrono::Utc::now();

    service
        .log(
            "100103".to_string(),
            "time_tester".to_string(),
            AuditAction::UserCreated,
            AuditTargetType::User,
            Some("new_user".to_string()),
            serde_json::json!({}),
            None,
            None,
        )
        .await
        .expect("Log should succeed");

    let after = chrono::Utc::now();

    // Verify timestamp is within expected range
    let row: (chrono::DateTime<chrono::Utc>,) =
        sqlx::query_as("SELECT created_at FROM audit_logs WHERE actor_id = '100103'")
            .fetch_one(&pool)
            .await
            .expect("Query should succeed");

    assert!(row.0 >= before, "created_at should be >= before");
    assert!(row.0 <= after, "created_at should be <= after");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_log_multiple_actions_same_actor() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    // Log multiple actions by same actor
    let actions = vec![
        AuditAction::UserCreated,
        AuditAction::UserBanned,
        AuditAction::RoomCreated,
        AuditAction::RoomBanned,
        AuditAction::SettingsUpdated,
    ];

    for action in &actions {
        service
            .log(
                "100104".to_string(),
                "multi_tester".to_string(),
                *action,
                match action {
                    AuditAction::UserCreated | AuditAction::UserBanned => AuditTargetType::User,
                    AuditAction::RoomCreated | AuditAction::RoomBanned => AuditTargetType::Room,
                    _ => AuditTargetType::Settings,
                },
                Some("target".to_string()),
                serde_json::json!({}),
                None,
                None,
            )
            .await
            .expect("Log should succeed");
    }

    // Verify all actions are logged
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_logs WHERE actor_id = '100104'")
            .fetch_one(&pool)
            .await
            .expect("Query should succeed");

    assert_eq!(
        count,
        i64::try_from(actions.len()).expect("action count should fit in i64"),
        "All actions should be logged"
    );

    // Verify unique IDs for each log
    let ids: Vec<(i64,)> =
        sqlx::query_as("SELECT id FROM audit_logs WHERE actor_id = '100104' ORDER BY created_at")
            .fetch_all(&pool)
            .await
            .expect("Query should succeed");

    let unique_ids: std::collections::HashSet<_> = ids.into_iter().collect();
    assert_eq!(
        unique_ids.len(),
        actions.len(),
        "Each log should have unique ID"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_buffer_full_drops_events_with_fake_pool() {
    let pool = sqlx::PgPool::connect_lazy("postgresql://fake").unwrap();
    let (service, _handle) = AuditService::with_capacity(pool, 5);

    // Fill buffer rapidly
    let mut ok_count = 0;
    let mut error_count = 0;

    for i in 0..100 {
        match service
            .log(
                format!("buffer_actor_{i}"),
                "buffer_tester".to_string(),
                AuditAction::UserCreated,
                AuditTargetType::User,
                Some(format!("target_{i}")),
                serde_json::json!({}),
                None,
                None,
            )
            .await
        {
            Ok(()) => ok_count += 1,
            Err(_) => error_count += 1,
        }
    }

    // Give background task time to process (and fail on fake pool)
    tokio::time::sleep(Duration::from_millis(500)).await;

    // With capacity 5 and 100 events, at least some should be dropped or errored
    // The exact count depends on timing
    assert!(
        service.dropped_count() > 0 || error_count > 0,
        "With capacity 5 and 100 events, should have drops or errors"
    );

    tracing::info!(
        "Buffer test: {} ok, {} errors, {} dropped",
        ok_count,
        error_count,
        service.dropped_count()
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_dropped_count_starts_at_zero() {
    let pool = sqlx::PgPool::connect_lazy("postgresql://fake").unwrap();
    let (service, _handle) = AuditService::with_capacity(pool, 100);

    assert_eq!(
        service.dropped_count(),
        0,
        "Dropped count should start at 0"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_unbuffered_service_never_drops() {
    let pool = sqlx::PgPool::connect_lazy("postgresql://fake").unwrap();
    let service = AuditService::new_unbuffered(pool);

    // Unbuffered service always attempts direct write
    // With fake pool, it will return an error but not drop
    let result = service
        .log(
            "unbuf_actor".to_string(),
            "unbuffered_tester".to_string(),
            AuditAction::UserCreated,
            AuditTargetType::User,
            Some("target".to_string()),
            serde_json::json!({}),
            None,
            None,
        )
        .await;

    // Should fail (fake pool) but dropped_count should remain 0
    assert!(result.is_err(), "Unbuffered write to fake pool should fail");
    assert_eq!(
        service.dropped_count(),
        0,
        "Unbuffered service should not count drops"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_buffered_write_eventually_visible() {
    let (_container, pool) = create_test_pool().await;

    let (service, _handle) = AuditService::new(pool.clone());

    // Log an event
    service
        .log(
            "100105".to_string(),
            "buffered_writer".to_string(),
            AuditAction::UserCreated,
            AuditTargetType::User,
            Some("target".to_string()),
            serde_json::json!({}),
            None,
            None,
        )
        .await
        .expect("Buffered log should succeed");

    tokio::time::sleep(Duration::from_secs(6)).await;

    // Event should be visible in database
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_logs WHERE actor_id = '100105'")
            .fetch_one(&pool)
            .await
            .expect("Query should succeed");

    assert_eq!(count, 1, "Buffered event should eventually be written");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_audit_logging() {
    let (_container, pool) = create_test_pool().await;

    let service = Arc::new(AuditService::new_unbuffered(pool.clone()));
    let barrier = Arc::new(tokio::sync::Barrier::new(20));

    let mut handles = Vec::with_capacity(20);

    for i in 0..20 {
        let s = service.clone();
        let b = barrier.clone();

        let handle = tokio::spawn(async move {
            b.wait().await;
            s.log(
                format!("1002{i}"),
                format!("concurrent_tester_{i}"),
                AuditAction::UserCreated,
                AuditTargetType::User,
                Some(format!("concurrent_target_{i}")),
                serde_json::json!({"index": i}),
                None,
                None,
            )
            .await
        });
        handles.push(handle);
    }

    let mut success_count = 0;
    for handle in handles {
        if handle.await.expect("Task panicked").is_ok() {
            success_count += 1;
        }
    }

    assert_eq!(success_count, 20, "All concurrent logs should succeed");

    // Verify all are in database
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_logs WHERE actor_id::text LIKE '1002%'")
            .fetch_one(&pool)
            .await
            .expect("Query should succeed");

    assert_eq!(count, 20, "All concurrent events should be stored");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_all_audit_actions_are_logged() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    let actions = vec![
        AuditAction::UserCreated,
        AuditAction::UserBanned,
        AuditAction::UserUnbanned,
        AuditAction::UserDeleted,
        AuditAction::RoomCreated,
        AuditAction::RoomBanned,
        AuditAction::RoomUnbanned,
        AuditAction::RoomDeleted,
        AuditAction::SettingsUpdated,
        AuditAction::StreamKicked,
        AuditAction::ChatMessageDeleted,
    ];

    for (i, action) in actions.iter().enumerate() {
        service
            .log(
                format!("act_actr_{i}"),
                "action_tester".to_string(),
                *action,
                match action {
                    AuditAction::UserCreated
                    | AuditAction::UserBanned
                    | AuditAction::UserUnbanned
                    | AuditAction::UserDeleted => AuditTargetType::User,
                    AuditAction::RoomCreated
                    | AuditAction::RoomBanned
                    | AuditAction::RoomUnbanned
                    | AuditAction::RoomDeleted => AuditTargetType::Room,
                    AuditAction::StreamKicked => AuditTargetType::Stream,
                    AuditAction::ChatMessageDeleted => AuditTargetType::ChatMessage,
                    _ => AuditTargetType::Settings,
                },
                Some(format!("action_target_{i}")),
                serde_json::json!({}),
                None,
                None,
            )
            .await
            .expect("Log should succeed");
    }

    // Verify all actions are logged with stable numeric action codes.
    let logged_actions: Vec<(i16,)> = sqlx::query_as(
        "SELECT action FROM audit_logs WHERE actor_username = 'action_tester' ORDER BY created_at",
    )
    .fetch_all(&pool)
    .await
    .expect("Query should succeed");

    assert_eq!(logged_actions.len(), actions.len());

    for ((logged,), expected) in logged_actions.iter().zip(actions.iter()) {
        assert_eq!(
            AuditAction::try_from(*logged).unwrap(),
            *expected,
            "Action code should match"
        );
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_all_target_types_are_logged() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    let target_types = [
        AuditTargetType::User,
        AuditTargetType::Room,
        AuditTargetType::Stream,
        AuditTargetType::Settings,
        AuditTargetType::ChatMessage,
    ];

    for (i, target_type) in target_types.iter().enumerate() {
        service
            .log(
                format!("tgt_actr_{i}"),
                "target_tester".to_string(),
                AuditAction::UserCreated,
                *target_type,
                Some(format!("target_{i}")),
                serde_json::json!({}),
                None,
                None,
            )
            .await
            .expect("Log should succeed");
    }

    // Verify all target types are logged with stable numeric target codes.
    let logged_types: Vec<(Option<i16>,)> = sqlx::query_as(
        "SELECT target_type FROM audit_logs WHERE actor_username = 'target_tester' ORDER BY created_at",
    )
    .fetch_all(&pool)
    .await
    .expect("Query should succeed");

    assert_eq!(logged_types.len(), target_types.len());

    for ((logged,), expected) in logged_types.iter().zip(target_types.iter()) {
        assert_eq!(
            logged.map(|value| AuditTargetType::try_from(value).unwrap()),
            Some(*expected),
            "Target type code should match"
        );
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_log_with_all_null_optionals() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    service
        .log(
            "100106".to_string(),
            "null_tester".to_string(),
            AuditAction::UserCreated,
            AuditTargetType::User,
            None, // No target
            serde_json::json!({}),
            None, // No IP
            None, // No user agent
        )
        .await
        .expect("Log should succeed");

    let row: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT target_id, ip_address, user_agent FROM audit_logs WHERE actor_id = '100106'",
    )
    .fetch_one(&pool)
    .await
    .expect("Query should succeed");

    assert_eq!(row.0, None, "Target ID should be NULL");
    assert_eq!(row.1, None, "IP address should be NULL");
    assert_eq!(row.2, None, "User agent should be NULL");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_log_with_all_optionals() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    service
        .log(
            "100107".to_string(),
            "all_optionals_tester".to_string(),
            AuditAction::UserCreated,
            AuditTargetType::User,
            Some("target_id".to_string()),
            serde_json::json!({"key": "value"}),
            Some("10.0.0.1".to_string()),
            Some("TestClient/2.0".to_string()),
        )
        .await
        .expect("Log should succeed");

    let row: (Option<String>, Option<String>, Option<String>, serde_json::Value) = sqlx::query_as(
        "SELECT target_id, ip_address, user_agent, details FROM audit_logs WHERE actor_id = '100107'",
    )
    .fetch_one(&pool)
    .await
    .expect("Query should succeed");

    assert_eq!(row.0, Some("target_id".to_string()));
    assert_eq!(row.1, Some("10.0.0.1".to_string()));
    assert_eq!(row.2, Some("TestClient/2.0".to_string()));
    assert_eq!(row.3["key"], "value");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_log_stream_kicked_helper() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    service
        .log_stream_kicked(
            "100108".to_string(),
            "stream_kicker".to_string(),
            "room_123".to_string(),
            "media_456".to_string(),
            Some("Inappropriate content".to_string()),
            Some("192.168.1.1".to_string()),
            Some("StreamClient/1.0".to_string()),
        )
        .await
        .expect("Stream kick log should succeed");

    let row: (i16, i16, Option<String>, String, serde_json::Value) = sqlx::query_as(
        r"
        SELECT action, target_type, target_id, actor_username, details
        FROM audit_logs
        WHERE actor_id = '100108'
        ",
    )
    .fetch_one(&pool)
    .await
    .expect("Query should succeed");

    assert_eq!(
        AuditAction::try_from(row.0).unwrap(),
        AuditAction::StreamKicked
    );
    assert_eq!(
        AuditTargetType::try_from(row.1).unwrap(),
        AuditTargetType::Stream
    );
    assert_eq!(row.2, Some("room_123:media_456".to_string()));
    assert_eq!(row.3, "stream_kicker");
    assert_eq!(row.4["room_id"], "room_123");
    assert_eq!(row.4["media_id"], "media_456");
    assert_eq!(row.4["reason"], "Inappropriate content");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_log_stream_kicked_without_reason() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    service
        .log_stream_kicked(
            "100109".to_string(),
            "stream_admin".to_string(),
            "room_abc".to_string(),
            "media_xyz".to_string(),
            None, // No reason
            None,
            None,
        )
        .await
        .expect("Stream kick log should succeed");

    let row: (serde_json::Value,) =
        sqlx::query_as("SELECT details FROM audit_logs WHERE actor_id = '100109'")
            .fetch_one(&pool)
            .await
            .expect("Query should succeed");

    assert_eq!(row.0["room_id"], "room_abc");
    assert_eq!(row.0["media_id"], "media_xyz");
    assert_eq!(row.0["reason"], "");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_settings_viewed_audit_log() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    // Log a settings view event
    service
        .log(
            "100110".to_string(),
            "admin_user".to_string(),
            AuditAction::SettingsViewed,
            AuditTargetType::Settings,
            None,
            serde_json::json!({
                "group_count": 5,
                "groups": ["general", "security", "proxy", "email", "p2p"],
            }),
            Some("192.168.1.50".to_string()),
            Some("Mozilla/5.0 AdminClient/1.0".to_string()),
        )
        .await
        .expect("Settings viewed log should succeed");

    let row: (i16, i16, Option<String>, Option<String>, serde_json::Value) = sqlx::query_as(
        r"
        SELECT action, target_type, ip_address, user_agent, details
        FROM audit_logs
        WHERE actor_id = '100110'
        ",
    )
    .fetch_one(&pool)
    .await
    .expect("Query should succeed");

    assert_eq!(
        AuditAction::try_from(row.0).unwrap(),
        AuditAction::SettingsViewed
    );
    assert_eq!(
        AuditTargetType::try_from(row.1).unwrap(),
        AuditTargetType::Settings
    );
    assert_eq!(row.2, Some("192.168.1.50".to_string()));
    assert_eq!(row.3, Some("Mozilla/5.0 AdminClient/1.0".to_string()));
    assert_eq!(row.4["group_count"], 5);
    assert_eq!(row.4["groups"].as_array().unwrap().len(), 5);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_settings_group_viewed_audit_log() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    // Log a settings group view event
    service
        .log(
            "100111".to_string(),
            "admin_user".to_string(),
            AuditAction::SettingsGroupViewed,
            AuditTargetType::Settings,
            None,
            serde_json::json!({
                "group": "security",
            }),
            Some("10.0.0.1".to_string()),
            None,
        )
        .await
        .expect("Settings group viewed log should succeed");

    let row: (i16, i16, serde_json::Value) = sqlx::query_as(
        r"
        SELECT action, target_type, details
        FROM audit_logs
        WHERE actor_id = '100111'
        ",
    )
    .fetch_one(&pool)
    .await
    .expect("Query should succeed");

    assert_eq!(
        AuditAction::try_from(row.0).unwrap(),
        AuditAction::SettingsGroupViewed
    );
    assert_eq!(
        AuditTargetType::try_from(row.1).unwrap(),
        AuditTargetType::Settings
    );
    assert_eq!(row.2["group"], "security");
}
