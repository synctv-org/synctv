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
use synctv_core::repository::UserProviderCredentialRepository;

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

/// Shared implementation for querying saved provider credentials ("binds").
///
/// Eliminates duplication across Alist, Emby, and Bilibili HTTP/gRPC handlers.
pub async fn get_provider_binds(
    repo: &Arc<UserProviderCredentialRepository>,
    user_id: &str,
    provider_name: &str,
    user_field_key: &str,
) -> Result<Vec<ProviderBind>, String> {
    let credentials = repo
        .get_by_user(user_id)
        .await
        .map_err(|e| format!("Failed to query credentials: {e}"))?;

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
                created_at_str: c.created_at.to_rfc3339(),
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
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}
