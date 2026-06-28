//! Credential Resolver
//!
//! Resolves provider credentials from the database without storing credential
//! owner references in media `source_config`.

use crate::models::{ProviderCredential, UserId};
use crate::repository::UserProviderCredentialRepository;

use super::ExecutionControl;
use super::ProviderError;

#[derive(Debug, Clone)]
pub struct ResolvedProviderCredential {
    pub credential: ProviderCredential,
    pub id: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// Opaque credential version suitable for cache partitioning.
    ///
    /// The row id protects against same-timestamp replacements and the
    /// microsecond timestamp changes on normal updates.
    pub revision: String,
}

#[must_use]
pub fn credential_revision(id: i64, updated_at: chrono::DateTime<chrono::Utc>) -> String {
    format!("{id}:{}", updated_at.timestamp_micros())
}

/// Resolve a typed `ProviderCredential` by owner and server id via DB lookup.
///
/// Looks up the credential in the database, checks expiry, decrypts if needed,
/// and returns the typed credential.
///
/// # Errors
///
/// Returns `ProviderError::CredentialNotFound` if the credential doesn't exist.
/// Returns `ProviderError::CredentialExpired` if the credential has expired.
pub async fn resolve_credential_for_owner(
    repo: &UserProviderCredentialRepository,
    provider: &str,
    credential_owner_id: UserId,
    server_id: &str,
    request_context: Option<&ExecutionControl>,
) -> Result<ProviderCredential, ProviderError> {
    resolve_credential_record_for_owner(
        repo,
        provider,
        credential_owner_id,
        server_id,
        request_context,
    )
    .await
    .map(|resolved| resolved.credential)
}

/// Resolve a typed credential plus metadata needed for cache partitioning.
pub async fn resolve_credential_record_for_owner(
    repo: &UserProviderCredentialRepository,
    provider: &str,
    credential_owner_id: UserId,
    server_id: &str,
    request_context: Option<&ExecutionControl>,
) -> Result<ResolvedProviderCredential, ProviderError> {
    if let Some(request_context) = request_context {
        request_context
            .check_active()
            .map_err(|err| ProviderError::NetworkError(err.to_string()))?;
    }

    let credential_record = repo
        .get_by_provider_and_server(credential_owner_id, provider, server_id)
        .await
        .map_err(|e| {
            ProviderError::Internal(format!("Failed to query credential from database: {e}"))
        })?
        .ok_or_else(|| {
            ProviderError::CredentialNotFound(format!(
                "No {provider} credential found for user '{credential_owner_id}' with server_id '{server_id}'"
            ))
        })?;

    if let Some(request_context) = request_context {
        request_context
            .check_active()
            .map_err(|err| ProviderError::NetworkError(err.to_string()))?;
    }

    // Check expiry
    if credential_record.is_expired() {
        return Err(ProviderError::CredentialExpired(format!(
            "{provider} credential for user '{credential_owner_id}' has expired"
        )));
    }

    let credential = credential_record.credential_data;

    Ok(ResolvedProviderCredential {
        credential,
        id: credential_record.id.to_string(),
        updated_at: credential_record.updated_at,
        revision: credential_revision(credential_record.id, credential_record.updated_at),
    })
}

#[cfg(test)]
mod tests {
    use crate::test_helpers::some;

    use chrono::{TimeZone, Utc};

    #[test]
    fn credential_revision_includes_row_id_and_microsecond_timestamp() {
        let updated_at = some(
            Utc.timestamp_opt(1_700_000_000, 123_456_000).single(),
            "valid timestamp",
        );

        assert_eq!(
            super::credential_revision(42, updated_at),
            "42:1700000000123456"
        );
    }
}
