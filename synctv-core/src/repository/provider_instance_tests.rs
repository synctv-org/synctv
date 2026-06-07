use super::*;
use crate::credential_encryption::CredentialEncryption;
use crate::models::SortDirection;
use serde_json::json;

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
    let encryption = CredentialEncryption::new(&[7u8; 32]).unwrap();

    let err =
        ProviderInstanceRepository::decrypt_field_with(Some(&encryption), Some("plaintext-secret"))
            .unwrap_err();
    assert!(
        err.to_string().contains("plaintext sensitive data"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_user_provider_credential_repo_rejects_plaintext_json_when_encryption_enabled() {
    let encryption = CredentialEncryption::new(&[9u8; 32]).unwrap();

    let err = UserProviderCredentialRepository::decrypt_credential_with(
        Some(&encryption),
        &json!({"token": "plaintext"}),
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("Credential value must be an encrypted string"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_provider_instance_repo_requires_encryption_when_sensitive_fields_present() {
    let err = ProviderInstanceRepository::ensure_encryption_for_sensitive_fields_with(
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
            providers: vec!["alist".to_string()],
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        },
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("Credential encryption must be configured"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_provider_instance_repo_requires_encryption_for_sensitive_reads() {
    let err =
        ProviderInstanceRepository::decrypt_field_with(None, Some("enc:placeholder")).unwrap_err();
    assert!(
        err.to_string()
            .contains("Credential encryption must be configured"),
        "unexpected error: {err}"
    );
    assert_eq!(
        ProviderInstanceRepository::decrypt_field_with(None, None).unwrap(),
        None
    );
    assert_eq!(
        ProviderInstanceRepository::decrypt_field_with(None, Some("")).unwrap(),
        None
    );
}

#[tokio::test]
async fn test_user_provider_credential_repo_requires_encryption_for_storage() {
    let err = UserProviderCredentialRepository::encrypt_credential_with(
        None,
        &json!({"token": "plaintext"}),
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("Credential encryption must be configured"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_user_provider_credential_repo_requires_encryption_for_reads() {
    let err =
        UserProviderCredentialRepository::decrypt_credential_with(None, &json!("enc:placeholder"))
            .unwrap_err();
    assert!(
        err.to_string()
            .contains("Credential encryption must be configured"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_normalize_provider_instance_name_for_db() {
    assert_eq!(
        UserProviderCredentialRepository::normalize_provider_instance_name_for_db(None),
        None
    );
    assert_eq!(
        UserProviderCredentialRepository::normalize_provider_instance_name_for_db(Some("")),
        None
    );
    assert_eq!(
        UserProviderCredentialRepository::normalize_provider_instance_name_for_db(Some("   ")),
        None
    );
    assert_eq!(
        UserProviderCredentialRepository::normalize_provider_instance_name_for_db(Some(
            "alist-main"
        )),
        Some("alist-main")
    );
    assert_eq!(
        UserProviderCredentialRepository::normalize_provider_instance_name_for_db(Some(
            "  alist-main  "
        )),
        Some("alist-main")
    );
}
