//! ProviderInstanceRepository integration tests
//!
//! Tests: migrate_plaintext_to_encrypted (idempotency, None fields, rollback),
//!        encryption prefix edge case.
//!
//! Run with: cargo test -p synctv-core --test provider_instance_repository_tests

use synctv_core::{
    models::ProviderInstance,
    repository::ProviderInstanceRepository,
    service::CredentialEncryption,
};
use chrono::Utc;
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

fn test_key() -> Vec<u8> {
    vec![
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
        0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
        0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
    ]
}

fn make_instance(name: &str, jwt_secret: Option<&str>, custom_ca: Option<&str>) -> ProviderInstance {
    let now = Utc::now();
    ProviderInstance {
        name: name.to_string(),
        endpoint: "grpc://localhost:50051".to_string(),
        comment: Some("test instance".to_string()),
        jwt_secret: jwt_secret.map(|s| s.to_string()),
        custom_ca: custom_ca.map(|s| s.to_string()),
        timeout: "10s".to_string(),
        tls: false,
        insecure_tls: false,
        providers: vec!["bilibili".to_string()],
        enabled: true,
        created_at: now,
        updated_at: now,
    }
}

// ─── migrate_plaintext_to_encrypted: idempotency ─────────────────────

#[tokio::test]
async fn test_migrate_plaintext_to_encrypted_idempotent() {
    let (_container, pool) = create_test_pool().await;
    let enc = CredentialEncryption::new(&test_key()).unwrap();

    // Create an instance with plaintext secrets
    let no_enc_repo = ProviderInstanceRepository::new(pool.clone());
    let instance = make_instance("idempotent_test", Some("my_secret"), Some("my_ca_cert"));
    no_enc_repo.create(&instance).await.unwrap();

    let enc_repo = ProviderInstanceRepository::new_with_encryption(pool.clone(), enc.clone());

    // First migration should migrate 1 instance
    let count1 = enc_repo.migrate_plaintext_to_encrypted().await.unwrap();
    assert_eq!(count1, 1);

    // Second migration should migrate 0 (already encrypted)
    let count2 = enc_repo.migrate_plaintext_to_encrypted().await.unwrap();
    assert_eq!(count2, 0, "Second migration should be idempotent");

    // Verify decryption works
    let fetched = enc_repo.get_by_name("idempotent_test").await.unwrap().unwrap();
    assert_eq!(fetched.jwt_secret.as_deref(), Some("my_secret"));
    assert_eq!(fetched.custom_ca.as_deref(), Some("my_ca_cert"));
}

// ─── migrate_plaintext_to_encrypted: None fields ─────────────────────

#[tokio::test]
async fn test_migrate_plaintext_to_encrypted_none_fields() {
    let (_container, pool) = create_test_pool().await;
    let enc = CredentialEncryption::new(&test_key()).unwrap();

    // Create an instance with no secrets (None)
    let no_enc_repo = ProviderInstanceRepository::new(pool.clone());
    let instance = make_instance("none_fields_test", None, None);
    no_enc_repo.create(&instance).await.unwrap();

    let enc_repo = ProviderInstanceRepository::new_with_encryption(pool.clone(), enc);

    // Migration should skip this instance
    let count = enc_repo.migrate_plaintext_to_encrypted().await.unwrap();
    assert_eq!(count, 0, "Instance with None secrets should not need migration");

    // Verify it still reads correctly
    let fetched = enc_repo.get_by_name("none_fields_test").await.unwrap().unwrap();
    assert!(fetched.jwt_secret.is_none());
    assert!(fetched.custom_ca.is_none());
}

// ─── migrate_plaintext_to_encrypted: rollback (no encryption) ────────

#[tokio::test]
async fn test_migrate_without_encryption_returns_error() {
    let (_container, pool) = create_test_pool().await;
    let repo = ProviderInstanceRepository::new(pool.clone());

    let result = repo.migrate_plaintext_to_encrypted().await;
    assert!(result.is_err(), "Migration without encryption configured should fail");
}

// ─── encryption prefix edge case ─────────────────────────────────────

#[tokio::test]
async fn test_encryption_prefix_edge_case() {
    let (_container, pool) = create_test_pool().await;
    let enc = CredentialEncryption::new(&test_key()).unwrap();

    // Create an instance where the plaintext value starts with "enc:" prefix
    let no_enc_repo = ProviderInstanceRepository::new(pool.clone());
    let instance = make_instance("prefix_test", Some("enc:looks_like_encrypted"), None);
    no_enc_repo.create(&instance).await.unwrap();

    let enc_repo = ProviderInstanceRepository::new_with_encryption(pool.clone(), enc);

    // This value already starts with "enc:" so migration should skip it
    // (it looks like an already-encrypted value)
    let count = enc_repo.migrate_plaintext_to_encrypted().await.unwrap();
    assert_eq!(count, 0, "Value starting with 'enc:' should be treated as already encrypted");
}

// ─── create and read with encryption ─────────────────────────────────

#[tokio::test]
async fn test_create_and_read_with_encryption() {
    let (_container, pool) = create_test_pool().await;
    let enc = CredentialEncryption::new(&test_key()).unwrap();
    let enc_repo = ProviderInstanceRepository::new_with_encryption(pool.clone(), enc);

    let instance = make_instance("enc_create_test", Some("secret_jwt"), Some("secret_ca"));
    enc_repo.create(&instance).await.unwrap();

    // Verify the stored value is encrypted (not plaintext)
    let raw: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT jwt_secret FROM media_provider_instances WHERE name = $1",
    )
    .bind("enc_create_test")
    .fetch_optional(&pool)
    .await
    .unwrap();
    let raw_jwt = raw.unwrap().0.unwrap();
    assert!(
        raw_jwt.starts_with("enc:"),
        "Stored value should be encrypted, got: {}",
        &raw_jwt[..20.min(raw_jwt.len())]
    );

    // Read back with decryption
    let fetched = enc_repo.get_by_name("enc_create_test").await.unwrap().unwrap();
    assert_eq!(fetched.jwt_secret.as_deref(), Some("secret_jwt"));
    assert_eq!(fetched.custom_ca.as_deref(), Some("secret_ca"));
}
