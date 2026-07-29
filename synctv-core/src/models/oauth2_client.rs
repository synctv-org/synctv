//! OAuth2/OIDC client model
//!
//! Stores OAuth2/OIDC authentication tokens for third-party login

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use crate::models::UserId;

/// OAuth2/OIDC provider type
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OAuth2Provider {
    /// QQ
    QQ,
    /// GitHub
    GitHub,
    /// Google
    Google,
    /// Microsoft
    Microsoft,
    /// Discord
    Discord,
    /// Casdoor (OIDC)
    Casdoor,
    /// Logto (OIDC)
    Logto,
    /// Generic OIDC provider
    Oidc,
    /// Feishu SSO
    Feishu,
    /// Gitee
    Gitee,
    /// Sign in with Apple
    Apple,
}

impl OAuth2Provider {
    #[must_use]
    pub const fn as_i16(&self) -> i16 {
        match self {
            Self::QQ => 1,
            Self::GitHub => 2,
            Self::Google => 3,
            Self::Microsoft => 4,
            Self::Discord => 5,
            Self::Casdoor => 6,
            Self::Logto => 7,
            Self::Oidc => 8,
            Self::Feishu => 9,
            Self::Gitee => 10,
            Self::Apple => 11,
        }
    }

    #[must_use]
    pub const fn as_str(&self) -> &str {
        match self {
            Self::QQ => "qq",
            Self::GitHub => "github",
            Self::Google => "google",
            Self::Microsoft => "microsoft",
            Self::Discord => "discord",
            Self::Casdoor => "casdoor",
            Self::Logto => "logto",
            Self::Oidc => "oidc",
            Self::Feishu => "feishu",
            Self::Gitee => "gitee",
            Self::Apple => "apple",
        }
    }

    /// Parse `OAuth2` provider type from string name (case-insensitive)
    #[must_use]
    pub fn from_str_name(s: &str) -> Option<Self> {
        s.parse().ok()
    }

    /// Check if this provider type uses OIDC standard
    #[must_use]
    pub const fn is_oidc(&self) -> bool {
        matches!(
            self,
            Self::Casdoor
                | Self::Logto
                | Self::Oidc
                | Self::Feishu
                | Self::Google
                | Self::Microsoft
                | Self::Apple
        )
    }

    /// Get default scopes for this provider type
    #[must_use]
    pub fn default_scopes(&self) -> Vec<String> {
        if matches!(self, Self::Apple) {
            vec!["openid".to_string()]
        } else if self.is_oidc() {
            vec!["openid".to_string(), "profile".to_string()]
        } else {
            vec!["identify".to_string()]
        }
    }
}

impl FromStr for OAuth2Provider {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "qq" => Ok(Self::QQ),
            "github" => Ok(Self::GitHub),
            "google" => Ok(Self::Google),
            "microsoft" => Ok(Self::Microsoft),
            "discord" => Ok(Self::Discord),
            "casdoor" => Ok(Self::Casdoor),
            "logto" => Ok(Self::Logto),
            "oidc" => Ok(Self::Oidc),
            "feishu" => Ok(Self::Feishu),
            "gitee" => Ok(Self::Gitee),
            "apple" => Ok(Self::Apple),
            other => Err(format!("Unknown OAuth2 provider: {other}")),
        }
    }
}

impl TryFrom<i16> for OAuth2Provider {
    type Error = String;

    fn try_from(value: i16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::QQ),
            2 => Ok(Self::GitHub),
            3 => Ok(Self::Google),
            4 => Ok(Self::Microsoft),
            5 => Ok(Self::Discord),
            6 => Ok(Self::Casdoor),
            7 => Ok(Self::Logto),
            8 => Ok(Self::Oidc),
            9 => Ok(Self::Feishu),
            10 => Ok(Self::Gitee),
            11 => Ok(Self::Apple),
            other => Err(format!("Unknown OAuth2 provider type code: {other}")),
        }
    }
}

impl From<OAuth2Provider> for i16 {
    fn from(value: OAuth2Provider) -> Self {
        value.as_i16()
    }
}

pub fn oauth2_provider_type_code_from_name(raw: &str) -> Result<i16, String> {
    raw.parse::<OAuth2Provider>()
        .map(|provider| provider.as_i16())
}

pub fn oauth2_provider_type_name_from_code(code: i16) -> Result<String, String> {
    OAuth2Provider::try_from(code).map(|provider| provider.to_string())
}

impl std::fmt::Display for OAuth2Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// OAuth2/OIDC provider identity mapping.
///
/// Maps provider account identities to local users for lookup and linking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserOAuthProviderMapping {
    pub id: i64,
    pub provider: OAuth2Provider,
    pub provider_instance_name: String,
    pub provider_issuer: Option<String>,
    pub provider_user_id: String,
    pub user_id: UserId,
    pub username: String,
    pub avatar_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `OAuth2` user info from provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuth2UserInfo {
    pub provider: OAuth2Provider,
    pub provider_instance_name: String,
    pub provider_issuer: Option<String>,
    pub provider_user_id: String,
    pub username: String,
    pub avatar: Option<String>,
}

/// `OAuth2` authorization URL response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuth2AuthUrlResponse {
    pub url: String,
    pub state: String,
}

/// `OAuth2` callback request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuth2CallbackRequest {
    pub code: String,
    pub state: String,
}

/// `OAuth2` callback response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuth2CallbackResponse {
    pub token: Option<String>,    // JWT token if login
    pub redirect: Option<String>, // Redirect URL
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_ok<T>(result: serde_json::Result<T>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    #[test]
    fn provider_names_codes_and_serde_are_stable() {
        let providers = [
            (OAuth2Provider::QQ, "qq", 1),
            (OAuth2Provider::GitHub, "github", 2),
            (OAuth2Provider::Google, "google", 3),
            (OAuth2Provider::Microsoft, "microsoft", 4),
            (OAuth2Provider::Discord, "discord", 5),
            (OAuth2Provider::Casdoor, "casdoor", 6),
            (OAuth2Provider::Logto, "logto", 7),
            (OAuth2Provider::Oidc, "oidc", 8),
            (OAuth2Provider::Feishu, "feishu", 9),
            (OAuth2Provider::Gitee, "gitee", 10),
            (OAuth2Provider::Apple, "apple", 11),
        ];

        for (provider, name, code) in providers {
            assert_eq!(provider.as_str(), name);
            assert_eq!(provider.to_string(), name);
            assert_eq!(
                OAuth2Provider::from_str_name(&name.to_ascii_uppercase()),
                Some(provider.clone())
            );
            assert_eq!(oauth2_provider_type_code_from_name(name), Ok(code));
            assert_eq!(OAuth2Provider::try_from(code), Ok(provider.clone()));
            assert_eq!(
                oauth2_provider_type_name_from_code(code).as_deref(),
                Ok(name)
            );

            let json = json_ok(
                serde_json::to_string(&provider),
                "provider should serialize",
            );
            assert_eq!(
                json_ok(
                    serde_json::from_str::<OAuth2Provider>(&json),
                    "provider should deserialize"
                ),
                provider
            );
        }

        assert_eq!(OAuth2Provider::from_str_name("auth0"), None);
        assert!(OAuth2Provider::try_from(0).is_err());
        assert!(oauth2_provider_type_code_from_name("unknown").is_err());
    }

    #[test]
    fn provider_default_scopes_follow_protocol_family() {
        for provider in [
            OAuth2Provider::Casdoor,
            OAuth2Provider::Logto,
            OAuth2Provider::Oidc,
            OAuth2Provider::Feishu,
            OAuth2Provider::Google,
            OAuth2Provider::Microsoft,
        ] {
            let scopes = provider.default_scopes();
            assert!(provider.is_oidc());
            assert!(scopes.contains(&"openid".to_string()));
            assert!(scopes.contains(&"profile".to_string()));
            assert!(!scopes.contains(&"email".to_string()));
        }

        assert_eq!(
            OAuth2Provider::Apple.default_scopes(),
            vec!["openid".to_string()]
        );

        for provider in [
            OAuth2Provider::QQ,
            OAuth2Provider::GitHub,
            OAuth2Provider::Discord,
            OAuth2Provider::Gitee,
        ] {
            assert!(!provider.is_oidc());
            assert_eq!(provider.default_scopes(), vec!["identify".to_string()]);
        }
    }

    #[test]
    fn mapping_provider_is_typed() {
        let mapping = UserOAuthProviderMapping {
            id: 1,
            provider: OAuth2Provider::GitHub,
            provider_instance_name: "github-main".to_string(),
            provider_issuer: Some("https://github.com".to_string()),
            provider_user_id: "gh_123".to_string(),
            user_id: UserId::expect_positive(1),
            username: "testuser".to_string(),
            avatar_url: Some("https://example.com/avatar.png".to_string()),
            created_at: crate::SystemClock.now(),
            updated_at: crate::SystemClock.now(),
        };
        assert_eq!(mapping.provider, OAuth2Provider::GitHub);
    }
}
