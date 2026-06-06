use super::*;
use crate::credential_encryption::CredentialEncryption;
use crate::models::SortDirection;
use serde_json::json;

// Note: These are unit tests for the repository structure.
// Integration tests with actual database should be in tests/ directory.

fn order_by_sql(query: &ProviderInstanceListQuery) -> String {
    let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("");
    ProviderInstanceRepository::push_list_order_by(&mut builder, query);
    builder.sql().to_string()
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
    let pool = PgPool::connect_lazy("postgresql://test").unwrap();
    let encryption = CredentialEncryption::new(&[7u8; 32]).unwrap();
    let repo = ProviderInstanceRepository::new_with_encryption(pool, encryption);

    let err = repo.decrypt_field(Some("plaintext-secret")).unwrap_err();
    assert!(
        err.to_string().contains("plaintext sensitive data"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_user_provider_credential_repo_rejects_plaintext_json_when_encryption_enabled() {
    let pool = PgPool::connect_lazy("postgresql://test").unwrap();
    let encryption = CredentialEncryption::new(&[9u8; 32]).unwrap();
    let repo = UserProviderCredentialRepository::new_with_encryption(pool, encryption);

    let err = repo
        .decrypt_credential(&json!({"token": "plaintext"}))
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("Credential value must be an encrypted string"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_provider_instance_repo_requires_encryption_when_sensitive_fields_present() {
    let pool = PgPool::connect_lazy("postgresql://test").unwrap();
    let repo = ProviderInstanceRepository::new(pool);

    let err = repo
        .ensure_encryption_for_sensitive_fields(&ProviderInstance {
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
        })
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("Credential encryption must be configured"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_provider_instance_repo_requires_encryption_for_sensitive_reads() {
    let pool = PgPool::connect_lazy("postgresql://test").unwrap();
    let repo = ProviderInstanceRepository::new(pool);

    let err = repo.decrypt_field(Some("enc:placeholder")).unwrap_err();
    assert!(
        err.to_string()
            .contains("Credential encryption must be configured"),
        "unexpected error: {err}"
    );
    assert_eq!(repo.decrypt_field(None).unwrap(), None);
    assert_eq!(repo.decrypt_field(Some("")).unwrap(), None);
}

#[tokio::test]
async fn test_user_provider_credential_repo_requires_encryption_for_storage() {
    let pool = PgPool::connect_lazy("postgresql://test").unwrap();
    let repo = UserProviderCredentialRepository::new(pool);

    let err = repo
        .encrypt_credential(&json!({"token": "plaintext"}))
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("Credential encryption must be configured"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_user_provider_credential_repo_requires_encryption_for_reads() {
    let pool = PgPool::connect_lazy("postgresql://test").unwrap();
    let repo = UserProviderCredentialRepository::new(pool);

    let err = repo
        .decrypt_credential(&json!("enc:placeholder"))
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
