//! `ProviderInstanceRepository` integration tests
//!
//! Tests: encryption of sensitive fields, CHECK constraints.
//!
//! Run with: cargo test -p synctv-core --test `provider_instance_repository_tests`
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use synctv_core::{
    models::ProviderInstance, repository::ProviderInstanceRepository, service::CredentialEncryption,
};
use synctv_core_testing::create_test_pool;
fn test_key() -> Vec<u8> {
    vec![
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ]
}

fn make_instance(
    name: &str,
    jwt_secret: Option<&str>,
    custom_ca: Option<&str>,
) -> ProviderInstance {
    let now = Utc::now();
    ProviderInstance {
        name: name.to_string(),
        endpoint: "http://localhost:50051".to_string(),
        comment: Some("test instance".to_string()),
        jwt_secret: jwt_secret.map(std::string::ToString::to_string),
        custom_ca: custom_ca.map(std::string::ToString::to_string),
        timeout: "10s".to_string(),
        tls: false,
        insecure_tls: false,
        providers: vec!["bilibili".to_string()],
        enabled: true,
        created_at: now,
        updated_at: now,
    }
}

// ─── CHECK constraint: plaintext jwt_secret rejected ─────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_plaintext_jwt_secret_rejected_by_check_constraint() {
    let (_container, pool) = create_test_pool().await;

    // Attempt to insert plaintext jwt_secret (without enc: prefix) should fail
    let result = sqlx::query(
        "INSERT INTO media_provider_instances (name, endpoint, jwt_secret, timeout, tls, insecure_tls, providers, enabled) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind("constraint_test_jwt")
    .bind("http://localhost:50051")
    .bind("plaintext_secret") // NOT enc: prefixed
    .bind("10s")
    .bind(false)
    .bind(false)
    .bind(&["bilibili"] as &[&str])
    .bind(true)
    .execute(&pool)
    .await;

    assert!(
        result.is_err(),
        "Plaintext jwt_secret should be rejected by CHECK constraint"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("valid_jwt_secret_format") || err_msg.contains("check"),
        "Error should mention CHECK constraint, got: {err_msg}"
    );
}

// ─── CHECK constraint: plaintext custom_ca rejected ───────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_plaintext_custom_ca_rejected_by_check_constraint() {
    let (_container, pool) = create_test_pool().await;

    // Attempt to insert plaintext custom_ca (without enc: prefix) should fail
    let result = sqlx::query(
        "INSERT INTO media_provider_instances (name, endpoint, custom_ca, timeout, tls, insecure_tls, providers, enabled) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind("constraint_test_ca")
    .bind("http://localhost:50051")
    .bind("-----BEGIN CERTIFICATE-----\nplaintext_cert\n-----END CERTIFICATE-----") // NOT enc: prefixed
    .bind("10s")
    .bind(false)
    .bind(false)
    .bind(&["bilibili"] as &[&str])
    .bind(true)
    .execute(&pool)
    .await;

    assert!(
        result.is_err(),
        "Plaintext custom_ca should be rejected by CHECK constraint"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("valid_custom_ca_format") || err_msg.contains("check"),
        "Error should mention CHECK constraint, got: {err_msg}"
    );
}

// ─── CHECK constraint: NULL secrets allowed ────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_null_secrets_allowed_by_check_constraint() {
    let (_container, pool) = create_test_pool().await;

    // NULL secrets should be allowed
    let result = sqlx::query(
        "INSERT INTO media_provider_instances (name, endpoint, jwt_secret, custom_ca, timeout, tls, insecure_tls, providers, enabled) \
         VALUES ($1, $2, NULL, NULL, $3, $4, $5, $6, $7)",
    )
    .bind("constraint_test_null")
    .bind("http://localhost:50051")
    .bind("10s")
    .bind(false)
    .bind(false)
    .bind(&["bilibili"] as &[&str])
    .bind(true)
    .execute(&pool)
    .await;

    assert!(
        result.is_ok(),
        "NULL secrets should be allowed, got error: {:?}",
        result.err()
    );
}

// ─── CHECK constraint: enc: prefixed secrets allowed ───────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_enc_prefixed_secrets_allowed_by_check_constraint() {
    let (_container, pool) = create_test_pool().await;

    // enc: prefixed secrets should be allowed
    let result = sqlx::query(
        "INSERT INTO media_provider_instances (name, endpoint, jwt_secret, custom_ca, timeout, tls, insecure_tls, providers, enabled) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind("constraint_test_enc")
    .bind("http://localhost:50051")
    .bind("enc:encrypted_jwt_secret_data")
    .bind("enc:encrypted_custom_ca_data")
    .bind("10s")
    .bind(false)
    .bind(false)
    .bind(&["bilibili"] as &[&str])
    .bind(true)
    .execute(&pool)
    .await;

    assert!(
        result.is_ok(),
        "enc: prefixed secrets should be allowed, got error: {:?}",
        result.err()
    );
}

// ─── create and read with encryption ─────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_and_read_with_encryption() {
    let (_container, pool) = create_test_pool().await;
    let enc = CredentialEncryption::new(&test_key()).unwrap();
    let enc_repo = ProviderInstanceRepository::new_with_encryption(pool.clone(), enc);

    let instance = make_instance("enc_create_test", Some("secret_jwt"), Some("secret_ca"));
    enc_repo.create(&instance).await.unwrap();

    // Verify the stored value is encrypted (not plaintext)
    let raw: Option<(Option<String>,)> =
        sqlx::query_as("SELECT jwt_secret FROM media_provider_instances WHERE name = $1")
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
    let fetched = enc_repo
        .get_by_name("enc_create_test")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fetched.jwt_secret.as_deref(), Some("secret_jwt"));
    assert_eq!(fetched.custom_ca.as_deref(), Some("secret_ca"));
}
