//! Credential Types
//!
//! Type definitions for provider credentials.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Provider type enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    Bilibili,
    Alist,
    Emby,
}

impl ProviderType {
    /// Get the string representation of the provider type
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Bilibili => "bilibili",
            Self::Alist => "alist",
            Self::Emby => "emby",
        }
    }
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for ProviderType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "bilibili" => Ok(Self::Bilibili),
            "alist" => Ok(Self::Alist),
            "emby" => Ok(Self::Emby),
            _ => Err(format!("Unknown provider type: {s}")),
        }
    }
}

/// Credential data for different provider types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CredentialData {
    /// Bilibili credentials (cookies map)
    Bilibili { cookies: HashMap<String, String> },

    /// Alist credentials
    Alist {
        host: String,
        username: String,
        password: String, // Hashed password
    },

    /// Emby/Jellyfin credentials
    Emby {
        host: String,
        api_key: String,
        emby_user_id: String,
    },
}

impl CredentialData {
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

    /// Create Bilibili credential data from cookies
    #[must_use]
    pub const fn bilibili(cookies: HashMap<String, String>) -> Self {
        Self::Bilibili { cookies }
    }

    /// Create Alist credential data
    #[must_use]
    pub const fn alist(host: String, username: String, password: String) -> Self {
        Self::Alist {
            host,
            username,
            password,
        }
    }

    /// Create Emby credential data
    #[must_use]
    pub const fn emby(host: String, api_key: String, emby_user_id: String) -> Self {
        Self::Emby {
            host,
            api_key,
            emby_user_id,
        }
    }

    /// Get the provider type for this credential
    #[must_use]
    pub const fn provider_type(&self) -> ProviderType {
        match self {
            Self::Bilibili { .. } => ProviderType::Bilibili,
            Self::Alist { .. } => ProviderType::Alist,
            Self::Emby { .. } => ProviderType::Emby,
        }
    }

    /// Try to extract Alist credential fields.
    ///
    /// Returns `(host, username, password)` if this is an Alist credential,
    /// or an error describing the type mismatch.
    pub fn as_alist(&self) -> std::result::Result<(&str, &str, &str), String> {
        match self {
            Self::Alist {
                host,
                username,
                password,
            } => Ok((host, username, password)),
            other => Err(format!(
                "Expected Alist credential data, got {}",
                other.provider_type()
            )),
        }
    }

    /// Try to extract Emby credential fields.
    ///
    /// Returns `(host, api_key, emby_user_id)` if this is an Emby credential,
    /// or an error describing the type mismatch.
    pub fn as_emby(&self) -> std::result::Result<(&str, &str, &str), String> {
        match self {
            Self::Emby {
                host,
                api_key,
                emby_user_id,
            } => Ok((host, api_key, emby_user_id)),
            other => Err(format!(
                "Expected Emby credential data, got {}",
                other.provider_type()
            )),
        }
    }

    /// Try to extract Bilibili credential fields.
    ///
    /// Returns the cookies map if this is a Bilibili credential,
    /// or an error describing the type mismatch.
    pub fn as_bilibili(&self) -> std::result::Result<&HashMap<String, String>, String> {
        match self {
            Self::Bilibili { cookies } => Ok(cookies),
            other => Err(format!(
                "Expected Bilibili credential data, got {}",
                other.provider_type()
            )),
        }
    }

    /// Get the server ID for this credential
    /// - Bilibili: SHA-256 hash of the global "bilibili" scope
    /// - Alist/Emby: SHA-256 hash of host
    #[must_use]
    pub fn server_id(&self) -> String {
        match self {
            Self::Bilibili { .. } => Self::hash_server_id("bilibili"),
            Self::Alist { host, .. } | Self::Emby { host, .. } => Self::hash_server_id(host),
        }
    }

    /// Get the server ID for this credential, scoped to an optional provider instance.
    #[must_use]
    pub fn server_id_for_instance(&self, provider_instance_name: Option<&str>) -> String {
        match self {
            Self::Bilibili { .. } => self.server_id(),
            Self::Alist { host, .. } | Self::Emby { host, .. } => {
                match Self::normalized_instance_name(provider_instance_name) {
                    Some(instance_name) => {
                        Self::hash_server_id(&format!("{host}\n{instance_name}"))
                    }
                    None => self.server_id(),
                }
            }
        }
    }

    fn hash_server_id(input: &str) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(input.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_provider_type_as_str() {
        assert_eq!(ProviderType::Bilibili.as_str(), "bilibili");
        assert_eq!(ProviderType::Alist.as_str(), "alist");
        assert_eq!(ProviderType::Emby.as_str(), "emby");
    }

    #[test]
    fn test_provider_type_from_str() {
        assert_eq!(
            ProviderType::from_str("bilibili").unwrap(),
            ProviderType::Bilibili
        );
        assert_eq!(
            ProviderType::from_str("Bilibili").unwrap(),
            ProviderType::Bilibili
        );
        assert_eq!(
            ProviderType::from_str("alist").unwrap(),
            ProviderType::Alist
        );
        assert_eq!(ProviderType::from_str("emby").unwrap(), ProviderType::Emby);
        assert!(ProviderType::from_str("unknown").is_err());
    }

    #[test]
    fn test_credential_data_provider_type() {
        let bilibili = CredentialData::bilibili(HashMap::new());
        assert_eq!(bilibili.provider_type(), ProviderType::Bilibili);

        let alist =
            CredentialData::alist("https://example.com".into(), "user".into(), "pass".into());
        assert_eq!(alist.provider_type(), ProviderType::Alist);

        let emby = CredentialData::emby("https://example.com".into(), "key".into(), "uid".into());
        assert_eq!(emby.provider_type(), ProviderType::Emby);
    }

    #[test]
    fn test_credential_data_server_id() {
        // Bilibili always has the same global server_id used by core storage.
        let bilibili = CredentialData::bilibili(HashMap::new());
        assert_eq!(bilibili.server_id().len(), 64);

        // Alist/Emby use SHA-256 hash of host
        let alist = CredentialData::alist(
            "https://alist.example.com".into(),
            "user".into(),
            "pass".into(),
        );
        assert_eq!(alist.server_id().len(), 64); // SHA-256 hex is 64 chars

        // Same host should produce same server_id
        let alist2 = CredentialData::alist(
            "https://alist.example.com".into(),
            "other".into(),
            "cred".into(),
        );
        assert_eq!(alist.server_id(), alist2.server_id());

        // Different hosts should produce different server_ids
        let alist3 = CredentialData::alist(
            "https://other.example.com".into(),
            "user".into(),
            "pass".into(),
        );
        assert_ne!(alist.server_id(), alist3.server_id());
    }

    #[test]
    fn test_credential_data_server_id_for_instance_scopes_identifier() {
        let alist = CredentialData::alist(
            "https://alist.example.com".into(),
            "user".into(),
            "pass".into(),
        );
        assert_eq!(alist.server_id_for_instance(None), alist.server_id());
        assert_eq!(alist.server_id_for_instance(Some("   ")), alist.server_id());
        assert_ne!(
            alist.server_id_for_instance(Some("alist-main")),
            alist.server_id_for_instance(Some("alist-backup"))
        );

        let bilibili = CredentialData::bilibili(HashMap::new());
        assert_eq!(bilibili.server_id_for_instance(None), bilibili.server_id());
        assert_eq!(
            bilibili.server_id_for_instance(Some("bili-main")),
            bilibili.server_id_for_instance(Some("bili-backup"))
        );
    }

    #[test]
    fn test_credential_data_serialization() {
        let mut cookies = HashMap::new();
        cookies.insert("SESSDATA".to_string(), "test_value".to_string());

        let cred = CredentialData::bilibili(cookies);
        let json = serde_json::to_string(&cred).unwrap();
        assert!(json.contains(r#""type":"bilibili"#));
        assert!(json.contains("SESSDATA"));

        let deserialized: CredentialData = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, CredentialData::Bilibili { .. }));
    }
}
