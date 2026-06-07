#![allow(clippy::unwrap_used)]

use synctv_core::repository::SettingsRepository;
use synctv_core_testing::create_test_pool;

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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_all_ordering_by_group_name() {
    let (_container, pool) = create_test_pool().await;
    let repo = SettingsRepository::new(pool.clone());

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

    let our_groups: Vec<String> = all
        .iter()
        .filter(|s| {
            s.group_name == "a_group" || s.group_name == "m_group" || s.group_name == "z_group"
        })
        .map(|s| s.group_name.clone())
        .collect();

    assert_eq!(our_groups, vec!["a_group", "m_group", "z_group"]);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_all_returns_empty_when_no_settings() {
    let (_container, pool) = create_test_pool().await;
    let repo = SettingsRepository::new(pool.clone());

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

    sqlx::query(
        "INSERT INTO settings (key, group_name, value) VALUES ($1, $2, $3) ON CONFLICT (key) DO NOTHING"
    )
    .bind("roundtrip_key")
    .bind("test_group")
    .bind("original_value")
    .execute(&pool)
    .await
    .unwrap();

    let setting = repo.get("roundtrip_key").await.unwrap();
    assert_eq!(setting.value, "original_value");
    assert_eq!(setting.group_name, "test_group");

    let updated = repo.update("roundtrip_key", "new_value").await.unwrap();
    assert_eq!(updated.value, "new_value");
    assert!(updated.updated_at >= setting.updated_at);

    let re_read = repo.get("roundtrip_key").await.unwrap();
    assert_eq!(re_read.value, "new_value");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_with_version_increments_version() {
    let (_container, pool) = create_test_pool().await;
    let repo = SettingsRepository::new(pool.clone());

    sqlx::query(
        "INSERT INTO settings (key, group_name, value) VALUES ($1, $2, $3) ON CONFLICT (key) DO NOTHING"
    )
    .bind("version_test_key")
    .bind("test_group")
    .bind("value_v0")
    .execute(&pool)
    .await
    .unwrap();

    let v0 = repo.get("version_test_key").await.unwrap();
    assert_eq!(v0.version, 0, "Initial version should be 0");

    let v1 = repo
        .update_with_version("version_test_key", "value_v1", 0)
        .await
        .unwrap();
    assert_eq!(v1.version, 1, "Version should increment to 1");

    let v2 = repo
        .update_with_version("version_test_key", "value_v2", 1)
        .await
        .unwrap();
    assert_eq!(v2.version, 2, "Version should increment to 2");

    let result = repo
        .update_with_version("version_test_key", "value_stale", 0)
        .await;
    assert!(result.is_err(), "Update with stale version should fail");
    assert!(
        matches!(
            result.unwrap_err(),
            synctv_core::Error::OptimisticLockConflict
        ),
        "Error should be OptimisticLockConflict"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_with_wrong_version_fails() {
    let (_container, pool) = create_test_pool().await;
    let repo = SettingsRepository::new(pool.clone());

    sqlx::query(
        "INSERT INTO settings (key, group_name, value) VALUES ($1, $2, $3) ON CONFLICT (key) DO NOTHING"
    )
    .bind("wrong_version_key")
    .bind("test_group")
    .bind("original")
    .execute(&pool)
    .await
    .unwrap();

    let result = repo
        .update_with_version("wrong_version_key", "new_value", 999)
        .await;
    assert!(result.is_err(), "Update with wrong version should fail");
    assert!(
        matches!(
            result.unwrap_err(),
            synctv_core::Error::OptimisticLockConflict
        ),
        "Error should be OptimisticLockConflict"
    );
}
