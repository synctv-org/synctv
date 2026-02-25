//! SettingsRepository integration tests
//!
//! Tests: get non-existent key, get_all ordering.
//!
//! Run with: cargo test -p synctv-core --test settings_repository_tests

use synctv_core::repository::SettingsRepository;
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

// ─── get non-existent key ────────────────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_nonexistent_key_returns_error() {
    let (_container, pool) = create_test_pool().await;
    let repo = SettingsRepository::new(pool.clone());

    let result = repo.get("this_key_does_not_exist_at_all").await;
    assert!(
        result.is_err(),
        "Getting a non-existent key should return an error"
    );
}

// ─── get_all ordering ────────────────────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_all_ordering_by_group_name() {
    let (_container, pool) = create_test_pool().await;
    let repo = SettingsRepository::new(pool.clone());

    // Insert test settings in deliberately unsorted order
    sqlx::query(
        "INSERT INTO settings (key, group_name, value) VALUES ($1, $2, $3) ON CONFLICT (key) DO NOTHING"
    )
    .bind("z_test.key1")
    .bind("z_group")
    .bind("value1")
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO settings (key, group_name, value) VALUES ($1, $2, $3) ON CONFLICT (key) DO NOTHING"
    )
    .bind("a_test.key2")
    .bind("a_group")
    .bind("value2")
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO settings (key, group_name, value) VALUES ($1, $2, $3) ON CONFLICT (key) DO NOTHING"
    )
    .bind("m_test.key3")
    .bind("m_group")
    .bind("value3")
    .execute(&pool)
    .await
    .unwrap();

    let all = repo.get_all().await.unwrap();
    assert!(all.len() >= 3, "Should have at least 3 settings");

    // Filter to just our test keys
    let our_groups: Vec<String> = all
        .iter()
        .filter(|s| s.group_name == "a_group" || s.group_name == "m_group" || s.group_name == "z_group")
        .map(|s| s.group_name.clone())
        .collect();

    // Verify they appear in alphabetical order by group_name
    assert_eq!(our_groups, vec!["a_group", "m_group", "z_group"]);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_all_returns_empty_when_no_settings() {
    let (_container, pool) = create_test_pool().await;
    let repo = SettingsRepository::new(pool.clone());

    // Clear all settings
    sqlx::query("DELETE FROM settings")
        .execute(&pool)
        .await
        .unwrap();

    let all = repo.get_all().await.unwrap();
    assert!(all.is_empty());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_and_update_round_trip() {
    let (_container, pool) = create_test_pool().await;
    let repo = SettingsRepository::new(pool.clone());

    // Insert a setting
    sqlx::query(
        "INSERT INTO settings (key, group_name, value) VALUES ($1, $2, $3) ON CONFLICT (key) DO NOTHING"
    )
    .bind("roundtrip_key")
    .bind("test_group")
    .bind("original_value")
    .execute(&pool)
    .await
    .unwrap();

    // Read it
    let setting = repo.get("roundtrip_key").await.unwrap();
    assert_eq!(setting.value, "original_value");
    assert_eq!(setting.group_name, "test_group");

    // Update it
    let updated = repo.update("roundtrip_key", "new_value").await.unwrap();
    assert_eq!(updated.value, "new_value");
    assert!(updated.updated_at >= setting.updated_at);

    // Read again
    let re_read = repo.get("roundtrip_key").await.unwrap();
    assert_eq!(re_read.value, "new_value");
}
