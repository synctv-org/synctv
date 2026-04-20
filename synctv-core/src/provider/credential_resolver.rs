//! Credential Resolver
//!
//! Resolves provider credentials from the database using a `CredentialRef`.
//! This allows media `source_config` to store a reference to credentials
//! instead of raw credential data.

use serde::{Deserialize, Serialize};

use crate::models::ProviderCredential;
use crate::repository::UserProviderCredentialRepository;

use super::ExecutionControl;
use super::ProviderError;

/// Reference to stored credentials, used in `source_config` instead of raw credentials.
///
/// When media is created, the `credential_owner_id` is set to the creating user's ID.
/// When another user plays the media, the system resolves the creator's credentials
/// from the database using this reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialRef {
    /// User ID of the credential owner (media creator)
    pub credential_owner_id: String,
    /// Server identifier (SHA-256(host) for Emby/Alist, "bilibili" for Bilibili)
    pub server_id: String,
}

/// Resolve a typed `ProviderCredential` from a `CredentialRef` via DB lookup.
///
/// Looks up the credential in the database, checks expiry, decrypts if needed,
/// and returns the typed credential.
///
/// # Errors
///
/// Returns `ProviderError::CredentialNotFound` if the credential doesn't exist.
/// Returns `ProviderError::CredentialExpired` if the credential has expired.
pub async fn resolve_credential(
    repo: &UserProviderCredentialRepository,
    provider: &str,
    cred_ref: &CredentialRef,
    request_context: Option<&ExecutionControl>,
) -> Result<ProviderCredential, ProviderError> {
    if let Some(request_context) = request_context {
        request_context
            .check_active()
            .map_err(|err| ProviderError::NetworkError(err.to_string()))?;
    }

    let credential = repo
        .get_by_provider_and_server(&cred_ref.credential_owner_id, provider, &cred_ref.server_id)
        .await
        .map_err(|e| {
            ProviderError::Internal(format!("Failed to query credential from database: {e}"))
        })?
        .ok_or_else(|| {
            ProviderError::CredentialNotFound(format!(
                "No {provider} credential found for user '{}' with server_id '{}'",
                cred_ref.credential_owner_id, cred_ref.server_id
            ))
        })?;

    if let Some(request_context) = request_context {
        request_context
            .check_active()
            .map_err(|err| ProviderError::NetworkError(err.to_string()))?;
    }

    // Check expiry
    if credential.is_expired() {
        return Err(ProviderError::CredentialExpired(format!(
            "{provider} credential for user '{}' has expired",
            cred_ref.credential_owner_id
        )));
    }

    // Parse the credential data (already decrypted by repository)
    credential.get_credential().map_err(|e| {
        ProviderError::Internal(format!("Failed to parse {provider} credential data: {e}"))
    })
}
