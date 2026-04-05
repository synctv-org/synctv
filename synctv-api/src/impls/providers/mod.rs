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

/// Shared implementation for querying saved provider credentials ("binds").
///
/// Eliminates duplication across Alist, Emby, and Bilibili HTTP/gRPC handlers.
pub async fn get_provider_binds(
    repo: &Arc<UserProviderCredentialRepository>,
    user_id: &str,
    provider_name: &str,
    user_field_key: &str,
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

    let binds = credentials
        .into_iter()
        .filter(|c| c.provider == provider_name)
        .map(|c| {
            let host = c
                .credential_data
                .get("host")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            let label_value = c
                .credential_data
                .get(user_field_key)
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            ProviderBind {
                id: c.id,
                host,
                label_key: user_field_key.to_string(),
                label_value,
                created_at: c.created_at.timestamp(),
                created_at_str: synctv_common::time::format_datetime_rfc3339(c.created_at),
            }
        })
        .collect();

    Ok(binds)
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
        extract_instance_name, get_provider_binds, resolve_bound_instance_name,
        PROVIDER_BINDS_UNAVAILABLE_MESSAGE,
    };
    use crate::impls::ApiError;
    use std::sync::Arc;
    use synctv_core::provider::ProviderError;
    use synctv_core::repository::UserProviderCredentialRepository;

    #[tokio::test]
    async fn get_provider_binds_backend_outage_maps_to_service_unavailable() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgresql://localhost:1/synctv")
            .expect("lazy pool");
        let repo = Arc::new(UserProviderCredentialRepository::new(pool));

        let err = get_provider_binds(&repo, "user-1", "alist", "username")
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
}
