// Media Provider Instance Models
// Core data structures for media provider instance management system.
// Supports both local (in-process) and remote (gRPC) provider instances.

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha1::Sha1;
use std::collections::HashMap;

use super::{pagination::PageParams, query::SortDirection, UserId};

pub const DEFAULT_PROVIDER_INSTANCE_TIMEOUT_SECONDS: u32 = 10;
pub const PROVIDER_INSTANCE_NAME_MAX_LEN: usize = 64;

/// Normalize optional provider instance names at API, service, and repository boundaries.
///
/// Blank names represent the default local provider binding and are stored as `NULL`.
#[must_use]
pub fn normalize_provider_instance_name(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|trimmed| !trimmed.is_empty())
}

#[must_use]
pub fn normalize_provider_instance_name_owned(value: Option<String>) -> Option<String> {
    value.and_then(|value| normalize_provider_instance_name(Some(&value)).map(str::to_owned))
}

#[must_use]
pub fn is_valid_provider_instance_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= PROVIDER_INSTANCE_NAME_MAX_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub fn validate_provider_instance_name(value: &str) -> Result<(), String> {
    if is_valid_provider_instance_name(value) {
        Ok(())
    } else {
        Err(format!(
            "provider instance name must be 1-{PROVIDER_INSTANCE_NAME_MAX_LEN} characters of letters, numbers, underscores, or hyphens"
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInstanceBindingMismatch;

impl std::fmt::Display for ProviderInstanceBindingMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "provider_instance_name does not match the provider instance used when the credential was created",
        )
    }
}

impl std::error::Error for ProviderInstanceBindingMismatch {}

/// Resolve the effective provider instance for a credential-backed request.
///
/// If no credential is involved, the explicit request binding is used. If a
/// credential is involved, its stored provider instance is authoritative: an
/// omitted request binding adopts it, while a different explicit binding is
/// rejected.
pub fn resolve_provider_instance_binding(
    requested_instance_name: Option<&str>,
    credential_instance_name: Option<Option<&str>>,
) -> Result<Option<String>, ProviderInstanceBindingMismatch> {
    let requested = normalize_provider_instance_name(requested_instance_name).map(str::to_string);
    let Some(credential_instance_name) = credential_instance_name else {
        return Ok(requested);
    };

    let credential_instance =
        normalize_provider_instance_name(credential_instance_name).map(str::to_string);
    if let Some(requested) = requested {
        if Some(requested.clone()) != credential_instance {
            return Err(ProviderInstanceBindingMismatch);
        }
    }

    Ok(credential_instance)
}

sort_field_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum ProviderInstanceListSortBy {
        Name => { display: "name", sql: "name" },
        Endpoint => { display: "endpoint", sql: "endpoint" },
        UpdatedAt => { display: "updated_at", sql: "updated_at", aliases: ["updatedat"] },
        CreatedAt => { display: "created_at", sql: "created_at", aliases: ["createdat"] },
    }
    default = CreatedAt;
    error = "Unknown provider instance list sort field";
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

#[derive(Debug, Clone)]
pub struct NewProviderInstance {
    pub name: String,
    pub endpoint: String,
    pub comment: Option<String>,
    pub jwt_secret: Option<String>,
    pub custom_ca: Option<String>,
    pub timeout_seconds: u32,
    pub tls: bool,
    pub insecure_tls: bool,
    pub providers: Vec<String>,
}

impl ProviderInstance {
    #[must_use]
    pub fn new_remote(request: NewProviderInstance) -> Self {
        let now = Utc::now();
        let timeout_seconds = if request.timeout_seconds == 0 {
            DEFAULT_PROVIDER_INSTANCE_TIMEOUT_SECONDS
        } else {
            request.timeout_seconds
        };

        Self {
            name: request.name,
            endpoint: request.endpoint,
            comment: trim_optional_string(request.comment),
            jwt_secret: trim_optional_string(request.jwt_secret),
            custom_ca: trim_optional_string(request.custom_ca),
            timeout: Self::timeout_string_from_seconds(timeout_seconds),
            tls: request.tls,
            insecure_tls: request.insecure_tls,
            providers: request.providers,
            enabled: true,
            created_at: now,
            updated_at: now,
        }
    }

    #[must_use]
    pub fn timeout_string_from_seconds(seconds: u32) -> String {
        format!("{seconds}s")
    }

    #[must_use]
    pub fn timeout_seconds(&self) -> u32 {
        self.parse_timeout()
            .ok()
            .and_then(|timeout| u32::try_from(timeout.as_secs()).ok())
            .unwrap_or(DEFAULT_PROVIDER_INSTANCE_TIMEOUT_SECONDS)
    }

    /// Check if this instance supports a specific media provider type
    #[must_use]
    pub fn supports_provider(&self, provider: &str) -> bool {
        self.providers.iter().any(|candidate| candidate == provider)
    }

    /// Parse timeout string to Duration
    pub fn parse_timeout(&self) -> Result<std::time::Duration, String> {
        self.timeout
            .parse::<humantime::Duration>()
            .map(std::time::Duration::from)
            .map_err(|e| format!("Invalid timeout format '{}': {}", self.timeout, e))
    }
}

fn trim_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed.to_string())
    })
}

/// User Media Provider Credential
///
/// Stores user-specific credentials for media providers (Bilibili cookies, Alist passwords, etc.)
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct UserProviderCredential {
    /// Credential row ID.
    pub id: i64,

    /// User ID (foreign key to users table).
    pub user_id: UserId,

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

        match normalize_provider_instance_name(provider_instance_name) {
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
    fn provider_instance_new_remote_normalizes_optional_fields_and_default_timeout() {
        let instance = ProviderInstance::new_remote(NewProviderInstance {
            name: "remote".to_string(),
            endpoint: "http://localhost:50051".to_string(),
            comment: Some("  primary remote  ".to_string()),
            jwt_secret: Some("   ".to_string()),
            custom_ca: Some("  ca-pem  ".to_string()),
            timeout_seconds: 0,
            tls: true,
            insecure_tls: false,
            providers: vec!["alist".to_string()],
        });

        assert_eq!(instance.comment.as_deref(), Some("primary remote"));
        assert_eq!(instance.jwt_secret, None);
        assert_eq!(instance.custom_ca.as_deref(), Some("ca-pem"));
        assert_eq!(
            instance.timeout_seconds(),
            DEFAULT_PROVIDER_INSTANCE_TIMEOUT_SECONDS
        );
        assert_eq!(instance.timeout, "10s");
        assert!(instance.enabled);
    }

    #[test]
    fn test_normalize_provider_instance_name() {
        assert_eq!(normalize_provider_instance_name(None), None);
        assert_eq!(normalize_provider_instance_name(Some("")), None);
        assert_eq!(normalize_provider_instance_name(Some("   ")), None);
        assert_eq!(
            normalize_provider_instance_name(Some("  alist_home  ")),
            Some("alist_home")
        );
        assert_eq!(
            normalize_provider_instance_name_owned(Some("  emby_main  ".to_string())),
            Some("emby_main".to_string())
        );
    }

    #[test]
    fn test_validate_provider_instance_name_matches_proto_contract() {
        assert!(validate_provider_instance_name("alist-main_01").is_ok());
        assert!(validate_provider_instance_name("").is_err());
        assert!(validate_provider_instance_name("bad name").is_err());
        assert!(validate_provider_instance_name("bad.name").is_err());
        assert!(validate_provider_instance_name("中文").is_err());
        assert!(validate_provider_instance_name(&"a".repeat(65)).is_err());
    }

    #[test]
    fn test_user_credential_generate_server_id() {
        let server_id = UserProviderCredential::generate_server_id("https://alist.example.com");
        assert_eq!(server_id.len(), 64); // SHA-256 hex string is 64 chars
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
            id: 0,
            user_id: UserId::new(),
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

    #[test]
    fn provider_instance_binding_uses_credential_when_request_omits_instance() {
        assert_eq!(
            resolve_provider_instance_binding(None, Some(Some(" alist_remote ")))
                .expect("credential binding should resolve")
                .as_deref(),
            Some("alist_remote")
        );
    }

    #[test]
    fn provider_instance_binding_rejects_explicit_conflict() {
        assert!(
            resolve_provider_instance_binding(Some("alist_other"), Some(Some("alist_remote")),)
                .is_err()
        );
    }

    #[test]
    fn provider_instance_binding_rejects_explicit_instance_for_unbound_credential() {
        assert!(resolve_provider_instance_binding(Some("alist_remote"), Some(None)).is_err());
    }
}
