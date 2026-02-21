//! AuditLogRepository integration tests
//!
//! Tests: list filter combinations, get_by_id 365-day visibility limit.
//!
//! Run with: cargo test -p synctv-core --test audit_repository_tests

use synctv_core::repository::{AuditLogRepository, AuditLogQuery};
use synctv_core::models::{PageParams, generate_id};
use chrono::{Utc, Duration};
use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

const POSTGRES_VERSION: &str = "16-alpine";

async fn create_test_pool() -> (ContainerAsync<Postgres>, PgPool) {
    let postgres = Postgres::default()
        .with_db_name("synctv_test")
        .with_user("synctv")
        .with_password("synctv_test")
        .with_tag(POSTGRES_VERSION)
        .start()
        .await
        .expect("Failed to start Postgres container");

    let connection_string = format!(
        "postgresql://synctv:synctv_test@127.0.0.1:{}/synctv_test",
        postgres
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get port")
    );

    let pool = PgPool::connect(&connection_string)
        .await
        .expect("Failed to create pool");

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (postgres, pool)
}

/// Insert an audit log directly via SQL and return the generated id.
async fn insert_audit_log(
    pool: &PgPool,
    actor_id: Option<&str>,
    actor_username: Option<&str>,
    action: &str,
    target_type: Option<&str>,
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
    .bind(action)
    .bind(target_type)
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

    let actor_a = generate_id(); // 12-char nanoid
    let actor_b = generate_id();

    insert_audit_log(&pool, Some(&actor_a), None, "login", None, None, now).await;
    insert_audit_log(&pool, Some(&actor_b), None, "login", None, None, now).await;

    let query = AuditLogQuery {
        actor_id: Some(actor_a.clone()),
        from: Some(now - Duration::hours(1)),
        ..Default::default()
    };
    let (rows, total) = repo.list(&query).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].actor_id.as_deref().map(|s| s.trim()), Some(actor_a.as_str()));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_filter_by_action() {
    let (_container, pool) = create_test_pool().await;
    let repo = AuditLogRepository::new(pool.clone());
    let now = Utc::now();

    insert_audit_log(&pool, None, None, "user_created", None, None, now).await;
    insert_audit_log(&pool, None, None, "user_banned", None, None, now).await;
    insert_audit_log(&pool, None, None, "user_banned", None, None, now).await;

    let query = AuditLogQuery {
        action: Some("user_banned".to_string()),
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

    insert_audit_log(&pool, None, None, "delete", Some("room"), Some("room_001"), now).await;
    insert_audit_log(&pool, None, None, "update", Some("room"), Some("room_002"), now).await;
    insert_audit_log(&pool, None, None, "update", Some("user"), Some("user_001"), now).await;

    let query = AuditLogQuery {
        target_type: Some("room".to_string()),
        target_id: Some("room_001".to_string()),
        from: Some(now - Duration::hours(1)),
        ..Default::default()
    };
    let (rows, total) = repo.list(&query).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "delete");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_filter_by_time_range() {
    let (_container, pool) = create_test_pool().await;
    let repo = AuditLogRepository::new(pool.clone());
    let now = Utc::now();
    let yesterday = now - Duration::days(1);
    let two_days_ago = now - Duration::days(2);

    insert_audit_log(&pool, None, None, "old_action", None, None, two_days_ago).await;
    insert_audit_log(&pool, None, None, "recent_action", None, None, yesterday).await;
    insert_audit_log(&pool, None, None, "newest_action", None, None, now).await;

    let query = AuditLogQuery {
        from: Some(yesterday - Duration::hours(1)),
        to: Some(yesterday + Duration::hours(1)),
        ..Default::default()
    };
    let (rows, total) = repo.list(&query).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "recent_action");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_all_filters_combined() {
    let (_container, pool) = create_test_pool().await;
    let repo = AuditLogRepository::new(pool.clone());
    let now = Utc::now();

    let actor_admin_1 = generate_id();
    let actor_admin_2 = generate_id();

    // The target row
    insert_audit_log(
        &pool,
        Some(&actor_admin_1),
        Some("admin"),
        "ban_user",
        Some("user"),
        Some("user_999"),
        now,
    )
    .await;

    // Decoy rows
    insert_audit_log(&pool, Some(&actor_admin_1), None, "ban_user", Some("room"), Some("room_1"), now).await;
    insert_audit_log(&pool, Some(&actor_admin_2), None, "ban_user", Some("user"), Some("user_999"), now).await;
    insert_audit_log(&pool, Some(&actor_admin_1), None, "delete_user", Some("user"), Some("user_999"), now).await;

    let query = AuditLogQuery {
        actor_id: Some(actor_admin_1.clone()),
        action: Some("ban_user".to_string()),
        target_type: Some("user".to_string()),
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

    let id = insert_audit_log(&pool, None, None, "recent_event", None, None, now).await;

    let row = repo.get_by_id(id).await.unwrap();
    assert!(row.is_some());
    assert_eq!(row.unwrap().action, "recent_event");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_id_older_than_365_days_returns_none() {
    let (_container, pool) = create_test_pool().await;
    let repo = AuditLogRepository::new(pool.clone());
    let old_date = Utc::now() - Duration::days(400);

    let id = insert_audit_log(&pool, None, None, "ancient_event", None, None, old_date).await;

    // The entry exists in the DB but get_by_id should not return it
    let row = repo.get_by_id(id).await.unwrap();
    assert!(row.is_none(), "get_by_id should not return entries older than 365 days");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_id_nonexistent() {
    let (_container, pool) = create_test_pool().await;
    let repo = AuditLogRepository::new(pool.clone());

    let row = repo.get_by_id(999_999_999).await.unwrap();
    assert!(row.is_none());
}
