// Media Provider Instance Models
//
// Core data structures for media provider instance management system.
// Supports both local (in-process) and remote (gRPC) provider instances.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Media Provider Instance Configuration
///
/// Represents a gRPC media provider instance that can be deployed in different regions
/// for cross-region video parsing and content delivery.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProviderInstance {
    /// Instance name (primary key, unique identifier)
    pub name: String,

    /// gRPC service endpoint (e.g., "<grpc://beijing.example.com:50051>")
    pub endpoint: String,

    /// Human-readable description
    pub comment: Option<String>,

    /// JWT secret for authentication (encrypted in database)
    pub jwt_secret: Option<String>,

    /// Custom CA certificate in PEM format (encrypted in database)
    pub custom_ca: Option<String>,

    /// Request timeout (e.g., "10s", "30s")
    pub timeout: String,

    /// Enable TLS for gRPC connection
    pub tls: bool,

    /// Skip TLS certificate verification (UNSAFE, dev/test only)
    pub insecure_tls: bool,

    /// Supported media provider types (e.g., ["bilibili", "alist", "emby"])
    pub providers: Vec<String>,

    /// Whether this instance is enabled
    pub enabled: bool,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last update timestamp
    pub updated_at: DateTime<Utc>,
}

impl ProviderInstance {
    /// Check if this instance supports a specific media provider type
    #[must_use]
    pub fn supports_provider(&self, provider: &str) -> bool {
        self.providers.contains(&provider.to_string())
    }

    /// Parse timeout string to Duration
    pub fn parse_timeout(&self) -> Result<std::time::Duration, String> {
        self.timeout
            .parse::<humantime::Duration>()
            .map(std::time::Duration::from)
            .map_err(|e| format!("Invalid timeout format '{}': {}", self.timeout, e))
    }
}

/// User Media Provider Credential
///
/// Stores user-specific credentials for media providers (Bilibili cookies, Alist passwords, etc.)
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct UserProviderCredential {
    /// Credential ID (nanoid)
    pub id: String,

    /// User ID (nanoid, foreign key to users table)
    pub user_id: String,

    /// Media provider type ("bilibili", "alist", "emby")
    pub provider: String,

    /// Server identifier
    /// - Bilibili: "bilibili" (constant, ensures one credential per user)
    /// - Alist/Emby: MD5(host) (allows multiple servers per user)
    pub server_id: String,

    /// Associated media provider instance name (optional)
    pub provider_instance_name: Option<String>,

    /// Credential data in JSONB format (encrypted at rest via AES-256-GCM)
    pub credential_data: Value,

    /// Credential expiration time (optional, for tokens/cookies with TTL)
    pub expires_at: Option<DateTime<Utc>>,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last update timestamp
    pub updated_at: DateTime<Utc>,
}

impl UserProviderCredential {
    /// Fixed `server_id` for Bilibili (ensures one credential per user)
    pub const BILIBILI_SERVER_ID: &'static str = "bilibili";

    fn normalized_instance_name(provider_instance_name: Option<&str>) -> Option<&str> {
        provider_instance_name.and_then(|name| {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
    }

    /// Generate `server_id` for Alist/Emby from host URL
    #[must_use]
    pub fn generate_server_id(host: &str) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(host.as_bytes()))
    }

    /// Generate a `server_id` scoped to the provider instance.
    ///
    /// Without an instance name this preserves the legacy identifier so existing
    /// single-instance credentials are stable. When an instance name is present,
    /// it becomes part of the identifier to prevent credentials for different
    /// provider backends from overwriting each other.
    #[must_use]
    pub fn generate_server_id_for_instance(
        host: &str,
        provider_instance_name: Option<&str>,
    ) -> String {
        use sha2::{Digest, Sha256};

        match Self::normalized_instance_name(provider_instance_name) {
            Some(instance_name) => {
                format!("{:x}", Sha256::digest(format!("{host}\n{instance_name}").as_bytes()))
            }
            None => Self::generate_server_id(host),
        }
    }

    /// Generate the Bilibili `server_id`, optionally scoped to a provider instance.
    #[must_use]
    pub fn bilibili_server_id(provider_instance_name: Option<&str>) -> String {
        use sha2::{Digest, Sha256};

        match Self::normalized_instance_name(provider_instance_name) {
            Some(instance_name) => format!(
                "{:x}",
                Sha256::digest(format!("{}\n{instance_name}", Self::BILIBILI_SERVER_ID).as_bytes())
            ),
            None => Self::BILIBILI_SERVER_ID.to_string(),
        }
    }

    /// Check if this credential has expired
    #[must_use]
    pub fn is_expired(&self) -> bool {
        if let Some(expires_at) = self.expires_at {
            expires_at <= Utc::now()
        } else {
            false // No expiration set, never expires
        }
    }

    /// Check if this credential is still valid (not expired)
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.is_expired()
    }

    /// Parse credential data into a typed structure
    pub fn get_credential(&self) -> Result<ProviderCredential, serde_json::Error> {
        serde_json::from_value(self.credential_data.clone())
    }
}

/// Media Provider Credential Types
///
/// Enum representing different credential formats for supported media providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderCredential {
    /// Bilibili credentials (cookies)
    Bilibili { cookies: HashMap<String, String> },

    /// Alist credentials (username/password)
    Alist {
        host: String,
        username: String,
        password: String, // Already hashed
    },

    /// Emby/Jellyfin credentials (API key)
    Emby {
        host: String,
        api_key: String,
        emby_user_id: String,
    },
}

impl ProviderCredential {
    /// Create Bilibili credential from cookies map
    #[must_use]
    pub const fn bilibili(cookies: HashMap<String, String>) -> Self {
        Self::Bilibili { cookies }
    }

    /// Create Alist credential
    #[must_use]
    pub const fn alist(host: String, username: String, password: String) -> Self {
        Self::Alist {
            host,
            username,
            password,
        }
    }

    /// Create Emby credential
    #[must_use]
    pub const fn emby(host: String, api_key: String, emby_user_id: String) -> Self {
        Self::Emby {
            host,
            api_key,
            emby_user_id,
        }
    }

    /// Get the media provider type name
    #[must_use]
    pub const fn provider_type(&self) -> &'static str {
        match self {
            Self::Bilibili { .. } => "bilibili",
            Self::Alist { .. } => "alist",
            Self::Emby { .. } => "emby",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_instance_supports_provider() {
        let instance = ProviderInstance {
            name: "test-instance".to_string(),
            endpoint: "grpc://localhost:50051".to_string(),
            comment: None,
            jwt_secret: None,
            custom_ca: None,
            timeout: "10s".to_string(),
            tls: false,
            insecure_tls: false,
            providers: vec!["bilibili".to_string(), "alist".to_string()],
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        assert!(instance.supports_provider("bilibili"));
        assert!(instance.supports_provider("alist"));
        assert!(!instance.supports_provider("emby"));
    }

    #[test]
    fn test_provider_instance_parse_timeout() {
        let instance = ProviderInstance {
            name: "test".to_string(),
            endpoint: "grpc://localhost:50051".to_string(),
            comment: None,
            jwt_secret: None,
            custom_ca: None,
            timeout: "15s".to_string(),
            tls: false,
            insecure_tls: false,
            providers: vec![],
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let duration = instance.parse_timeout().unwrap();
        assert_eq!(duration, std::time::Duration::from_secs(15));
    }

    #[test]
    fn test_user_credential_generate_server_id() {
        let server_id = UserProviderCredential::generate_server_id("https://alist.example.com");
        assert_eq!(server_id.len(), 64); // SHA-256 hex string is 64 chars
    }

    #[test]
    fn test_user_credential_generate_server_id_for_instance_changes_with_instance_name() {
        let legacy =
            UserProviderCredential::generate_server_id_for_instance("https://alist.example.com", None);
        let scoped = UserProviderCredential::generate_server_id_for_instance(
            "https://alist.example.com",
            Some("alist-main"),
        );
        let scoped_duplicate = UserProviderCredential::generate_server_id_for_instance(
            "https://alist.example.com",
            Some("alist-main"),
        );
        let scoped_other = UserProviderCredential::generate_server_id_for_instance(
            "https://alist.example.com",
            Some("alist-backup"),
        );

        assert_eq!(legacy.len(), 64);
        assert_eq!(scoped.len(), 64);
        assert_eq!(scoped, scoped_duplicate);
        assert_ne!(legacy, scoped);
        assert_ne!(scoped, scoped_other);
    }

    #[test]
    fn test_bilibili_server_id_defaults_to_legacy_constant_without_instance() {
        assert_eq!(
            UserProviderCredential::bilibili_server_id(None),
            UserProviderCredential::BILIBILI_SERVER_ID
        );
        assert_eq!(
            UserProviderCredential::bilibili_server_id(Some("   ")),
            UserProviderCredential::BILIBILI_SERVER_ID
        );
    }

    #[test]
    fn test_bilibili_server_id_scopes_by_instance_name() {
        let main = UserProviderCredential::bilibili_server_id(Some("bili-main"));
        let main_dup = UserProviderCredential::bilibili_server_id(Some("bili-main"));
        let backup = UserProviderCredential::bilibili_server_id(Some("bili-backup"));

        assert_eq!(main.len(), 64);
        assert_eq!(main, main_dup);
        assert_ne!(main, backup);
        assert_ne!(main, UserProviderCredential::BILIBILI_SERVER_ID);
    }

    #[test]
    fn test_user_credential_is_expired() {
        use chrono::Duration;

        // Expired credential
        let expired = UserProviderCredential {
            id: "test_id".to_string(),
            user_id: "user_id".to_string(),
            provider: "bilibili".to_string(),
            server_id: "bilibili".to_string(),
            provider_instance_name: None,
            credential_data: serde_json::json!({}),
            expires_at: Some(Utc::now() - Duration::hours(1)),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert!(expired.is_expired());
        assert!(!expired.is_valid());

        // Valid credential
        let valid = UserProviderCredential {
            expires_at: Some(Utc::now() + Duration::hours(1)),
            ..expired.clone()
        };
        assert!(!valid.is_expired());
        assert!(valid.is_valid());

        // No expiration
        let no_expiry = UserProviderCredential {
            expires_at: None,
            ..expired
        };
        assert!(!no_expiry.is_expired());
        assert!(no_expiry.is_valid());
    }

    #[test]
    fn test_provider_credential_types() {
        let mut cookies = HashMap::new();
        cookies.insert("SESSDATA".to_string(), "test_session".to_string());

        let bilibili = ProviderCredential::bilibili(cookies);
        assert_eq!(bilibili.provider_type(), "bilibili");

        let alist = ProviderCredential::alist(
            "https://alist.example.com".to_string(),
            "admin".to_string(),
            "hashed_password".to_string(),
        );
        assert_eq!(alist.provider_type(), "alist");

        let emby = ProviderCredential::emby(
            "https://emby.example.com".to_string(),
            "api_key_123".to_string(),
            "user_uuid".to_string(),
        );
        assert_eq!(emby.provider_type(), "emby");
    }
}
