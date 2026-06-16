//! `ProviderInstanceRepository` integration tests
//!
//! Tests: encryption of sensitive fields and relational constraints.
//!

use chrono::Utc;
use synctv_core::{
    credential_encryption::CredentialEncryption,
    models::{ProviderInstance, ProviderType, SignupMethod},
    repository::ProviderInstanceRepository,
    Error,
};
use synctv_core_testing::{create_test_pool, create_test_pool_with_db_and_label, err, ok, some};

fn provider_codes(providers: &[ProviderType]) -> Vec<i16> {
    providers
        .iter()
        .copied()
        .map(ProviderType::as_i16)
        .collect()
}

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

// ─── Schema remains storage-only; encryption policy lives in repository ───

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_plaintext_jwt_secret_is_not_a_schema_policy() {
    let (_container, pool) = create_test_pool().await;

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
    .bind(provider_codes(&[ProviderType::Bilibili]))
    .bind(true)
    .execute(&pool)
    .await;

    assert!(
        result.is_ok(),
        "storage schema should not encode credential encryption format policy, got: {:?}",
        result.err()
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_plaintext_custom_ca_is_not_a_schema_policy() {
    let (_container, pool) = create_test_pool().await;

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
    .bind(provider_codes(&[ProviderType::Bilibili]))
    .bind(true)
    .execute(&pool)
    .await;

    assert!(
        result.is_ok(),
        "storage schema should not encode credential encryption format policy, got: {:?}",
        result.err()
    );
}

// ─── NULL secrets are storage-valid ─────────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_null_secrets_allowed_by_schema() {
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
    .bind(provider_codes(&[ProviderType::Bilibili]))
    .bind(true)
    .execute(&pool)
    .await;

    assert!(
        result.is_ok(),
        "NULL secrets should be allowed, got error: {:?}",
        result.err()
    );
}

// ─── Encrypted secrets are storage-valid ────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_enc_prefixed_secrets_allowed_by_schema() {
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
    .bind(provider_codes(&[ProviderType::Bilibili]))
    .bind(true)
    .execute(&pool)
    .await;

    assert!(
        result.is_ok(),
        "enc: prefixed secrets should be allowed, got error: {:?}",
        result.err()
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_referenced_provider_instance_is_rejected() {
    let (_container, pool) = create_test_pool().await;
    let repo = ProviderInstanceRepository::new(pool.clone());
    let instance = make_instance("referenced_instance", None, None);
    ok(
        repo.create(&instance).await,
        "referenced provider instance should be created",
    );

    let user_id: i64 = ok(
        sqlx::query_scalar(
            "INSERT INTO users (username, signup_method, role) VALUES ($1, $2, 3) RETURNING id",
        )
        .bind("provider_ref_owner")
        .bind(i16::from(SignupMethod::Email))
        .fetch_one(&pool)
        .await,
        "provider reference owner should be inserted",
    );
    let room_id: i64 = ok(
        sqlx::query_scalar("INSERT INTO rooms (name, created_by) VALUES ($1, $2) RETURNING id")
            .bind("Provider Ref Room")
            .bind(user_id)
            .fetch_one(&pool)
            .await,
        "provider reference room should be inserted",
    );
    ok(
        sqlx::query(
            "INSERT INTO playlists (room_id, creator_id, name, position, source_provider, source_config, provider_instance_name) \
         VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7)",
        )
        .bind(room_id)
        .bind(user_id)
        .bind("Remote Folder")
        .bind(1.0_f64)
        .bind(ProviderType::Bilibili.as_i16())
        .bind("{}")
        .bind("referenced_instance")
        .execute(&pool)
        .await,
        "referencing playlist should be inserted",
    );

    let error = err(
        repo.delete("referenced_instance").await,
        "referenced provider instances should be rejected during delete",
    );
    assert!(
        matches!(&error, Error::InvalidInput(message) if message.contains("still referenced")),
        "expected referenced instance delete to be rejected clearly, got: {error}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_enabled_connection_inputs_read_from_primary_pool() {
    let (_primary_container, primary_pool) =
        create_test_pool_with_db_and_label("synctv_test", "provider-primary").await;
    let (_read_container, read_pool) =
        create_test_pool_with_db_and_label("synctv_test", "provider-read").await;

    let primary_repo = ProviderInstanceRepository::new(primary_pool.clone());
    let read_repo = ProviderInstanceRepository::new(read_pool.clone());
    ok(
        primary_repo
            .create(&make_instance("primary_enabled_instance", None, None))
            .await,
        "primary provider instance should be created",
    );
    ok(
        read_repo
            .create(&make_instance("stale_read_instance", None, None))
            .await,
        "stale read provider instance should be created",
    );

    let repo = ProviderInstanceRepository::new_with_read_pool(primary_pool.clone(), read_pool);
    let enabled = ok(
        repo.get_all_enabled().await,
        "enabled provider instances should be loaded",
    );
    let names: Vec<_> = enabled.into_iter().map(|instance| instance.name).collect();
    assert_eq!(names, vec!["primary_enabled_instance".to_string()]);

    primary_pool.close().await;
}

// ─── create and read with encryption ─────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_and_read_with_encryption() {
    let (_container, pool) = create_test_pool().await;
    let enc = ok(
        CredentialEncryption::new(&test_key()),
        "test credential encryption should initialize",
    );
    let enc_repo = ProviderInstanceRepository::new_with_encryption(pool.clone(), enc);

    let instance = make_instance("enc_create_test", Some("secret_jwt"), Some("secret_ca"));
    ok(
        enc_repo.create(&instance).await,
        "encrypted provider instance should be created",
    );

    // Verify the stored value is encrypted (not plaintext)
    let raw: Option<(Option<String>,)> = ok(
        sqlx::query_as("SELECT jwt_secret FROM media_provider_instances WHERE name = $1")
            .bind("enc_create_test")
            .fetch_optional(&pool)
            .await,
        "encrypted provider instance row should be queried",
    );
    let raw_jwt = some(
        some(raw, "encrypted provider instance row should exist").0,
        "encrypted JWT secret should exist",
    );
    assert!(
        raw_jwt.starts_with("enc:"),
        "Stored value should be encrypted, got: {}",
        &raw_jwt[..20.min(raw_jwt.len())]
    );

    // Read back with decryption
    let fetched = ok(
        enc_repo.get_by_name("enc_create_test").await,
        "encrypted provider instance should be fetched",
    );
    let fetched = some(fetched, "encrypted provider instance should exist");
    assert_eq!(fetched.jwt_secret.as_deref(), Some("secret_jwt"));
    assert_eq!(fetched.custom_ca.as_deref(), Some("secret_ca"));
}
