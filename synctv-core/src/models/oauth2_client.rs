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
}

impl OAuth2Provider {
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
        )
    }

    /// Get default scopes for this provider type
    #[must_use]
    pub fn default_scopes(&self) -> Vec<String> {
        if self.is_oidc() {
            vec![
                "openid".to_string(),
                "profile".to_string(),
                "email".to_string(),
            ]
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
            other => Err(format!("Unknown OAuth2 provider: {other}")),
        }
    }
}

impl std::fmt::Display for OAuth2Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// OAuth2/OIDC provider mapping (NO TOKENS)
///
/// Maps `OAuth2` provider accounts to local users.
/// Tokens are NOT stored - only identity information for lookups.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserOAuthProviderMapping {
    pub id: i64,
    pub provider: String, // Stored as string in DB
    pub provider_user_id: String,
    pub user_id: UserId,
    pub username: String,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl UserOAuthProviderMapping {
    /// Get the provider as `OAuth2Provider` enum
    #[must_use]
    pub fn provider_enum(&self) -> Option<OAuth2Provider> {
        OAuth2Provider::from_str_name(&self.provider)
    }
}

/// `OAuth2` user info from provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuth2UserInfo {
    pub provider: OAuth2Provider,
    pub provider_user_id: String,
    pub username: String,
    pub email: Option<String>,
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

    #[test]
    fn test_provider_from_str_name_all_variants() {
        assert_eq!(
            OAuth2Provider::from_str_name("qq"),
            Some(OAuth2Provider::QQ)
        );
        assert_eq!(
            OAuth2Provider::from_str_name("github"),
            Some(OAuth2Provider::GitHub)
        );
        assert_eq!(
            OAuth2Provider::from_str_name("google"),
            Some(OAuth2Provider::Google)
        );
        assert_eq!(
            OAuth2Provider::from_str_name("microsoft"),
            Some(OAuth2Provider::Microsoft)
        );
        assert_eq!(
            OAuth2Provider::from_str_name("discord"),
            Some(OAuth2Provider::Discord)
        );
        assert_eq!(
            OAuth2Provider::from_str_name("casdoor"),
            Some(OAuth2Provider::Casdoor)
        );
        assert_eq!(
            OAuth2Provider::from_str_name("logto"),
            Some(OAuth2Provider::Logto)
        );
        assert_eq!(
            OAuth2Provider::from_str_name("oidc"),
            Some(OAuth2Provider::Oidc)
        );
        assert_eq!(
            OAuth2Provider::from_str_name("feishu"),
            Some(OAuth2Provider::Feishu)
        );
        assert_eq!(
            OAuth2Provider::from_str_name("gitee"),
            Some(OAuth2Provider::Gitee)
        );
    }

    #[test]
    fn test_provider_from_str_name_case_insensitive() {
        assert_eq!(
            OAuth2Provider::from_str_name("GitHub"),
            Some(OAuth2Provider::GitHub)
        );
        assert_eq!(
            OAuth2Provider::from_str_name("GOOGLE"),
            Some(OAuth2Provider::Google)
        );
        assert_eq!(
            OAuth2Provider::from_str_name("Discord"),
            Some(OAuth2Provider::Discord)
        );
    }

    #[test]
    fn test_provider_from_str_name_invalid() {
        assert_eq!(OAuth2Provider::from_str_name("invalid"), None);
        assert_eq!(OAuth2Provider::from_str_name(""), None);
        assert_eq!(OAuth2Provider::from_str_name("auth0"), None);
    }

    #[test]
    fn test_provider_is_oidc() {
        // OIDC providers
        assert!(OAuth2Provider::Casdoor.is_oidc());
        assert!(OAuth2Provider::Logto.is_oidc());
        assert!(OAuth2Provider::Oidc.is_oidc());
        assert!(OAuth2Provider::Feishu.is_oidc());
        assert!(OAuth2Provider::Google.is_oidc());
        assert!(OAuth2Provider::Microsoft.is_oidc());

        // Non-OIDC providers
        assert!(!OAuth2Provider::QQ.is_oidc());
        assert!(!OAuth2Provider::GitHub.is_oidc());
        assert!(!OAuth2Provider::Discord.is_oidc());
        assert!(!OAuth2Provider::Gitee.is_oidc());
    }

    #[test]
    fn test_provider_default_scopes_oidc() {
        let scopes = OAuth2Provider::Google.default_scopes();
        assert!(scopes.contains(&"openid".to_string()));
        assert!(scopes.contains(&"profile".to_string()));
        assert!(scopes.contains(&"email".to_string()));
    }

    #[test]
    fn test_provider_default_scopes_non_oidc() {
        let scopes = OAuth2Provider::GitHub.default_scopes();
        assert!(scopes.contains(&"identify".to_string()));
        assert!(!scopes.contains(&"openid".to_string()));
    }

    #[test]
    fn test_provider_serde_roundtrip() {
        let provider = OAuth2Provider::GitHub;
        let json = serde_json::to_string(&provider).unwrap();
        assert_eq!(json, "\"github\"");
        let deserialized: OAuth2Provider = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, provider);
    }

    #[test]
    fn test_provider_serde_all_variants() {
        let providers = vec![
            OAuth2Provider::QQ,
            OAuth2Provider::GitHub,
            OAuth2Provider::Google,
            OAuth2Provider::Microsoft,
            OAuth2Provider::Discord,
            OAuth2Provider::Casdoor,
            OAuth2Provider::Logto,
            OAuth2Provider::Oidc,
            OAuth2Provider::Feishu,
            OAuth2Provider::Gitee,
        ];
        for p in providers {
            let json = serde_json::to_string(&p).unwrap();
            let deserialized: OAuth2Provider = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, p);
        }
    }

    #[test]
    fn test_mapping_provider_enum() {
        let mapping = UserOAuthProviderMapping {
            id: 1,
            provider: "github".to_string(),
            provider_user_id: "gh_123".to_string(),
            user_id: UserId::expect_positive(1),
            username: "testuser".to_string(),
            email: Some("test@example.com".to_string()),
            avatar_url: Some("https://example.com/avatar.png".to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert_eq!(mapping.provider_enum(), Some(OAuth2Provider::GitHub));
    }

    #[test]
    fn test_mapping_provider_enum_unknown() {
        let mapping = UserOAuthProviderMapping {
            id: 1,
            provider: "unknown_provider".to_string(),
            provider_user_id: "xyz".to_string(),
            user_id: UserId::expect_positive(1),
            username: "testuser".to_string(),
            email: None,
            avatar_url: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert_eq!(mapping.provider_enum(), None);
    }

    #[test]
    fn test_mapping_serde_roundtrip() {
        let mapping = UserOAuthProviderMapping {
            id: 1,
            provider: "google".to_string(),
            provider_user_id: "goog_456".to_string(),
            user_id: UserId::expect_positive(2),
            username: "googleuser".to_string(),
            email: Some("user@gmail.com".to_string()),
            avatar_url: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let json = serde_json::to_value(&mapping).unwrap();
        let deserialized: UserOAuthProviderMapping = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.id, mapping.id);
        assert_eq!(deserialized.provider, mapping.provider);
        assert_eq!(deserialized.provider_user_id, mapping.provider_user_id);
        assert_eq!(deserialized.username, mapping.username);
    }

    #[test]
    fn test_user_info_serde_roundtrip() {
        let info = OAuth2UserInfo {
            provider: OAuth2Provider::GitHub,
            provider_user_id: "gh_789".to_string(),
            username: "ghuser".to_string(),
            email: Some("gh@example.com".to_string()),
            avatar: Some("https://avatars.githubusercontent.com/u/123".to_string()),
        };
        let json = serde_json::to_value(&info).unwrap();
        let deserialized: OAuth2UserInfo = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.provider, OAuth2Provider::GitHub);
        assert_eq!(deserialized.provider_user_id, "gh_789");
        assert_eq!(deserialized.username, "ghuser");
    }

    #[test]
    fn test_provider_display_and_parse_roundtrip() {
        assert_eq!(OAuth2Provider::GitHub.to_string(), "github");
        assert_eq!(
            "LOGTO".parse::<OAuth2Provider>().unwrap(),
            OAuth2Provider::Logto
        );
        assert_eq!(
            OAuth2Provider::from_str_name("google"),
            Some(OAuth2Provider::Google)
        );
        assert!("unknown".parse::<OAuth2Provider>().is_err());
    }

    #[test]
    fn test_auth_url_response_serde() {
        let resp = OAuth2AuthUrlResponse {
            url: "https://github.com/login/oauth/authorize?client_id=xxx".to_string(),
            state: "random_state".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("authorize"));
        assert!(json.contains("random_state"));
    }

    #[test]
    fn test_callback_request_deserialize() {
        let json = serde_json::json!({
            "code": "auth_code_123",
            "state": "state_456"
        });
        let req: OAuth2CallbackRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.code, "auth_code_123");
        assert_eq!(req.state, "state_456");
    }

    #[test]
    fn test_callback_response_with_token() {
        let resp = OAuth2CallbackResponse {
            token: Some("jwt.token.here".to_string()),
            redirect: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["token"], "jwt.token.here");
        assert!(json["redirect"].is_null());
    }

    #[test]
    fn test_callback_response_with_redirect() {
        let resp = OAuth2CallbackResponse {
            token: None,
            redirect: Some("https://example.com/bind".to_string()),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json["token"].is_null());
        assert_eq!(json["redirect"], "https://example.com/bind");
    }
}
