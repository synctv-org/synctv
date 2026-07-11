use super::*;
use crate::credential_encryption::CredentialEncryption;
use crate::models::{ProviderCredential, SortDirection};
use crate::test_helpers::{err, ok};

fn order_by_sql(query: &ProviderInstanceListQuery) -> String {
    let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("");
    ProviderInstanceRepository::push_list_order_by(&mut builder, query);
    builder.sql().as_str().to_string()
}

#[test]
fn test_provider_instance_list_select_columns_are_explicit() {
    assert_eq!(
        ProviderInstanceRepository::INSTANCE_SELECT_COLUMNS,
        "name, endpoint, comment, jwt_secret, custom_ca, timeout, tls, insecure_tls, providers, enabled, created_at, updated_at"
    );
}

#[test]
fn test_provider_instance_list_order_by_uses_static_sort_branches() {
    let mut query = ProviderInstanceListQuery {
        sort_by: ProviderInstanceListSortBy::Name,
        sort_direction: SortDirection::Asc,
        ..ProviderInstanceListQuery::default()
    };
    assert_eq!(order_by_sql(&query), " ORDER BY name ASC, created_at ASC");

    query.sort_by = ProviderInstanceListSortBy::Endpoint;
    query.sort_direction = SortDirection::Desc;
    assert_eq!(
        order_by_sql(&query),
        " ORDER BY endpoint DESC, created_at DESC"
    );

    query.sort_by = ProviderInstanceListSortBy::UpdatedAt;
    query.sort_direction = SortDirection::Asc;
    assert_eq!(order_by_sql(&query), " ORDER BY updated_at ASC, name ASC");

    query.sort_by = ProviderInstanceListSortBy::CreatedAt;
    query.sort_direction = SortDirection::Desc;
    assert_eq!(order_by_sql(&query), " ORDER BY created_at DESC, name DESC");
}

#[tokio::test]
async fn test_provider_instance_repo_rejects_plaintext_sensitive_fields_when_encryption_enabled() {
    let encryption = ok(CredentialEncryption::new(&[7u8; 32]), "encryption key");

    let err = err(
        ProviderInstanceRepository::decrypt_field_with(Some(&encryption), Some("plaintext-secret")),
        "plaintext secret should fail",
    );
    assert!(
        err.to_string().contains("plaintext sensitive data"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_user_provider_credential_repo_rejects_plaintext_json_when_encryption_enabled() {
    let encryption = ok(CredentialEncryption::new(&[9u8; 32]), "encryption key");

    let err = err(
        UserProviderCredentialRepository::decrypt_credential_with(
            Some(&encryption),
            &super::StoredProviderCredential::Bilibili {
                cookies: super::EncryptedCredentialValue::from_string_for_test(
                    r#"{"SESSDATA":"plaintext"}"#,
                ),
            },
        ),
        "plaintext credential should fail",
    );
    assert!(
        err.to_string()
            .contains("Credential data must be an encrypted string"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_user_provider_credential_storage_keeps_alist_metadata_plaintext() {
    let encryption = ok(CredentialEncryption::new(&[11u8; 32]), "encryption key");
    let credential = ProviderCredential::Alist {
        host: "https://alist.example.com".to_string(),
        username: "alice".to_string(),
        password: "secret-password".to_string(),
        otp_secret: Some("otp-secret".to_string()),
    };

    let stored = ok(
        UserProviderCredentialRepository::encrypt_credential_with(Some(&encryption), &credential),
        "credential should encrypt",
    );
    let stored_json = ok(
        serde_json::to_value(&stored).map_err(crate::Error::from),
        "stored credential should serialize",
    );

    assert_eq!(stored_json["type"], "alist");
    assert_eq!(stored_json["host"], "https://alist.example.com");
    assert_eq!(stored_json["username"], "alice");
    assert!(stored_json["password"]
        .as_str()
        .is_some_and(|value| value.starts_with("enc:")));
    assert!(stored_json["otpSecret"]
        .as_str()
        .is_some_and(|value| value.starts_with("enc:")));

    let decrypted = ok(
        UserProviderCredentialRepository::decrypt_credential_with(Some(&encryption), &stored),
        "stored credential should decrypt",
    );
    let ProviderCredential::Alist {
        host,
        username,
        password,
        otp_secret,
    } = decrypted
    else {
        panic!("expected alist credential");
    };
    assert_eq!(host, "https://alist.example.com");
    assert_eq!(username, "alice");
    assert_eq!(password, "secret-password");
    assert_eq!(otp_secret.as_deref(), Some("otp-secret"));
}

#[tokio::test]
async fn test_user_provider_credential_storage_encrypts_cloudreve_password() {
    let encryption = ok(CredentialEncryption::new(&[13u8; 32]), "encryption key");
    let credential = ProviderCredential::Cloudreve {
        host: "https://cloudreve.example.com".to_string(),
        email: "alice@example.com".to_string(),
        password: "secret-password".to_string(),
    };

    let stored = ok(
        UserProviderCredentialRepository::encrypt_credential_with(Some(&encryption), &credential),
        "credential should encrypt",
    );
    let stored_json = ok(
        serde_json::to_value(&stored).map_err(crate::Error::from),
        "stored credential should serialize",
    );

    assert_eq!(stored_json["type"], "cloudreve");
    assert_eq!(stored_json["host"], "https://cloudreve.example.com");
    assert_eq!(stored_json["email"], "alice@example.com");
    assert!(stored_json["password"]
        .as_str()
        .is_some_and(|value| value.starts_with("enc:")));
    assert_ne!(stored_json["password"], "secret-password");

    let decrypted = ok(
        UserProviderCredentialRepository::decrypt_credential_with(Some(&encryption), &stored),
        "stored credential should decrypt",
    );
    let ProviderCredential::Cloudreve {
        host,
        email,
        password,
    } = decrypted
    else {
        panic!("expected cloudreve credential");
    };
    assert_eq!(host, "https://cloudreve.example.com");
    assert_eq!(email, "alice@example.com");
    assert_eq!(password, "secret-password");
}

#[tokio::test]
async fn test_provider_instance_repo_requires_encryption_when_sensitive_fields_present() {
    let err = err(
        ProviderInstanceRepository::ensure_encryption_for_sensitive_fields_with(
            None,
            &ProviderInstance {
                name: "remote".to_string(),
                endpoint: "http://remote.example.com:50051".to_string(),
                comment: None,
                jwt_secret: Some("secret".to_string()),
                custom_ca: None,
                timeout: "10s".to_string(),
                tls: false,
                insecure_tls: false,
                providers: vec![SourceProvider::Alist],
                enabled: true,
                created_at: crate::SystemClock.now(),
                updated_at: crate::SystemClock.now(),
            },
        ),
        "sensitive fields should require encryption",
    );
    assert!(
        err.to_string()
            .contains("Credential encryption must be configured"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_provider_instance_repo_requires_encryption_for_sensitive_reads() {
    let err = err(
        ProviderInstanceRepository::decrypt_field_with(None, Some("enc:placeholder")),
        "encrypted field should require encryption",
    );
    assert!(
        err.to_string()
            .contains("Credential encryption must be configured"),
        "unexpected error: {err}"
    );
    assert_eq!(
        ok(
            ProviderInstanceRepository::decrypt_field_with(None, None),
            "empty field should decrypt",
        ),
        None
    );
    assert_eq!(
        ok(
            ProviderInstanceRepository::decrypt_field_with(None, Some("")),
            "blank field should decrypt",
        ),
        None
    );
}

#[tokio::test]
async fn test_user_provider_credential_repo_requires_encryption_for_storage() {
    let err = err(
        UserProviderCredentialRepository::encrypt_credential_with(
            None,
            &ProviderCredential::Alist {
                host: "https://alist.example.com".to_string(),
                username: "user".to_string(),
                password: "plaintext".to_string(),
                otp_secret: None,
            },
        ),
        "credential storage should require encryption",
    );
    assert!(
        err.to_string()
            .contains("Credential encryption must be configured"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_user_provider_credential_repo_requires_encryption_for_reads() {
    let err = err(
        UserProviderCredentialRepository::decrypt_credential_with(
            None,
            &super::StoredProviderCredential::Bilibili {
                cookies: super::EncryptedCredentialValue::from_string_for_test("enc:placeholder"),
            },
        ),
        "encrypted credential should require encryption",
    );
    assert!(
        err.to_string()
            .contains("Credential encryption must be configured"),
        "unexpected error: {err}"
    );
}
