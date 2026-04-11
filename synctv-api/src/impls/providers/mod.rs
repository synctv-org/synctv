//! Provider API Implementations
//!
//! Unified implementation for all provider API operations.
//! Used by both HTTP and gRPC handlers.

pub mod alist;
pub mod bilibili;
pub mod emby;

pub use alist::AlistApiImpl;
pub use bilibili::BilibiliApiImpl;
pub use emby::EmbyApiImpl;

use std::sync::Arc;
use synctv_core::models::UserProviderCredential;
use synctv_core::provider::ProviderError;
use synctv_core::repository::UserProviderCredentialRepository;

use crate::impls::ApiError;

/// Provider bind information returned by `get_binds`.
///
/// A generic representation of a saved provider credential, with two
/// user-identifying fields (`label_key`/`label_value`) whose meaning
/// varies by provider (e.g. "username" for Alist, "`user_id`" for Emby).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderBind {
    pub id: String,
    pub host: String,
    pub label_key: String,
    pub label_value: String,
    /// Unix epoch seconds
    pub created_at: i64,
    /// RFC 3339 formatted timestamp string (for HTTP JSON responses)
    pub created_at_str: String,
}

const PROVIDER_BINDS_UNAVAILABLE_MESSAGE: &str =
    "Provider bind information is temporarily unavailable";

fn filter_provider_binds(
    credentials: Vec<UserProviderCredential>,
    provider_name: &str,
    user_field_key: &str,
    instance_name: Option<&str>,
) -> Vec<ProviderBind> {
    let requested_instance_name = normalized_instance_name(instance_name);

    credentials
        .into_iter()
        .filter(|credential| credential.provider == provider_name)
        .filter(|credential| {
            requested_instance_name.is_none_or(|requested| {
                normalized_instance_name(credential.provider_instance_name.as_deref())
                    == Some(requested)
            })
        })
        .map(|credential| {
            let host = credential
                .credential_data
                .get("host")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            let label_value = credential
                .credential_data
                .get(user_field_key)
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            ProviderBind {
                id: credential.id,
                host,
                label_key: user_field_key.to_string(),
                label_value,
                created_at: credential.created_at.timestamp(),
                created_at_str: synctv_common::time::format_datetime_rfc3339(credential.created_at),
            }
        })
        .collect()
}

/// Shared implementation for querying saved provider credentials ("binds").
///
/// Eliminates duplication across Alist, Emby, and Bilibili HTTP/gRPC handlers.
pub async fn get_provider_binds(
    repo: &Arc<UserProviderCredentialRepository>,
    user_id: &str,
    provider_name: &str,
    user_field_key: &str,
    instance_name: Option<&str>,
) -> Result<Vec<ProviderBind>, ApiError> {
    let credentials = repo.get_by_user(user_id).await.map_err(|error| {
        tracing::error!(
            user_id,
            provider_name,
            error = %error,
            "Failed to query provider binds"
        );
        ApiError::ServiceUnavailable(PROVIDER_BINDS_UNAVAILABLE_MESSAGE.to_string())
    })?;

    Ok(filter_provider_binds(
        credentials,
        provider_name,
        user_field_key,
        instance_name,
    ))
}

/// Extract `instance_name` from a request field: empty string maps to `None`.
///
/// Eliminates the repetitive 5-line block duplicated across all gRPC provider methods.
#[must_use]
pub fn extract_instance_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn normalized_instance_name(name: Option<&str>) -> Option<&str> {
    name.and_then(|name| {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

/// Resolve the effective provider instance for a request against a stored credential.
///
/// Stored credentials are authoritative: once a credential is bound to an instance,
/// the client must not override that binding via query/request parameters.
pub(crate) fn resolve_bound_instance_name(
    requested_instance_name: Option<&str>,
    credential_instance_name: Option<&str>,
) -> Result<Option<String>, ProviderError> {
    let requested = normalized_instance_name(requested_instance_name);
    let credential = normalized_instance_name(credential_instance_name);

    match (requested, credential) {
        (Some(requested), Some(credential)) if requested != credential => Err(
            ProviderError::InvalidConfig(format!(
                "Stored credential is bound to provider instance '{credential}', but request specified '{requested}'"
            )),
        ),
        (_, Some(credential)) => Ok(Some(credential.to_string())),
        (Some(requested), None) => Err(ProviderError::InvalidConfig(format!(
            "Stored credential is not bound to a provider instance; omit instance_name '{requested}' and log in again if you need an instance-scoped credential"
        ))),
        (None, None) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        extract_instance_name, filter_provider_binds, get_provider_binds,
        resolve_bound_instance_name, PROVIDER_BINDS_UNAVAILABLE_MESSAGE,
    };
    use crate::impls::ApiError;
    use chrono::Utc;
    use serde_json::json;
    use std::sync::Arc;
    use synctv_core::models::UserProviderCredential;
    use synctv_core::provider::ProviderError;
    use synctv_core::repository::UserProviderCredentialRepository;

    fn sample_credential(
        id: &str,
        provider: &str,
        provider_instance_name: Option<&str>,
        host: &str,
        label_key: &str,
        label_value: &str,
    ) -> UserProviderCredential {
        UserProviderCredential {
            id: id.to_string(),
            user_id: "user-1".to_string(),
            provider: provider.to_string(),
            server_id: format!("srv-{id}"),
            provider_instance_name: provider_instance_name.map(ToString::to_string),
            credential_data: json!({
                "host": host,
                label_key: label_value,
            }),
            expires_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn get_provider_binds_backend_outage_maps_to_service_unavailable() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgresql://localhost:1/synctv")
            .expect("lazy pool");
        let repo = Arc::new(UserProviderCredentialRepository::new(pool));

        let err = get_provider_binds(&repo, "user-1", "alist", "username", None)
            .await
            .expect_err("bind query should fail");

        match err {
            ApiError::ServiceUnavailable(message) => {
                assert_eq!(message, PROVIDER_BINDS_UNAVAILABLE_MESSAGE);
            }
            other => panic!("expected ServiceUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn resolve_bound_instance_name_uses_credential_binding_when_request_omits_instance() {
        let resolved = resolve_bound_instance_name(None, Some("emby-main")).unwrap();
        assert_eq!(resolved.as_deref(), Some("emby-main"));
    }

    #[test]
    fn resolve_bound_instance_name_rejects_mismatched_request_instance() {
        let err = resolve_bound_instance_name(Some("emby-backup"), Some("emby-main")).unwrap_err();
        match err {
            ProviderError::InvalidConfig(msg) => {
                assert!(msg.contains("emby-main"));
                assert!(msg.contains("emby-backup"));
            }
            other => panic!("expected InvalidConfig, got {other:?}"),
        }
    }

    #[test]
    fn resolve_bound_instance_name_rejects_named_request_for_unbound_credential() {
        let err = resolve_bound_instance_name(Some("bili-main"), None).unwrap_err();
        match err {
            ProviderError::InvalidConfig(msg) => {
                assert!(msg.contains("not bound"));
                assert!(msg.contains("bili-main"));
            }
            other => panic!("expected InvalidConfig, got {other:?}"),
        }
    }

    #[test]
    fn resolve_bound_instance_name_treats_blank_names_as_absent() {
        let resolved = resolve_bound_instance_name(Some("   "), Some("")).unwrap();
        assert!(resolved.is_none());
    }

    #[test]
    fn extract_instance_name_treats_blank_names_as_absent() {
        assert_eq!(extract_instance_name(""), None);
        assert_eq!(extract_instance_name("   "), None);
        assert_eq!(
            extract_instance_name("  emby-main  "),
            Some("emby-main".to_string())
        );
    }

    #[test]
    fn filter_provider_binds_scopes_to_requested_instance() {
        let binds = filter_provider_binds(
            vec![
                sample_credential(
                    "cred-1",
                    "emby",
                    Some("emby-main"),
                    "https://main.example.com",
                    "emby_user_id",
                    "main-user",
                ),
                sample_credential(
                    "cred-2",
                    "emby",
                    Some("emby-backup"),
                    "https://backup.example.com",
                    "emby_user_id",
                    "backup-user",
                ),
                sample_credential(
                    "cred-3",
                    "alist",
                    Some("alist-main"),
                    "https://alist.example.com",
                    "username",
                    "alist-user",
                ),
            ],
            "emby",
            "emby_user_id",
            Some("emby-main"),
        );

        assert_eq!(binds.len(), 1);
        assert_eq!(binds[0].id, "cred-1");
        assert_eq!(binds[0].host, "https://main.example.com");
        assert_eq!(binds[0].label_value, "main-user");
    }

    #[test]
    fn filter_provider_binds_returns_all_instances_when_request_omits_instance() {
        let binds = filter_provider_binds(
            vec![
                sample_credential(
                    "cred-1",
                    "alist",
                    Some("alist-main"),
                    "https://main.example.com",
                    "username",
                    "main-user",
                ),
                sample_credential(
                    "cred-2",
                    "alist",
                    Some("alist-backup"),
                    "https://backup.example.com",
                    "username",
                    "backup-user",
                ),
            ],
            "alist",
            "username",
            None,
        );

        assert_eq!(binds.len(), 2);
    }
}
