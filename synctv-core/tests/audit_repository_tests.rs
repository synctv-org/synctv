//! `AuditLogRepository` integration tests
//!
//! Tests: list filter combinations, `get_by_id` 365-day visibility limit.
//!
//! Run with: cargo test -p synctv-core --test `audit_repository_tests`
#![allow(clippy::unwrap_used)]

use chrono::{Duration, Utc};
use sqlx::PgPool;
use synctv_core::models::{PageParams, UserId};
use synctv_core::repository::{AuditLogQuery, AuditLogRepository};
use synctv_core::service::{AuditAction, AuditTargetType};
use synctv_core_testing::create_test_pool;
/// Insert an audit log directly via SQL and return the generated id.
async fn insert_audit_log(
    pool: &PgPool,
    actor_id: Option<UserId>,
    actor_username: Option<&str>,
    action: AuditAction,
    target_type: Option<AuditTargetType>,
    target_id: Option<&str>,
    created_at: chrono::DateTime<Utc>,
) -> i64 {
    let row: (i64,) = sqlx::query_as(
        r"
        INSERT INTO audit_logs (actor_id, actor_username, action, target_type, target_id, created_at)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id
        ",
    )
    .bind(actor_id)
    .bind(actor_username)
    .bind(i16::from(action))
    .bind(target_type.map(i16::from))
    .bind(target_id)
    .bind(created_at)
    .fetch_one(pool)
    .await
    .expect("Failed to insert audit log");
    row.0
}

// ─── list filter tests ───────────────────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_filter_by_actor_id() {
    let (_container, pool) = create_test_pool().await;
    let repo = AuditLogRepository::new(pool.clone());
    let now = Utc::now();

    let actor_a = UserId::new();
    let actor_b = UserId::new();

    insert_audit_log(
        &pool,
        Some(actor_a),
        None,
        AuditAction::UserLogin,
        None,
        None,
        now,
    )
    .await;
    insert_audit_log(
        &pool,
        Some(actor_b),
        None,
        AuditAction::UserLogin,
        None,
        None,
        now,
    )
    .await;

    let query = AuditLogQuery {
        actor_id: Some(actor_a),
        from: Some(now - Duration::hours(1)),
        ..Default::default()
    };
    let (rows, total) = repo.list(&query).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].actor_id, Some(actor_a));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_filter_by_action() {
    let (_container, pool) = create_test_pool().await;
    let repo = AuditLogRepository::new(pool.clone());
    let now = Utc::now();

    insert_audit_log(&pool, None, None, AuditAction::UserCreated, None, None, now).await;
    insert_audit_log(&pool, None, None, AuditAction::UserBanned, None, None, now).await;
    insert_audit_log(&pool, None, None, AuditAction::UserBanned, None, None, now).await;

    let query = AuditLogQuery {
        action: Some(AuditAction::UserBanned),
        from: Some(now - Duration::hours(1)),
        ..Default::default()
    };
    let (rows, total) = repo.list(&query).await.unwrap();
    assert_eq!(total, 2);
    assert_eq!(rows.len(), 2);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_filter_by_target_type_and_target_id() {
    let (_container, pool) = create_test_pool().await;
    let repo = AuditLogRepository::new(pool.clone());
    let now = Utc::now();

    insert_audit_log(
        &pool,
        None,
        None,
        AuditAction::RoomDeleted,
        Some(AuditTargetType::Room),
        Some("room_001"),
        now,
    )
    .await;
    insert_audit_log(
        &pool,
        None,
        None,
        AuditAction::RoomCreated,
        Some(AuditTargetType::Room),
        Some("room_002"),
        now,
    )
    .await;
    insert_audit_log(
        &pool,
        None,
        None,
        AuditAction::UserCreated,
        Some(AuditTargetType::User),
        Some("user_001"),
        now,
    )
    .await;

    let query = AuditLogQuery {
        target_type: Some(AuditTargetType::Room),
        target_id: Some("room_001".to_string()),
        from: Some(now - Duration::hours(1)),
        ..Default::default()
    };
    let (rows, total) = repo.list(&query).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, AuditAction::RoomDeleted);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_filter_by_time_range() {
    let (_container, pool) = create_test_pool().await;
    let repo = AuditLogRepository::new(pool.clone());
    let now = Utc::now();
    let yesterday = now - Duration::days(1);
    let two_days_ago = now - Duration::days(2);

    insert_audit_log(
        &pool,
        None,
        None,
        AuditAction::UserDeleted,
        None,
        None,
        two_days_ago,
    )
    .await;
    insert_audit_log(
        &pool,
        None,
        None,
        AuditAction::UserCreated,
        None,
        None,
        yesterday,
    )
    .await;
    insert_audit_log(&pool, None, None, AuditAction::UserBanned, None, None, now).await;

    let query = AuditLogQuery {
        from: Some(yesterday - Duration::hours(1)),
        to: Some(yesterday + Duration::hours(1)),
        ..Default::default()
    };
    let (rows, total) = repo.list(&query).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, AuditAction::UserCreated);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_all_filters_combined() {
    let (_container, pool) = create_test_pool().await;
    let repo = AuditLogRepository::new(pool.clone());
    let now = Utc::now();

    let actor_admin_1 = UserId::new();
    let actor_admin_2 = UserId::new();

    // The target row
    insert_audit_log(
        &pool,
        Some(actor_admin_1),
        Some("admin"),
        AuditAction::UserBanned,
        Some(AuditTargetType::User),
        Some("user_999"),
        now,
    )
    .await;

    // Decoy rows
    insert_audit_log(
        &pool,
        Some(actor_admin_1),
        None,
        AuditAction::UserBanned,
        Some(AuditTargetType::Room),
        Some("room_1"),
        now,
    )
    .await;
    insert_audit_log(
        &pool,
        Some(actor_admin_2),
        None,
        AuditAction::UserBanned,
        Some(AuditTargetType::User),
        Some("user_999"),
        now,
    )
    .await;
    insert_audit_log(
        &pool,
        Some(actor_admin_1),
        None,
        AuditAction::UserDeleted,
        Some(AuditTargetType::User),
        Some("user_999"),
        now,
    )
    .await;

    let query = AuditLogQuery {
        actor_id: Some(actor_admin_1),
        action: Some(AuditAction::UserBanned),
        target_type: Some(AuditTargetType::User),
        target_id: Some("user_999".to_string()),
        from: Some(now - Duration::hours(1)),
        to: Some(now + Duration::hours(1)),
        page: PageParams::new(Some(1), Some(10)),
    };
    let (rows, total) = repo.list(&query).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].actor_username.as_deref(), Some("admin"));
}

// ─── get_by_id 365-day visibility ────────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_id_within_365_days() {
    let (_container, pool) = create_test_pool().await;
    let repo = AuditLogRepository::new(pool.clone());
    let now = Utc::now();

    let id = insert_audit_log(&pool, None, None, AuditAction::UserLogin, None, None, now).await;

    let row = repo.get_by_id(id).await.unwrap();
    assert!(row.is_some());
    assert_eq!(row.unwrap().action, AuditAction::UserLogin);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_id_older_than_365_days_returns_none() {
    let (_container, pool) = create_test_pool().await;
    let repo = AuditLogRepository::new(pool.clone());
    let old_date = Utc::now() - Duration::days(400);

    let id = insert_audit_log(
        &pool,
        None,
        None,
        AuditAction::UserLogout,
        None,
        None,
        old_date,
    )
    .await;

    // The entry exists in the DB but get_by_id should not return it
    let row = repo.get_by_id(id).await.unwrap();
    assert!(
        row.is_none(),
        "get_by_id should not return entries older than 365 days"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_id_nonexistent() {
    let (_container, pool) = create_test_pool().await;
    let repo = AuditLogRepository::new(pool.clone());

    let row = repo.get_by_id(999_999_999).await.unwrap();
    assert!(row.is_none());
}
