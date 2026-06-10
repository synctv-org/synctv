//! PostgreSQL-backed token blacklist integration tests.
//!
//! Pure in-memory and fallback behaviors are covered by the unit tests in
//! `service::auth::token_blacklist`. This file keeps only the Docker-backed
//! integration coverage that exercises the real database implementation.
use synctv_core::service::{auth::token_blacklist::PgTokenBlacklistStore, TokenBlacklistStore};
use synctv_core_testing::{create_test_pool, ok};

fn trusted_dynamic_sql(sql: String) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(sql)
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_pg_family_revocation_survives_cleanup_until_marker_expires() {
    let (_container, pool) = create_test_pool().await;
    let store = PgTokenBlacklistStore::new(pool);
    let key = format!("family:pg_cleanup_guard:{}", synctv_common::snanoid!(8));
    let timestamp = chrono::Utc::now().timestamp();

    ok(
        store.set_family_revoked(&key, timestamp, 120).await,
        "family revocation marker should be stored",
    );
    ok(
        store.cleanup_expired().await,
        "expired blacklist cleanup should succeed",
    );

    assert_eq!(
        ok(
            store.get_family_revoked_at_checked(&key).await,
            "family revocation marker should be read after cleanup"
        ),
        Some(timestamp),
        "cleanup must not delete the family revocation timestamp while the row is still alive"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_pg_family_revocation_timestamp_is_stable_across_reads() {
    let (_container, pool) = create_test_pool().await;
    let store = PgTokenBlacklistStore::new(pool);
    let key = format!("family:pg_stable_ts:{}", synctv_common::snanoid!(8));
    let timestamp = chrono::Utc::now().timestamp();

    ok(
        store.set_family_revoked(&key, timestamp, 120).await,
        "family revocation marker should be stored",
    );

    let first = ok(
        store.get_family_revoked_at_checked(&key).await,
        "first family revocation marker read should succeed",
    );
    tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
    let second = ok(
        store.get_family_revoked_at_checked(&key).await,
        "second family revocation marker read should succeed",
    );

    assert_eq!(first, Some(timestamp));
    assert_eq!(
        second,
        Some(timestamp),
        "family revocation timestamp must remain stable instead of drifting with wall-clock time"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_pg_family_revocation_is_atomic_when_timestamp_write_fails() {
    let (_container, pool) = create_test_pool().await;
    let store = PgTokenBlacklistStore::new(pool.clone());
    let key = format!("family:pg_atomicity_guard:{}", synctv_common::snanoid!(8));
    let timestamp = chrono::Utc::now().timestamp();
    let key_sql_literal = key.replace('\'', "''");

    let trigger_fn_sql = r"
        CREATE OR REPLACE FUNCTION fail_token_blacklist_family_insert()
        RETURNS trigger AS $$
        BEGIN
            IF NEW.jti = 'REPLACE_ME' THEN
                RAISE EXCEPTION 'forced family timestamp failure';
            END IF;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        "
    .replace("REPLACE_ME", &key_sql_literal);

    ok(
        sqlx::query(trusted_dynamic_sql(trigger_fn_sql))
            .execute(&pool)
            .await,
        "failure trigger function should be created",
    );

    ok(
        sqlx::query(
            "DROP TRIGGER IF EXISTS trg_fail_token_blacklist_family_insert ON auth_token_blacklist",
        )
        .execute(&pool)
        .await,
        "existing failure trigger should be dropped",
    );
    ok(
        sqlx::query(
            r"
        CREATE TRIGGER trg_fail_token_blacklist_family_insert
        BEFORE INSERT OR UPDATE ON auth_token_blacklist
        FOR EACH ROW
        EXECUTE FUNCTION fail_token_blacklist_family_insert()
        ",
        )
        .execute(&pool)
        .await,
        "failure trigger should be created",
    );

    let result = store.set_family_revoked(&key, timestamp, 120).await;
    assert!(
        result.is_err(),
        "forced family revoke write failure must bubble up as an error"
    );

    let row_exists: bool = ok(
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM auth_token_blacklist WHERE jti = $1)")
            .bind(&key)
            .fetch_one(&pool)
            .await,
        "partial blacklist row lookup should succeed",
    );

    assert!(
        !row_exists,
        "family revoke must be atomic: no partial row should remain after failure"
    );

    ok(
        sqlx::query(
            "DROP TRIGGER IF EXISTS trg_fail_token_blacklist_family_insert ON auth_token_blacklist",
        )
        .execute(&pool)
        .await,
        "failure trigger should be dropped",
    );
    ok(
        sqlx::query("DROP FUNCTION IF EXISTS fail_token_blacklist_family_insert()")
            .execute(&pool)
            .await,
        "failure trigger function should be dropped",
    );
}
