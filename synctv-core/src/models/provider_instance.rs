// Media Provider Instance Models
// Core data structures for media provider instance management system.
// Supports both local (in-process) and remote (gRPC) provider instances.

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha1::Sha1;
use std::collections::HashMap;

use super::{pagination::PageParams, query::SortDirection};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderInstanceListSortBy {
    Name,
    Endpoint,
    UpdatedAt,
    #[default]
    CreatedAt,
}

impl ProviderInstanceListSortBy {
    #[must_use]
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Endpoint => "endpoint",
            Self::UpdatedAt => "updated_at",
            Self::CreatedAt => "created_at",
        }
    }
}

impl std::str::FromStr for ProviderInstanceListSortBy {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "name" => Ok(Self::Name),
            "endpoint" => Ok(Self::Endpoint),
            "updated_at" | "updatedat" => Ok(Self::UpdatedAt),
            "created_at" | "createdat" => Ok(Self::CreatedAt),
            other => Err(format!(
                "Unknown provider instance list sort field: {other}"
            )),
        }
    }
}

impl std::fmt::Display for ProviderInstanceListSortBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Name => "name",
            Self::Endpoint => "endpoint",
            Self::UpdatedAt => "updated_at",
            Self::CreatedAt => "created_at",
        };
        f.write_str(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInstanceListQuery {
    pub pagination: PageParams,
    pub provider_type: Option<String>,
    pub search: Option<String>,
    pub enabled: Option<bool>,
    pub tls: Option<bool>,
    #[serde(default)]
    pub sort_by: ProviderInstanceListSortBy,
    #[serde(default)]
    pub sort_direction: SortDirection,
}

impl Default for ProviderInstanceListQuery {
    fn default() -> Self {
        Self {
            pagination: PageParams::default(),
            provider_type: None,
            search: None,
            enabled: None,
            tls: None,
            sort_by: ProviderInstanceListSortBy::CreatedAt,
            sort_direction: SortDirection::Desc,
        }
    }
}

/// Media Provider Instance Configuration
///
/// Represents a gRPC media provider instance that can be deployed in different regions
/// for cross-region video parsing and content delivery.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProviderInstance {
    /// Instance name (primary key, unique identifier)
    pub name: String,

    /// gRPC service endpoint (e.g., "http://beijing.example.com:50051")
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

    /// Supported media provider types (e.g., `["bilibili", "alist", "emby"]`)
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
    /// Credential ID (shared base62 ID)
    pub id: String,

    /// User ID (shared base62 ID, foreign key to users table)
    pub user_id: String,

    /// Media provider type ("bilibili", "alist", "emby")
    pub provider: String,

    /// Server identifier
    /// - Bilibili: SHA-256(provider scope), globally unique per user
    /// - Alist/Emby: SHA-256(host or host+instance) (allows multiple servers per user)
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
    const BILIBILI_SCOPE: &'static str = "bilibili";

    /// Generate a shared 12-character credential ID for
    /// `user_media_provider_credentials.id`.
    #[must_use]
    pub fn new_id() -> String {
        crate::models::id::generate_id()
    }

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
        hex::encode(Sha256::digest(host.as_bytes()))
    }

    /// Generate a `server_id` scoped to the provider instance.
    #[must_use]
    pub fn generate_server_id_for_instance(
        host: &str,
        provider_instance_name: Option<&str>,
    ) -> String {
        use sha2::{Digest, Sha256};

        match Self::normalized_instance_name(provider_instance_name) {
            Some(instance_name) => hex::encode(Sha256::digest(
                format!("{host}\n{instance_name}").as_bytes(),
            )),
            None => Self::generate_server_id(host),
        }
    }

    /// Generate the single global Bilibili `server_id`.
    #[must_use]
    pub fn bilibili_server_id() -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(Self::BILIBILI_SCOPE.as_bytes()))
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        otp_secret: Option<String>,
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
    pub fn alist(
        host: String,
        username: String,
        password: String,
        otp_secret: Option<String>,
    ) -> Self {
        Self::Alist {
            host,
            username,
            password,
            otp_secret: Self::normalize_alist_otp_secret(otp_secret),
        }
    }

    #[must_use]
    pub fn normalize_alist_otp_secret(otp_secret: Option<String>) -> Option<String> {
        otp_secret.and_then(|otp_secret| {
            let trimmed = otp_secret.trim();
            if trimmed.is_empty() {
                return None;
            }

            if trimmed
                .get(..10)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("otpauth://"))
            {
                return url::Url::parse(trimmed).ok().and_then(|url| {
                    url.query_pairs()
                        .find(|(key, _)| key.eq_ignore_ascii_case("secret"))
                        .map(|(_, value)| value.trim().to_string())
                        .filter(|value| !value.is_empty())
                });
            }

            Some(trimmed.to_string())
        })
    }

    pub fn current_alist_otp_code(otp_secret: &str) -> Result<String, String> {
        Self::alist_otp_code_at_timestamp(otp_secret, Utc::now().timestamp())
    }

    pub fn alist_otp_code_at_timestamp(otp_secret: &str, timestamp: i64) -> Result<String, String> {
        let secret = Self::normalize_alist_otp_secret(Some(otp_secret.to_string()))
            .ok_or_else(|| "Alist OTP secret must not be empty".to_string())?;
        let key = decode_base32_secret(&secret)?;
        if key.is_empty() {
            return Err("Alist OTP secret must not decode to an empty key".to_string());
        }

        let counter = u64::try_from(timestamp.max(0) / 30)
            .map_err(|_| "Invalid Alist OTP timestamp".to_string())?;
        let mut mac = Hmac::<Sha1>::new_from_slice(&key)
            .map_err(|_| "Invalid Alist OTP secret key".to_string())?;
        mac.update(&counter.to_be_bytes());
        let digest = mac.finalize().into_bytes();
        let offset = usize::from(digest[digest.len() - 1] & 0x0f);
        let binary = ((u32::from(digest[offset]) & 0x7f) << 24)
            | (u32::from(digest[offset + 1]) << 16)
            | (u32::from(digest[offset + 2]) << 8)
            | u32::from(digest[offset + 3]);

        Ok(format!("{:06}", binary % 1_000_000))
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

fn decode_base32_secret(secret: &str) -> Result<Vec<u8>, String> {
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    let mut decoded = Vec::new();

    for ch in secret.chars() {
        if ch == '=' || ch.is_whitespace() {
            continue;
        }

        let value = match ch.to_ascii_uppercase() {
            'A'..='Z' => u32::from(ch.to_ascii_uppercase()) - u32::from('A'),
            '2'..='7' => u32::from(ch) - u32::from('2') + 26,
            _ => return Err("Alist OTP secret must be RFC 4648 base32".to_string()),
        };

        buffer = (buffer << 5) | value;
        bits += 5;

        if bits >= 8 {
            bits -= 8;
            let byte = u8::try_from((buffer >> bits) & 0xff)
                .map_err(|_| "Invalid Alist OTP base32 byte".to_string())?;
            decoded.push(byte);
            buffer &= (1_u32 << bits) - 1;
        }
    }

    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_instance_supports_provider() {
        let instance = ProviderInstance {
            name: "test-instance".to_string(),
            endpoint: "http://localhost:50051".to_string(),
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
            endpoint: "http://localhost:50051".to_string(),
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
    fn test_user_credential_new_id_uses_shared_12_char_format() {
        let id = UserProviderCredential::new_id();
        assert_eq!(id.len(), crate::models::ID_LENGTH);
        assert!(synctv_common::id::is_valid_with_len(
            &id,
            crate::models::ID_LENGTH
        ));
    }

    #[test]
    fn test_user_credential_generate_server_id_for_instance_changes_with_instance_name() {
        let unscoped = UserProviderCredential::generate_server_id_for_instance(
            "https://alist.example.com",
            None,
        );
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

        assert_eq!(unscoped.len(), 64);
        assert_eq!(scoped.len(), 64);
        assert_eq!(scoped, scoped_duplicate);
        assert_ne!(unscoped, scoped);
        assert_ne!(scoped, scoped_other);
    }

    #[test]
    fn test_bilibili_server_id_is_global_stable_hash() {
        let first = UserProviderCredential::bilibili_server_id();
        let second = UserProviderCredential::bilibili_server_id();

        assert_eq!(first.len(), 64);
        assert_eq!(first, second);
    }

    #[test]
    fn test_user_credential_is_expired() {
        use chrono::Duration;

        // Expired credential
        let expired = UserProviderCredential {
            id: "test_id".to_string(),
            user_id: "user_id".to_string(),
            provider: "bilibili".to_string(),
            server_id: UserProviderCredential::bilibili_server_id(),
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
            None,
        );
        assert_eq!(alist.provider_type(), "alist");

        let emby = ProviderCredential::emby(
            "https://emby.example.com".to_string(),
            "api_key_123".to_string(),
            "user_uuid".to_string(),
        );
        assert_eq!(emby.provider_type(), "emby");
    }

    #[test]
    fn alist_otp_code_matches_rfc6238_sha1_vector_truncated_to_six_digits() {
        let code =
            ProviderCredential::alist_otp_code_at_timestamp("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", 59)
                .expect("RFC test vector secret should decode");

        assert_eq!(code, "287082");
    }

    #[test]
    fn alist_otp_secret_normalization_accepts_otpauth_uri() {
        assert_eq!(
            ProviderCredential::normalize_alist_otp_secret(Some(
                "otpauth://totp/Alist:admin?secret=JBSWY3DPEHPK3PXP&issuer=Alist".to_string()
            ))
            .as_deref(),
            Some("JBSWY3DPEHPK3PXP")
        );
    }
}
