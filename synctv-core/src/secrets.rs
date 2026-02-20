//! Secure secrets management for production deployments
//!
//! This module provides a secure way to load secrets from various sources
//! with proper validation and minimal exposure risk.
//!
//! # Supported Sources
//!
//! 1. **Files** (recommended for Kubernetes/Docker secrets):
//!    - Secrets mounted as files in `/run/secrets/` or custom paths
//!    - Example: `/run/secrets/jwt_private_key`, `/run/secrets/database_password`
//!
//! 2. **Environment Variables** (fallback):
//!    - `DATABASE_PASSWORD`, `SMTP_PASSWORD`, etc.
//!    - ⚠️ Less secure as visible in process list and container inspect
//!
//! # Best Practices
//!
//! - **Kubernetes**: Use Secret resources mounted as files
//! - **Docker**: Use Docker secrets or bind-mount secret files
//! - **Never**: Commit secrets to git, include in images, or log them
//!
//! # Example Usage
//!
//! ```rust,ignore
//! use synctv_core::secrets::{SecretLoader, SecretSource};
//!
//! // Load database password from file (Kubernetes secret)
//! let db_password = SecretLoader::load(
//!     "database_password",
//!     SecretSource::File("/run/secrets/database_password")
//! )?;
//!
//! // Load SMTP password with fallback to environment variable
//! let smtp_password = SecretLoader::load_with_fallback(
//!     "smtp_password",
//!     SecretSource::File("/run/secrets/smtp_password"),
//!     SecretSource::Env("SMTP_PASSWORD")
//! )?;
//! ```

use std::fs;
use std::path::Path;
use anyhow::Context;
use tracing::{debug, warn};

use crate::Result;

/// Source for loading secrets
#[derive(Debug, Clone)]
pub enum SecretSource {
    /// Load secret from a file path
    File(&'static str),
    /// Load secret from an environment variable
    Env(&'static str),
    /// Load secret from a string (for testing only)
    #[cfg(test)]
    Direct(String),
}

/// Secure secret loader with multiple source support
pub struct SecretLoader;

impl SecretLoader {
    /// Load a secret from a specified source
    ///
    /// # Arguments
    /// * `name` - Human-readable name for logging (never logged with value)
    /// * `source` - Source to load the secret from
    ///
    /// # Returns
    /// The secret value as a String, or an error if not found
    ///
    /// # Security
    /// - Secret values are NEVER logged
    /// - Only secret names and sources are logged
    /// - Fails fast if secret is not found
    pub fn load(name: &str, source: SecretSource) -> Result<String> {
        match source {
            SecretSource::File(path) => {
                debug!(secret_name = name, source = "file", path = path, "Loading secret from file");
                let content = fs::read_to_string(path)
                    .with_context(|| format!("Failed to read secret '{name}' from file '{path}'"))?;

                let trimmed = content.trim().to_string();

                if trimmed.is_empty() {
                    return Err(crate::Error::Internal(
                        format!("Secret '{name}' from file '{path}' is empty"),
                    ));
                }

                debug!(secret_name = name, secret_len = trimmed.len(), "Secret loaded successfully from file");
                Ok(trimmed)
            }
            SecretSource::Env(env_var) => {
                debug!(secret_name = name, source = "env", env_var = env_var, "Loading secret from environment");
                warn!(
                    secret_name = name,
                    env_var = env_var,
                    "Loading secret from environment variable (less secure than file-based secrets)"
                );

                let value = std::env::var(env_var)
                    .with_context(|| format!("Failed to read secret '{name}' from environment variable '{env_var}'"))?;

                if value.is_empty() {
                    return Err(crate::Error::Internal(
                        format!("Secret '{name}' from environment variable '{env_var}' is empty"),
                    ));
                }

                debug!(secret_name = name, secret_len = value.len(), "Secret loaded successfully from environment");
                Ok(value)
            }
            #[cfg(test)]
            SecretSource::Direct(value) => Ok(value),
        }
    }

    /// Load a secret with fallback sources
    ///
    /// Attempts to load from the primary source first, then falls back to secondary source.
    /// This is useful for development/testing environments where file-based secrets may not be available.
    ///
    /// # Arguments
    /// * `name` - Human-readable name for logging
    /// * `primary` - Primary source (e.g., file)
    /// * `fallback` - Fallback source (e.g., environment variable)
    ///
    /// # Returns
    /// The secret value, or an error if not found in either source
    pub fn load_with_fallback(name: &str, primary: SecretSource, fallback: SecretSource) -> Result<String> {
        match Self::load(name, primary) {
            Ok(secret) => Ok(secret),
            Err(primary_err) => {
                debug!(
                    secret_name = name,
                    primary_error = %primary_err,
                    "Primary secret source failed, trying fallback"
                );

                Self::load(name, fallback).map_err(|fallback_err| {
                    crate::Error::Internal(format!(
                        "Failed to load secret '{name}' from both primary and fallback sources. \
                         Primary error: {primary_err}. Fallback error: {fallback_err}"
                    ))
                })
            }
        }
    }

    /// Load an optional secret
    ///
    /// Returns None if the secret is not found, instead of an error.
    /// Useful for optional features like SMTP configuration.
    ///
    /// # Arguments
    /// * `name` - Human-readable name for logging
    /// * `source` - Source to load the secret from
    ///
    /// # Returns
    /// Some(secret) if found, None otherwise
    pub fn load_optional(name: &str, source: SecretSource) -> Option<String> {
        match Self::load(name, source) {
            Ok(secret) => Some(secret),
            Err(e) => {
                debug!(secret_name = name, error = %e, "Optional secret not found");
                None
            }
        }
    }

    /// Check if a file-based secret exists
    ///
    /// Useful for conditional feature enabling based on secret availability.
    ///
    /// # Arguments
    /// * `path` - Path to the secret file
    ///
    /// # Returns
    /// true if the file exists and is readable, false otherwise
    #[must_use] 
    pub fn secret_file_exists(path: &str) -> bool {
        Path::new(path).exists()
    }
}

/// Helper to sanitize secret values for safe logging
///
/// Returns a fixed-length placeholder regardless of the actual secret length.
/// This prevents leaking the character count of secrets in logs or UI output.
/// Use this when you need to log information about secrets without exposing values.
///
/// # Example
/// ```rust,ignore
/// let password = "super_secret_123";
/// println!("Password loaded: {}", mask_secret(password)); // "Password loaded: [SECRET:redacted]"
/// ```
#[must_use]
pub fn mask_secret(_secret: &str) -> &'static str {
    "[SECRET:redacted]"
}

/// Validate that required secrets are available before starting the application
///
/// This prevents the application from starting with missing or invalid secrets.
///
/// # Arguments
/// * `required_secrets` - List of (name, source) tuples for required secrets
///
/// # Returns
/// Ok(()) if all secrets are available, Err otherwise
pub fn validate_required_secrets(required_secrets: &[(&str, SecretSource)]) -> Result<()> {
    debug!("Validating {} required secrets", required_secrets.len());

    let mut missing_secrets = Vec::new();

    for (name, source) in required_secrets {
        if SecretLoader::load(name, source.clone()).is_err() {
            missing_secrets.push(*name);
        }
    }

    if !missing_secrets.is_empty() {
        return Err(crate::Error::Internal(format!(
            "Missing required secrets: {}. Application cannot start without these secrets.",
            missing_secrets.join(", ")
        )));
    }

    debug!("All required secrets validated successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_secret_direct() {
        let secret = SecretLoader::load("test", SecretSource::Direct("my_secret".to_string())).unwrap();
        assert_eq!(secret, "my_secret");
    }

    #[test]
    fn test_load_secret_file_not_found() {
        let result = SecretLoader::load("test", SecretSource::File("/nonexistent/path"));
        assert!(result.is_err());
    }

    #[test]
    fn test_load_optional() {
        let secret = SecretLoader::load_optional("test", SecretSource::Direct("optional".to_string()));
        assert_eq!(secret, Some("optional".to_string()));

        let missing = SecretLoader::load_optional("test", SecretSource::File("/nonexistent"));
        assert_eq!(missing, None);
    }

    #[test]
    fn test_mask_secret() {
        // Fixed-length mask regardless of actual secret length.
        assert_eq!(mask_secret("password123"), "[SECRET:redacted]");
        assert_eq!(mask_secret(""), "[SECRET:redacted]");
        // Verify that secrets of different lengths produce identical output.
        assert_eq!(mask_secret("short"), mask_secret("a_much_longer_secret_value"));
    }

    #[test]
    fn test_validate_required_secrets() {
        let secrets = vec![
            ("test1", SecretSource::Direct("value1".to_string())),
            ("test2", SecretSource::Direct("value2".to_string())),
        ];

        assert!(validate_required_secrets(&secrets).is_ok());
    }

    #[test]
    fn test_validate_required_secrets_missing() {
        let secrets = vec![
            ("test1", SecretSource::Direct("value1".to_string())),
            ("test2", SecretSource::File("/nonexistent")),
        ];

        let result = validate_required_secrets(&secrets);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("test2"));
    }
}
