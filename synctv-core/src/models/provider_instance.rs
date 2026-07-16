// Media Provider Instance Models
// Core data structures for media provider instance management system.
// Supports both local (in-process) and remote provider instances.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::{SourceProvider, UserId, pagination::PageParams, query::SortDirection};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialProviderInstanceName<'a> {
    NotCredentialBacked,
    CredentialBacked(Option<&'a str>),
}

/// Resolve the effective provider instance for a credential-backed request.
///
/// If no credential is involved, the explicit request binding is used. If a
/// credential is involved, its stored provider instance is authoritative: an
/// omitted request binding adopts it, while a different explicit binding is
/// rejected.
pub fn resolve_provider_instance_binding(
    requested_instance_name: Option<&str>,
    credential_instance_name: CredentialProviderInstanceName<'_>,
) -> Result<Option<String>, ProviderInstanceBindingMismatch> {
    let requested = normalize_provider_instance_name(requested_instance_name).map(str::to_string);
    let credential_instance_name = match credential_instance_name {
        CredentialProviderInstanceName::NotCredentialBacked => return Ok(requested),
        CredentialProviderInstanceName::CredentialBacked(value) => value,
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
        UpdatedAt => { display: "updated_at", sql: "updated_at" },
        CreatedAt => { display: "created_at", sql: "created_at" },
    }
    default = CreatedAt;
    error = "Unknown provider instance list sort field";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInstanceListQuery {
    pub pagination: PageParams,
    pub provider_type: Option<SourceProvider>,
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
/// Represents a remote media provider instance that can be deployed in different regions
/// for cross-region video parsing and content delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInstance {
    /// Instance name (primary key, unique identifier)
    pub name: String,

    /// Remote service endpoint (e.g., "http://beijing.example.com:50051")
    pub endpoint: String,

    /// Human-readable description
    pub comment: Option<String>,

    /// JWT secret for authentication (encrypted in database)
    pub jwt_secret: Option<String>,

    /// Custom CA certificate in PEM format (encrypted in database)
    pub custom_ca: Option<String>,

    /// Request timeout (e.g., "10s", "30s")
    pub timeout: String,

    /// Enable TLS for the remote provider connection.
    pub tls: bool,

    /// Skip TLS certificate verification for explicitly trusted private endpoints.
    pub insecure_tls: bool,

    pub providers: Vec<SourceProvider>,

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
    pub providers: Vec<SourceProvider>,
}

impl ProviderInstance {
    #[must_use]
    pub fn new_remote(request: NewProviderInstance) -> Self {
        let now = crate::SystemClock.now();
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
        self.providers
            .iter()
            .any(|candidate| candidate.as_str() == provider)
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProviderCredential {
    /// Credential row ID.
    pub id: i64,

    /// User ID (foreign key to users table).
    pub user_id: UserId,

    /// Media provider type name ("bilibili", "alist", "emby").
    /// The database stores the corresponding numeric provider type code.
    pub provider: String,

    /// Provider-owned server identifier for credential lookup.
    pub server_id: String,

    /// Associated media provider instance name (optional)
    pub provider_instance_name: Option<String>,

    /// Typed credential data (encrypted at rest via AES-256-GCM).
    pub credential_data: ProviderCredential,

    /// Credential expiration time (optional, for tokens/cookies with TTL)
    pub expires_at: Option<DateTime<Utc>>,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last update timestamp
    pub updated_at: DateTime<Utc>,
}

impl UserProviderCredential {
    /// Check if this credential has expired
    #[must_use]
    pub fn is_expired(&self) -> bool {
        if let Some(expires_at) = self.expires_at {
            expires_at <= crate::SystemClock.now()
        } else {
            false // No expiration set, never expires
        }
    }

    /// Check if this credential is still valid (not expired)
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.is_expired()
    }

    #[must_use]
    pub fn credential(&self) -> &ProviderCredential {
        &self.credential_data
    }
}

/// Media Provider Credential Types
///
/// Enum representing different credential formats for supported media providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ProviderCredential {
    #[serde(rename = "bilibili")]
    /// Bilibili credentials (cookies)
    Bilibili { cookies: HashMap<String, String> },

    #[serde(rename = "alist")]
    /// Alist credentials (username/password)
    Alist {
        host: String,
        username: String,
        password: String, // Already hashed
        #[serde(default, skip_serializing_if = "Option::is_none")]
        otp_secret: Option<String>,
    },

    #[serde(rename = "emby")]
    /// Emby credentials (API key)
    Emby {
        host: String,
        api_key: String,
        emby_user_id: String,
    },

    #[serde(rename = "cloudreve")]
    /// Cloudreve credentials. Passwords are encrypted by the repository.
    Cloudreve {
        host: String,
        email: String,
        password: String,
    },

    #[serde(rename = "twitch")]
    /// Twitch web session credentials. Every value is encrypted by the repository.
    Twitch {
        login: String,
        twitch_user_id: String,
        client_id: String,
        #[serde(default)]
        scopes: Vec<String>,
        auth_token: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_integrity: Option<String>,
    },

    #[serde(rename = "youtube")]
    /// Optional YouTube Innertube session tokens. Secret fields are encrypted by the repository.
    Youtube {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        visitor_data: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        po_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cookie: Option<String>,
    },

    #[serde(rename = "douyin")]
    /// Douyin web session cookie. The cookie is encrypted by the repository.
    Douyin { label: String, cookie: String },

    #[serde(rename = "tiktok")]
    /// TikTok web session cookie. The cookie is encrypted by the repository.
    TikTok { label: String, cookie: String },

    #[serde(rename = "fnos")]
    /// FNOS RPC and WebDAV credentials. Secret fields are encrypted by the repository.
    Fnos {
        endpoint: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        webdav_endpoint: Option<String>,
        username: String,
        password: String,
        token: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        long_token: Option<String>,
        secret: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_endpoint: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_token: Option<String>,
    },

    #[serde(rename = "qnap")]
    /// QNAP File Station credentials. Secret fields are encrypted by the repository.
    Qnap {
        endpoint: String,
        username: String,
        password: String,
        sid: String,
        server_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        support_rtt: bool,
    },

    #[serde(rename = "synology")]
    /// Synology DSM File Station and Video Station credentials.
    Synology {
        endpoint: String,
        username: String,
        password: String,
        file_sid: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        video_sid: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        synotoken: Option<String>,
        apis: HashMap<String, SynologyApiBinding>,
    },

    #[serde(rename = "nextcloud")]
    /// Nextcloud DAV credentials. The app password is encrypted by the repository.
    Nextcloud {
        endpoint: String,
        username: String,
        user_id: String,
        app_password: String,
        version: String,
        edition: String,
        capabilities: serde_json::Value,
    },

    #[serde(rename = "seafile")]
    /// Seafile API token and per-library passwords. Secrets are encrypted by the repository.
    Seafile {
        endpoint: String,
        username: String,
        token: String,
        version: String,
        features: Vec<String>,
        library_passwords: HashMap<String, String>,
    },
    #[serde(rename = "truenas")]
    TrueNas {
        endpoint: String,
        api_key: String,
        hostname: String,
        version: String,
        system_product: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SynologyApiBinding {
    pub path: String,
    pub min_version: u32,
    pub max_version: u32,
}

impl Default for ProviderCredential {
    fn default() -> Self {
        Self::Bilibili {
            cookies: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

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
            providers: vec![SourceProvider::Bilibili, SourceProvider::Alist],
            enabled: true,
            created_at: crate::SystemClock.now(),
            updated_at: crate::SystemClock.now(),
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
            created_at: crate::SystemClock.now(),
            updated_at: crate::SystemClock.now(),
        };

        let duration = ok(instance.parse_timeout(), "provider timeout should parse");
        assert_eq!(duration, std::time::Duration::from_secs(15));
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
    fn test_validate_provider_instance_name_matches_remote_contract() {
        assert!(validate_provider_instance_name("alist-main_01").is_ok());
        assert!(validate_provider_instance_name("").is_err());
        assert!(validate_provider_instance_name("bad name").is_err());
        assert!(validate_provider_instance_name("bad.name").is_err());
        assert!(validate_provider_instance_name("中文").is_err());
        assert!(validate_provider_instance_name(&"a".repeat(65)).is_err());
    }

    #[test]
    fn test_user_credential_is_expired() {
        use chrono::Duration;

        // Expired credential
        let expired = UserProviderCredential {
            id: 0,
            user_id: UserId::new(),
            provider: "bilibili".to_string(),
            server_id: "bilibili-credential".to_string(),
            provider_instance_name: None,
            credential_data: ProviderCredential::default(),
            expires_at: Some(crate::SystemClock.now() - Duration::hours(1)),
            created_at: crate::SystemClock.now(),
            updated_at: crate::SystemClock.now(),
        };
        assert!(expired.is_expired());
        assert!(!expired.is_valid());

        // Valid credential
        let valid = UserProviderCredential {
            expires_at: Some(crate::SystemClock.now() + Duration::hours(1)),
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

        let bilibili = ProviderCredential::Bilibili { cookies };
        assert!(matches!(bilibili, ProviderCredential::Bilibili { .. }));

        let alist = ProviderCredential::Alist {
            host: "https://alist.example.com".to_string(),
            username: "admin".to_string(),
            password: "hashed_password".to_string(),
            otp_secret: None,
        };
        assert!(matches!(alist, ProviderCredential::Alist { .. }));

        let emby = ProviderCredential::Emby {
            host: "https://emby.example.com".to_string(),
            api_key: "api_key_123".to_string(),
            emby_user_id: "user_uuid".to_string(),
        };
        assert!(matches!(emby, ProviderCredential::Emby { .. }));
    }

    #[test]
    fn provider_instance_binding_uses_credential_when_request_omits_instance() {
        assert_eq!(
            ok(
                resolve_provider_instance_binding(
                    None,
                    CredentialProviderInstanceName::CredentialBacked(Some(" alist_remote ")),
                ),
                "credential binding should resolve"
            )
            .as_deref(),
            Some("alist_remote")
        );
    }

    #[test]
    fn provider_instance_binding_rejects_explicit_conflict() {
        assert!(
            resolve_provider_instance_binding(
                Some("alist_other"),
                CredentialProviderInstanceName::CredentialBacked(Some("alist_remote")),
            )
            .is_err()
        );
    }

    #[test]
    fn provider_instance_binding_rejects_explicit_instance_for_unbound_credential() {
        assert!(
            resolve_provider_instance_binding(
                Some("alist_remote"),
                CredentialProviderInstanceName::CredentialBacked(None),
            )
            .is_err()
        );
    }
}
