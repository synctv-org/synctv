//! SettingsRepository integration tests
//!
//! Tests: get non-existent key, get_all ordering.
//!
//! Run with: cargo test -p synctv-core --test settings_repository_tests
#![allow(clippy::unwrap_used)]

use synctv_core_testing::{create_test_pool};
use synctv_core::repository::SettingsRepository;
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

// ─── Task #45: Optimistic locking tests ───────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_update_without_optimistic_lock_causes_lost_update() {
    // This test demonstrates the "Lost Update" problem when two concurrent
    // updates happen without optimistic locking:
    //
    // 1. Transaction A reads setting (version N)
    // 2. Transaction B reads setting (version N)
    // 3. Transaction A updates setting
    // 4. Transaction B updates setting (overwrites A's change - LOST!)
    //
    // Expected: Transaction B should fail with OptimisticLockConflict

    let (_container, pool) = create_test_pool().await;
    let repo = SettingsRepository::new(pool.clone());

    // Insert a setting
    sqlx::query(
        "INSERT INTO settings (key, group_name, value) VALUES ($1, $2, $3) ON CONFLICT (key) DO NOTHING"
    )
    .bind("concurrent_test_key")
    .bind("test_group")
    .bind("original_value")
    .execute(&pool)
    .await
    .unwrap();

    // Get the initial version
    let initial = repo.get_with_version("concurrent_test_key").await.unwrap();
    let initial_version = initial.version;

    // Simulate two concurrent updates
    // Both read the same version
    let pool1 = pool.clone();
    let pool2 = pool.clone();
    let key1 = "concurrent_test_key".to_string();
    let key2 = "concurrent_test_key".to_string();

    let handle1 = tokio::spawn(async move {
        let repo1 = SettingsRepository::new(pool1);

        // First update should succeed
        repo1
            .update_with_version(&key1, "update_from_tx1", initial_version)
            .await
    });

    let handle2 = tokio::spawn(async move {
        let repo2 = SettingsRepository::new(pool2);

        // Small delay to ensure TX1 commits first
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Second update should fail with OptimisticLockConflict
        // because it's using stale version
        repo2
            .update_with_version(&key2, "update_from_tx2", initial_version)
            .await
    });

    let result1 = handle1.await.expect("Task 1 panicked");
    let result2 = handle2.await.expect("Task 2 panicked");

    // First update should succeed
    assert!(
        result1.is_ok(),
        "First concurrent update should succeed, got: {:?}",
        result1
    );

    // Second update should fail with OptimisticLockConflict
    // because the version has changed after TX1's update
    assert!(
        result2.is_err(),
        "Second concurrent update should fail due to stale version"
    );

    let err = result2.unwrap_err();
    assert!(
        matches!(err, synctv_core::Error::OptimisticLockConflict),
        "Error should be OptimisticLockConflict, got: {:?}",
        err
    );

    // Verify the final value is from the first update (not lost)
    let final_setting = repo.get("concurrent_test_key").await.unwrap();
    assert_eq!(
        final_setting.value, "update_from_tx1",
        "Final value should be from first update (no lost update)"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_with_version_increments_version() {
    // Verify that each update increments the version number

    let (_container, pool) = create_test_pool().await;
    let repo = SettingsRepository::new(pool.clone());

    // Insert a setting
    sqlx::query(
        "INSERT INTO settings (key, group_name, value) VALUES ($1, $2, $3) ON CONFLICT (key) DO NOTHING"
    )
    .bind("version_test_key")
    .bind("test_group")
    .bind("value_v0")
    .execute(&pool)
    .await
    .unwrap();

    // Get initial version
    let v0 = repo.get_with_version("version_test_key").await.unwrap();
    assert_eq!(v0.version, 0, "Initial version should be 0");

    // First update
    let v1 = repo
        .update_with_version("version_test_key", "value_v1", 0)
        .await
        .unwrap();
    assert_eq!(v1.version, 1, "Version should increment to 1");

    // Second update with correct version
    let v2 = repo
        .update_with_version("version_test_key", "value_v2", 1)
        .await
        .unwrap();
    assert_eq!(v2.version, 2, "Version should increment to 2");

    // Try update with stale version - should fail
    let result = repo.update_with_version("version_test_key", "value_stale", 0).await;
    assert!(
        result.is_err(),
        "Update with stale version should fail"
    );
    assert!(
        matches!(result.unwrap_err(), synctv_core::Error::OptimisticLockConflict),
        "Error should be OptimisticLockConflict"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_with_wrong_version_fails() {
    // Verify that update with incorrect version fails

    let (_container, pool) = create_test_pool().await;
    let repo = SettingsRepository::new(pool.clone());

    // Insert a setting
    sqlx::query(
        "INSERT INTO settings (key, group_name, value) VALUES ($1, $2, $3) ON CONFLICT (key) DO NOTHING"
    )
    .bind("wrong_version_key")
    .bind("test_group")
    .bind("original")
    .execute(&pool)
    .await
    .unwrap();

    // Try to update with wrong version
    let result = repo.update_with_version("wrong_version_key", "new_value", 999).await;
    assert!(
        result.is_err(),
        "Update with wrong version should fail"
    );
    assert!(
        matches!(result.unwrap_err(), synctv_core::Error::OptimisticLockConflict),
        "Error should be OptimisticLockConflict"
    );
}
