//! Audit service tests
//!
//! Tests audit service buffering, unbuffered writes, and query logic.
//!
//! Run with: cargo test --test audit_service_tests
//! Run Docker tests: cargo test --test audit_service_tests -- --ignored

use synctv_core::service::{
    AuditService, AuditAction, AuditTargetType,
};

async fn create_test_pool() -> (sqlx::PgPool, testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>) {
    use testcontainers::core::ImageExt;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::postgres::Postgres;

    let container = Postgres::default()
        .with_db_name("synctv_test")
        .with_user("synctv")
        .with_password("synctv_test")
        .with_tag("16-alpine")
        .start()
        .await
        .expect("Failed to start Postgres container");

    let host = container.get_host().await.expect("Failed to get Postgres host");
    let port = container.get_host_port_ipv4(5432).await.expect("Failed to get Postgres port");

    let database_url = format!(
        "postgresql://synctv:synctv_test@{}:{}/synctv_test",
        host, port
    );

    let pool = sqlx::PgPool::connect(&database_url)
        .await
        .expect("Failed to connect to Postgres container");

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (pool, container)
}

// ============================================================================
// Unbuffered write tests (require Docker)
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_unbuffered_writes_immediately() {
    let (pool, _container) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    // Write an audit event
    service
        .log(
            "actor_1".to_string(),
            "admin".to_string(),
            AuditAction::UserCreated,
            AuditTargetType::User,
            Some("target_user_1".to_string()),
            serde_json::json!({"reason": "test"}),
            Some("127.0.0.1".to_string()),
            Some("TestAgent/1.0".to_string()),
        )
        .await
        .expect("Unbuffered write should succeed");

    // Query immediately -- should be visible since unbuffered writes go directly to DB
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM audit_logs WHERE actor_id = 'actor_1'")
        .fetch_one(&pool)
        .await
        .expect("Query should succeed");

    assert_eq!(row.0, 1, "Unbuffered write should be immediately visible");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_audit_query_filter_by_action() {
    let (pool, _container) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    // Write events with different actions
    service
        .log(
            "actor_filter".to_string(),
            "admin".to_string(),
            AuditAction::UserBanned,
            AuditTargetType::User,
            Some("user_1".to_string()),
            serde_json::json!({}),
            None,
            None,
        )
        .await
        .unwrap();

    service
        .log(
            "actor_filter".to_string(),
            "admin".to_string(),
            AuditAction::RoomCreated,
            AuditTargetType::Room,
            Some("room_1".to_string()),
            serde_json::json!({}),
            None,
            None,
        )
        .await
        .unwrap();

    service
        .log(
            "actor_filter".to_string(),
            "admin".to_string(),
            AuditAction::UserBanned,
            AuditTargetType::User,
            Some("user_2".to_string()),
            serde_json::json!({}),
            None,
            None,
        )
        .await
        .unwrap();

    // Query filtered by action
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT target_id FROM audit_logs WHERE actor_id = 'actor_filter' AND action = 'user_banned' ORDER BY created_at",
    )
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
    let (pool, _container) = create_test_pool().await;

    let service = AuditService::new_unbuffered(pool.clone());

    // Write an event
    service
        .log(
            "actor_date".to_string(),
            "admin".to_string(),
            AuditAction::SettingsUpdated,
            AuditTargetType::Settings,
            None,
            serde_json::json!({"key": "value"}),
            None,
            None,
        )
        .await
        .unwrap();

    // Query with date range that includes now
    let now = chrono::Utc::now();
    let one_hour_ago = now - chrono::Duration::hours(1);
    let one_hour_later = now + chrono::Duration::hours(1);

    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT COUNT(*) FROM audit_logs WHERE actor_id = 'actor_date' AND created_at >= $1 AND created_at <= $2",
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
        "SELECT COUNT(*) FROM audit_logs WHERE actor_id = 'actor_date' AND created_at >= $1 AND created_at <= $2",
    )
    .bind(two_hours_ago)
    .bind(one_and_half_hours_ago)
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(rows[0].0, 0, "Event should not be in the past date range");
}

// ============================================================================
// Buffered service tests (no DB needed)
// ============================================================================

#[tokio::test]
async fn test_buffered_service_enqueues_without_error() {
    let pool = sqlx::PgPool::connect_lazy("postgresql://fake").unwrap();
    let (service, _handle) = AuditService::new(pool);

    // Events should be buffered without error even with a fake pool
    let result = service
        .log(
            "actor".to_string(),
            "admin".to_string(),
            AuditAction::UserCreated,
            AuditTargetType::User,
            Some("user1".to_string()),
            serde_json::json!({}),
            None,
            None,
        )
        .await;

    assert!(result.is_ok());
    assert_eq!(service.dropped_count(), 0);
}

#[tokio::test]
async fn test_unbuffered_service_dropped_count_zero() {
    let pool = sqlx::PgPool::connect_lazy("postgresql://fake").unwrap();
    let service = AuditService::new_unbuffered(pool);
    assert_eq!(service.dropped_count(), 0);
}
