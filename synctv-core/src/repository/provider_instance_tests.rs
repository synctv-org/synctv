use super::*;
use crate::credential_encryption::CredentialEncryption;
use crate::models::{ProviderCredential, SortDirection};
use crate::test_helpers::{err, ok};

fn encrypt_test_credential(
    encryption: Option<&CredentialEncryption>,
    provider: &str,
    credential: &ProviderCredential,
) -> Result<super::StoredProviderCredential> {
    UserProviderCredentialRepository::encrypt_credential_with(
        encryption,
        UserId::expect_positive(1),
        provider,
        "test-server",
        credential,
    )
}

fn decrypt_test_credential(
    encryption: Option<&CredentialEncryption>,
    provider: &str,
    credential: &super::StoredProviderCredential,
) -> Result<ProviderCredential> {
    UserProviderCredentialRepository::decrypt_credential_with(
        encryption,
        UserId::expect_positive(1),
        provider,
        "test-server",
        credential,
    )
}

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
        ProviderInstanceRepository::decrypt_field_with(
            Some(&encryption),
            "test-instance",
            "jwt_secret",
            Some("plaintext-secret"),
        ),
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
        decrypt_test_credential(
            Some(&encryption),
            "bilibili",
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
        encrypt_test_credential(Some(&encryption), "alist", &credential),
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
        decrypt_test_credential(Some(&encryption), "alist", &stored),
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

#[test]
fn test_user_provider_credential_ciphertext_is_bound_to_binding() {
    let encryption = ok(CredentialEncryption::new(&[12u8; 32]), "encryption key");
    let credential = ProviderCredential::Alist {
        host: "https://alist.example.com".to_string(),
        username: "alice".to_string(),
        password: "secret-password".to_string(),
        otp_secret: None,
    };
    let stored = ok(
        encrypt_test_credential(Some(&encryption), "alist", &credential),
        "credential should encrypt",
    );

    let transplanted = UserProviderCredentialRepository::decrypt_credential_with(
        Some(&encryption),
        UserId::expect_positive(1),
        "alist",
        "different-server",
        &stored,
    );
    assert!(transplanted.is_err());
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
        encrypt_test_credential(Some(&encryption), "cloudreve", &credential),
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
        decrypt_test_credential(Some(&encryption), "cloudreve", &stored),
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
async fn test_user_provider_credential_storage_encrypts_twitch_session() {
    let encryption = ok(CredentialEncryption::new(&[17u8; 32]), "encryption key");
    let credential = ProviderCredential::Twitch {
        login: "synctv".to_string(),
        twitch_user_id: "123456".to_string(),
        client_id: "web-client".to_string(),
        scopes: vec!["user:read:follows".to_string()],
        auth_token: "oauth-token".to_string(),
        device_id: Some("device-id".to_string()),
        client_integrity: Some("integrity-token".to_string()),
    };

    let stored = ok(
        encrypt_test_credential(Some(&encryption), "twitch", &credential),
        "credential should encrypt",
    );
    let stored_json = ok(
        serde_json::to_value(&stored).map_err(crate::Error::from),
        "stored credential should serialize",
    );
    assert_eq!(stored_json["type"], "twitch");
    assert_eq!(stored_json["login"], "synctv");
    for field in ["authToken", "deviceId", "clientIntegrity"] {
        assert!(stored_json[field]
            .as_str()
            .is_some_and(|value| value.starts_with("enc:")));
    }

    let decrypted = ok(
        decrypt_test_credential(Some(&encryption), "twitch", &stored),
        "stored credential should decrypt",
    );
    let ProviderCredential::Twitch {
        login,
        twitch_user_id,
        client_id,
        scopes,
        auth_token,
        device_id,
        client_integrity,
    } = decrypted
    else {
        panic!("expected Twitch credential");
    };
    assert_eq!(login, "synctv");
    assert_eq!(client_id, "web-client");
    assert_eq!(scopes, ["user:read:follows"]);
    assert_eq!(twitch_user_id, "123456");
    assert_eq!(auth_token, "oauth-token");
    assert_eq!(device_id.as_deref(), Some("device-id"));
    assert_eq!(client_integrity.as_deref(), Some("integrity-token"));
}

#[tokio::test]
async fn test_user_provider_credential_storage_encrypts_qnap_secrets() {
    let encryption = ok(CredentialEncryption::new(&[19u8; 32]), "encryption key");
    let credential = ProviderCredential::Qnap {
        endpoint: "https://qnap.example.com".to_string(),
        username: "alice".to_string(),
        password: "secret-password".to_string(),
        sid: "session-id".to_string(),
        server_name: "QNAP".to_string(),
        version: Some("5.2".to_string()),
        support_rtt: true,
    };
    let stored = ok(
        encrypt_test_credential(Some(&encryption), "qnap", &credential),
        "credential should encrypt",
    );
    let json = ok(
        serde_json::to_value(&stored).map_err(crate::Error::from),
        "stored credential should serialize",
    );
    assert_eq!(json["type"], "qnap");
    for field in ["password", "sid"] {
        assert!(json[field]
            .as_str()
            .is_some_and(|value| value.starts_with("enc:")));
    }
    let decrypted = ok(
        decrypt_test_credential(Some(&encryption), "qnap", &stored),
        "stored credential should decrypt",
    );
    assert!(matches!(
        decrypted,
        ProviderCredential::Qnap {
            endpoint,
            username,
            password,
            sid,
            server_name,
            version,
            support_rtt: true,
        } if endpoint == "https://qnap.example.com"
            && username == "alice"
            && password == "secret-password"
            && sid == "session-id"
            && server_name == "QNAP"
            && version.as_deref() == Some("5.2")
    ));
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
        ProviderInstanceRepository::decrypt_field_with(
            None,
            "test-instance",
            "jwt_secret",
            Some("enc:v1:placeholder"),
        ),
        "encrypted field should require encryption",
    );
    assert!(
        err.to_string()
            .contains("Credential encryption must be configured"),
        "unexpected error: {err}"
    );
    assert_eq!(
        ok(
            ProviderInstanceRepository::decrypt_field_with(
                None,
                "test-instance",
                "jwt_secret",
                None,
            ),
            "empty field should decrypt",
        ),
        None
    );
    assert_eq!(
        ok(
            ProviderInstanceRepository::decrypt_field_with(
                None,
                "test-instance",
                "jwt_secret",
                Some(""),
            ),
            "blank field should decrypt",
        ),
        None
    );
}

#[tokio::test]
async fn test_user_provider_credential_repo_requires_encryption_for_storage() {
    let err = err(
        encrypt_test_credential(
            None,
            "alist",
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
        decrypt_test_credential(
            None,
            "bilibili",
            &super::StoredProviderCredential::Bilibili {
                cookies: super::EncryptedCredentialValue::from_string_for_test(
                    "enc:v1:placeholder",
                ),
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
