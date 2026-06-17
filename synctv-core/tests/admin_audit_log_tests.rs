//! Admin audit log tests
//!
//! Docker tests: cargo test -p synctv-core --test `admin_audit_log_tests` -- --ignored --nocapture

use std::sync::Arc;

use synctv_core::models::{AuditAction, AuditTargetType};
use synctv_core::service::{AuditEventParams, AuditService, StreamKickAuditRequest};
use synctv_core_testing::{create_test_pool, ok, some};

fn audit_action(value: i16) -> AuditAction {
    ok(
        AuditAction::try_from(value),
        "audit action code should be valid",
    )
}

fn audit_target_type(value: i16) -> AuditTargetType {
    ok(
        AuditTargetType::try_from(value),
        "audit target type code should be valid",
    )
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_log_integrity_all_fields() {
    #[derive(sqlx::FromRow)]
    struct AuditLogRow {
        id: i64,
        actor_id: i64,
        actor_username: String,
        action: i16,
        target_type: Option<i16>,
        target_id: Option<String>,
        ip_address: Option<String>,
        user_agent: Option<String>,
    }

    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    ok(
        service
            .log(AuditEventParams {
                actor_id: "100101".to_string(),
                actor_username: "test_admin".to_string(),
                action: AuditAction::UserBanned,
                target_type: AuditTargetType::User,
                target_id: Some("target_user_002".to_string()),
                details: serde_json::json!({
                    "reason": "Policy violation",
                    "duration": "permanent"
                }),
                ip_address: Some("192.168.1.100".to_string()),
                user_agent: Some("Mozilla/5.0 TestAgent/1.0".to_string()),
            })
            .await,
        "audit log should be written",
    );

    let row = ok(
        sqlx::query_as!(
            AuditLogRow,
            r#"
        SELECT id AS "id!",
               actor_id AS "actor_id!",
               actor_username AS "actor_username!",
               action AS "action!",
               target_type AS "target_type!",
               target_id,
               ip_address,
               user_agent
        FROM audit_logs
        WHERE actor_id = 100101
        "#,
        )
        .fetch_one(&pool)
        .await,
        "audit log row should be fetched",
    );

    assert!(row.id > 0, "ID should be generated");
    assert_eq!(row.actor_id, 100_101, "Actor ID should match");
    assert_eq!(
        row.actor_username, "test_admin",
        "Actor username should match"
    );
    assert_eq!(
        audit_action(row.action),
        AuditAction::UserBanned,
        "Action should match"
    );
    assert_eq!(
        row.target_type.map(audit_target_type),
        Some(AuditTargetType::User),
        "Target type should match"
    );
    assert_eq!(
        row.target_id,
        Some("target_user_002".to_string()),
        "Target ID should match"
    );
    assert_eq!(
        row.ip_address,
        Some("192.168.1.100".to_string()),
        "IP address should match"
    );
    assert_eq!(
        row.user_agent,
        Some("Mozilla/5.0 TestAgent/1.0".to_string()),
        "User agent should match"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_log_details_json_integrity() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

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

    ok(
        service
            .log(AuditEventParams {
                actor_id: "100102".to_string(),
                actor_username: "json_tester".to_string(),
                action: AuditAction::SettingsUpdated,
                target_type: AuditTargetType::Settings,
                target_id: None,
                details: details.clone(),
                ip_address: None,
                user_agent: None,
            })
            .await,
        "audit log should be written",
    );

    let details: serde_json::Value = ok(
        sqlx::query_scalar!(
            r#"SELECT details AS "details!: serde_json::Value" FROM audit_logs WHERE actor_id = '100102'"#
        )
        .fetch_one(&pool)
        .await,
        "audit log details should be fetched",
    );

    assert_eq!(details["nested"]["deeply"]["value"], 42);
    assert_eq!(details["array"], serde_json::json!([1, 2, 3]));
    assert_eq!(details["string"], "test");
    assert_eq!(details["boolean"], true);
    assert!(details["null"].is_null());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_log_created_at_timestamp() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    let before = chrono::Utc::now();

    ok(
        service
            .log(AuditEventParams {
                actor_id: "100103".to_string(),
                actor_username: "time_tester".to_string(),
                action: AuditAction::UserCreated,
                target_type: AuditTargetType::User,
                target_id: Some("new_user".to_string()),
                details: serde_json::json!({}),
                ip_address: None,
                user_agent: None,
            })
            .await,
        "audit log should be written",
    );

    let after = chrono::Utc::now();

    let created_at: chrono::DateTime<chrono::Utc> = ok(
        sqlx::query_scalar!(
            r#"SELECT created_at AS "created_at!" FROM audit_logs WHERE actor_id = '100103'"#
        )
        .fetch_one(&pool)
        .await,
        "audit log timestamp should be fetched",
    );

    assert!(created_at >= before, "created_at should be >= before");
    assert!(created_at <= after, "created_at should be <= after");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_log_multiple_actions_same_actor() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    let actions = vec![
        AuditAction::UserCreated,
        AuditAction::UserBanned,
        AuditAction::RoomCreated,
        AuditAction::RoomBanned,
        AuditAction::SettingsUpdated,
    ];

    for action in &actions {
        ok(
            service
                .log(AuditEventParams {
                    actor_id: "100104".to_string(),
                    actor_username: "multi_tester".to_string(),
                    action: *action,
                    target_type: match action {
                        AuditAction::UserCreated | AuditAction::UserBanned => AuditTargetType::User,
                        AuditAction::RoomCreated | AuditAction::RoomBanned => AuditTargetType::Room,
                        _ => AuditTargetType::Settings,
                    },
                    target_id: Some("target".to_string()),
                    details: serde_json::json!({}),
                    ip_address: None,
                    user_agent: None,
                })
                .await,
            "audit log should be written",
        );
    }

    let count: i64 = ok(
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!" FROM audit_logs WHERE actor_id = '100104'"#
        )
        .fetch_one(&pool)
        .await,
        "audit log count should be fetched",
    );

    assert_eq!(usize::try_from(count), Ok(actions.len()));

    let ids: Vec<i64> = ok(
        sqlx::query_scalar!(
            r#"SELECT id AS "id!" FROM audit_logs WHERE actor_id = '100104' ORDER BY created_at"#
        )
        .fetch_all(&pool)
        .await,
        "audit log ids should be fetched",
    );

    let unique_ids: std::collections::HashSet<_> = ids.into_iter().collect();
    assert_eq!(
        unique_ids.len(),
        actions.len(),
        "Each log should have unique ID"
    );
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
            s.log(AuditEventParams {
                actor_id: format!("1002{i}"),
                actor_username: format!("concurrent_tester_{i}"),
                action: AuditAction::UserCreated,
                target_type: AuditTargetType::User,
                target_id: Some(format!("concurrent_target_{i}")),
                details: serde_json::json!({"index": i}),
                ip_address: None,
                user_agent: None,
            })
            .await
        });
        handles.push(handle);
    }

    let mut success_count = 0;
    for handle in handles {
        if ok(handle.await, "audit logging task should complete").is_ok() {
            success_count += 1;
        }
    }

    assert_eq!(success_count, 20, "All concurrent logs should succeed");

    let count: i64 = ok(
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!" FROM audit_logs WHERE actor_id::text LIKE '1002%'"#
        )
        .fetch_one(&pool)
        .await,
        "concurrent audit log count should be fetched",
    );

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
        ok(
            service
                .log(AuditEventParams {
                    actor_id: format!("act_actr_{i}"),
                    actor_username: "action_tester".to_string(),
                    action: *action,
                    target_type: match action {
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
                    target_id: Some(format!("action_target_{i}")),
                    details: serde_json::json!({}),
                    ip_address: None,
                    user_agent: None,
                })
                .await,
            "audit log should be written",
        );
    }

    let logged_actions: Vec<i16> = ok(
        sqlx::query_scalar!(
            r#"SELECT action AS "action!" FROM audit_logs WHERE actor_username = 'action_tester' ORDER BY created_at"#
        )
        .fetch_all(&pool)
        .await,
        "logged audit actions should be fetched",
    );

    assert_eq!(logged_actions.len(), actions.len());

    for (logged, expected) in logged_actions.iter().zip(actions.iter()) {
        assert_eq!(audit_action(*logged), *expected, "Action code should match");
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
        ok(
            service
                .log(AuditEventParams {
                    actor_id: format!("tgt_actr_{i}"),
                    actor_username: "target_tester".to_string(),
                    action: AuditAction::UserCreated,
                    target_type: *target_type,
                    target_id: Some(format!("target_{i}")),
                    details: serde_json::json!({}),
                    ip_address: None,
                    user_agent: None,
                })
                .await,
            "audit log should be written",
        );
    }

    let logged_types: Vec<Option<i16>> = ok(
        sqlx::query_scalar!(
            "SELECT target_type FROM audit_logs WHERE actor_username = 'target_tester' ORDER BY created_at"
        )
        .fetch_all(&pool)
        .await,
        "logged audit target types should be fetched",
    );

    assert_eq!(logged_types.len(), target_types.len());

    for (logged, expected) in logged_types.iter().zip(target_types.iter()) {
        assert_eq!(
            logged.map(audit_target_type),
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

    ok(
        service
            .log(AuditEventParams {
                actor_id: "100106".to_string(),
                actor_username: "null_tester".to_string(),
                action: AuditAction::UserCreated,
                target_type: AuditTargetType::User,
                target_id: None,
                details: serde_json::json!({}),
                ip_address: None,
                user_agent: None,
            })
            .await,
        "audit log should be written",
    );

    let row = ok(
        sqlx::query!(
            "SELECT target_id, ip_address, user_agent FROM audit_logs WHERE actor_id = '100106'",
        )
        .fetch_one(&pool)
        .await,
        "audit log optional fields should be fetched",
    );

    assert_eq!(row.target_id, None, "Target ID should be NULL");
    assert_eq!(row.ip_address, None, "IP address should be NULL");
    assert_eq!(row.user_agent, None, "User agent should be NULL");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_log_with_all_optionals() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    ok(
        service
            .log(AuditEventParams {
                actor_id: "100107".to_string(),
                actor_username: "all_optionals_tester".to_string(),
                action: AuditAction::UserCreated,
                target_type: AuditTargetType::User,
                target_id: Some("target_id".to_string()),
                details: serde_json::json!({"key": "value"}),
                ip_address: Some("10.0.0.1".to_string()),
                user_agent: Some("TestClient/2.0".to_string()),
            })
            .await,
        "audit log should be written",
    );

    let row = ok(
        sqlx::query!(
            r#"SELECT target_id, ip_address, user_agent, details AS "details!: serde_json::Value" FROM audit_logs WHERE actor_id = '100107'"#,
        )
        .fetch_one(&pool)
        .await,
        "audit log optional fields should be fetched",
    );

    assert_eq!(row.target_id, Some("target_id".to_string()));
    assert_eq!(row.ip_address, Some("10.0.0.1".to_string()));
    assert_eq!(row.user_agent, Some("TestClient/2.0".to_string()));
    assert_eq!(row.details["key"], "value");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_log_stream_kicked_helper() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    ok(
        service
            .log_stream_kicked(StreamKickAuditRequest {
                actor_id: "100108".to_string(),
                actor_username: "stream_kicker".to_string(),
                room_id: "room_123".to_string(),
                media_id: "media_456".to_string(),
                reason: Some("Inappropriate content".to_string()),
                ip_address: Some("192.168.1.1".to_string()),
                user_agent: Some("StreamClient/1.0".to_string()),
            })
            .await,
        "stream kick audit log should be written",
    );

    let row = ok(
        sqlx::query!(
            r#"
        SELECT action AS "action!",
               target_type AS "target_type!",
               target_id,
               actor_username AS "actor_username!",
               details AS "details!: serde_json::Value"
        FROM audit_logs
        WHERE actor_id = '100108'
        "#,
        )
        .fetch_one(&pool)
        .await,
        "stream kick audit row should be fetched",
    );

    assert_eq!(audit_action(row.action), AuditAction::StreamKicked);
    assert_eq!(audit_target_type(row.target_type), AuditTargetType::Stream);
    assert_eq!(row.target_id, Some("room_123:media_456".to_string()));
    assert_eq!(row.actor_username, "stream_kicker");
    assert_eq!(row.details["room_id"], "room_123");
    assert_eq!(row.details["media_id"], "media_456");
    assert_eq!(row.details["reason"], "Inappropriate content");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_log_stream_kicked_without_reason() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    ok(
        service
            .log_stream_kicked(StreamKickAuditRequest {
                actor_id: "100109".to_string(),
                actor_username: "stream_admin".to_string(),
                room_id: "room_abc".to_string(),
                media_id: "media_xyz".to_string(),
                reason: None,
                ip_address: None,
                user_agent: None,
            })
            .await,
        "stream kick audit log should be written",
    );

    let details: serde_json::Value = ok(
        sqlx::query_scalar!(
            r#"SELECT details AS "details!: serde_json::Value" FROM audit_logs WHERE actor_id = '100109'"#
        )
        .fetch_one(&pool)
        .await,
        "stream kick audit details should be fetched",
    );

    assert_eq!(details["room_id"], "room_abc");
    assert_eq!(details["media_id"], "media_xyz");
    assert_eq!(details["reason"], serde_json::Value::Null);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_settings_viewed_audit_log() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    ok(
        service
            .log(AuditEventParams {
                actor_id: "100110".to_string(),
                actor_username: "admin_user".to_string(),
                action: AuditAction::SettingsViewed,
                target_type: AuditTargetType::Settings,
                target_id: None,
                details: serde_json::json!({
                    "group_count": 5,
                    "groups": ["general", "security", "proxy", "email", "p2p"],
                }),
                ip_address: Some("192.168.1.50".to_string()),
                user_agent: Some("Mozilla/5.0 AdminClient/1.0".to_string()),
            })
            .await,
        "settings viewed audit log should be written",
    );

    let row = ok(
        sqlx::query!(
            r#"
        SELECT action AS "action!",
               target_type AS "target_type!",
               ip_address,
               user_agent,
               details AS "details!: serde_json::Value"
        FROM audit_logs
        WHERE actor_id = '100110'
        "#,
        )
        .fetch_one(&pool)
        .await,
        "settings viewed audit row should be fetched",
    );

    assert_eq!(audit_action(row.action), AuditAction::SettingsViewed);
    assert_eq!(
        audit_target_type(row.target_type),
        AuditTargetType::Settings
    );
    assert_eq!(row.ip_address, Some("192.168.1.50".to_string()));
    assert_eq!(
        row.user_agent,
        Some("Mozilla/5.0 AdminClient/1.0".to_string())
    );
    assert_eq!(row.details["group_count"], 5);
    assert_eq!(
        some(
            row.details["groups"].as_array(),
            "settings groups should be an array"
        )
        .len(),
        5
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_settings_group_viewed_audit_log() {
    let (_container, pool) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    ok(
        service
            .log(AuditEventParams {
                actor_id: "100111".to_string(),
                actor_username: "admin_user".to_string(),
                action: AuditAction::SettingsGroupViewed,
                target_type: AuditTargetType::Settings,
                target_id: None,
                details: serde_json::json!({
                    "group": "security",
                }),
                ip_address: Some("10.0.0.1".to_string()),
                user_agent: None,
            })
            .await,
        "settings group viewed audit log should be written",
    );

    let row = ok(
        sqlx::query!(
            r#"
        SELECT action AS "action!",
               target_type AS "target_type!",
               details AS "details!: serde_json::Value"
        FROM audit_logs
        WHERE actor_id = '100111'
        "#,
        )
        .fetch_one(&pool)
        .await,
        "settings group viewed audit row should be fetched",
    );

    assert_eq!(audit_action(row.action), AuditAction::SettingsGroupViewed);
    assert_eq!(
        audit_target_type(row.target_type),
        AuditTargetType::Settings
    );
    assert_eq!(row.details["group"], "security");
}
