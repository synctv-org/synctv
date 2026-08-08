//! Logto `OAuth2` provider

use super::oidc::{OidcEndpointOverrides, OidcProvider};
use super::{validate_provider_url, validate_required_oauth2_field};
use crate::oauth2::{OAuth2Authorization, OAuth2AuthorizationMode, OAuth2UserInfo, Provider};
use crate::service::{OAuth2LogtoProviderConfig, OAuth2ProviderPrivateConfig};
use crate::Error;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Logto `OAuth2` provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogtoConfig {
    pub client_id: String,
    pub client_secret: String,
    pub endpoint: String,
}

/// Logto `OAuth2` provider
///
/// Supports multiple instances (e.g., logto1, logto2) with different endpoints.
/// Similar to Go's logtoProvider in synctv/internal/provider/providers/logto.go
pub struct LogtoProvider {
    oidc: OidcProvider,
}

impl LogtoProvider {
    /// Create a new Logto provider with configuration
    ///
    /// # Errors
    pub fn create(client_id: String, client_secret: String, endpoint: &str) -> Result<Self, Error> {
        Self::create_with_ssrf_guard(
            client_id,
            client_secret,
            endpoint,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
    }

    pub fn create_with_ssrf_guard(
        client_id: String,
        client_secret: String,
        endpoint: &str,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Self, Error> {
        let endpoint = endpoint.trim_end_matches('/');
        validate_provider_url(endpoint, "Invalid Logto endpoint", ssrf_guard)?;
        let auth_url_str = format!("{endpoint}/oidc/auth");
        let token_url_str = format!("{endpoint}/oidc/token");
        let userinfo_url = format!("{endpoint}/oidc/me");
        let issuer = format!("{endpoint}/oidc");
        let jwks_url = format!("{endpoint}/oidc/jwks");
        validate_provider_url(&auth_url_str, "Invalid Logto auth URL", ssrf_guard)?;
        validate_provider_url(&token_url_str, "Invalid Logto token URL", ssrf_guard)?;
        validate_provider_url(&userinfo_url, "Invalid Logto user info URL", ssrf_guard)?;
        validate_provider_url(&jwks_url, "Invalid Logto JWKS URL", ssrf_guard)?;
        let oidc = OidcProvider::create_with_endpoints_scopes_and_ssrf_guard(
            client_id,
            client_secret,
            &issuer,
            OidcEndpointOverrides {
                auth_url: Some(auth_url_str),
                token_url: Some(token_url_str),
                userinfo_url: Some(userinfo_url),
                jwks_url: Some(jwks_url),
            },
            vec!["openid".to_string(), "profile".to_string()],
            ssrf_guard,
        )?;

        Ok(Self { oidc })
    }
}

#[async_trait]
impl Provider for LogtoProvider {
    async fn new_auth_url(
        &self,
        state: &str,
        redirect_url: Option<&str>,
        mode: OAuth2AuthorizationMode,
    ) -> Result<OAuth2Authorization, Error> {
        self.oidc.new_auth_url(state, redirect_url, mode).await
    }

    async fn get_user_info(
        &self,
        code: &str,
        redirect_url: Option<&str>,
        pkce_verifier: Option<&str>,
        nonce: Option<&str>,
        mode: OAuth2AuthorizationMode,
    ) -> Result<OAuth2UserInfo, Error> {
        self.oidc
            .get_user_info(code, redirect_url, pkce_verifier, nonce, mode)
            .await
    }
}

#[cfg(test)]
fn username_from_profile(
    username: Option<String>,
    name: Option<String>,
    provider_user_id: &str,
) -> String {
    username
        .or(name)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| provider_user_id.to_string())
}

pub fn logto_factory_from_private_config(
    config: &OAuth2ProviderPrivateConfig,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    let OAuth2ProviderPrivateConfig::Logto(config) = config else {
        return Err(Error::InvalidInput(
            "Logto provider requires logto config".to_string(),
        ));
    };
    logto_factory_from_typed_config(config, ssrf_guard)
}

fn logto_factory_from_typed_config(
    config: &OAuth2LogtoProviderConfig,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    validate_required_oauth2_field("Logto", "client_id", &config.client_id)?;
    validate_required_oauth2_field("Logto", "client_secret", &config.client_secret)?;
    validate_required_oauth2_field("Logto", "endpoint", &config.endpoint)?;
    Ok(Box::new(LogtoProvider::create_with_ssrf_guard(
        config.client_id.clone(),
        config.client_secret.clone(),
        &config.endpoint,
        ssrf_guard,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::TestResultExt;

    fn logto_private_config(
        client_id: &str,
        client_secret: &str,
        endpoint: &str,
    ) -> OAuth2ProviderPrivateConfig {
        OAuth2ProviderPrivateConfig::Logto(OAuth2LogtoProviderConfig {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            endpoint: endpoint.to_string(),
        })
    }

    #[test]
    fn test_create_provider_valid_config() {
        let provider = LogtoProvider::create(
            "logto_client_id".to_string(),
            "logto_secret".to_string(),
            "https://logto.example.com",
        );
        assert!(provider.is_ok());
    }

    #[test]
    fn test_create_provider_endpoint_trailing_slash_trimmed() {
        let _provider = LogtoProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "https://logto.example.com/",
        )
        .checked("operation should succeed");
    }

    #[test]
    fn test_create_provider_invalid_endpoint() {
        let result = LogtoProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "not a valid endpoint",
        );
        // Invalid endpoint should fail when constructing auth/token URLs
        assert!(result.is_err());
        assert!(matches!(result.err(), Some(Error::InvalidInput(_))));
    }

    #[test]
    fn test_create_provider_allows_loopback_endpoint_when_ssrf_is_explicitly_disabled() {
        let guard = synctv_common::ssrf::SsrfGuard::disabled();
        let result = LogtoProvider::create_with_ssrf_guard(
            "id".to_string(),
            "secret".to_string(),
            "http://127.0.0.1:8443",
            &guard,
        );

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_new_auth_url_contains_required_params() {
        let provider = LogtoProvider::create(
            "logto_test_id".to_string(),
            "test_secret".to_string(),
            "https://auth.logto.io",
        )
        .checked("operation should succeed");

        let state = "logto_state_xyz";
        let auth = provider
            .new_auth_url(
                state,
                Some("https://example.com/callback"),
                OAuth2AuthorizationMode::Browser,
            )
            .await
            .checked("operation should succeed");
        let auth_url = auth.auth_url;
        let pkce_verifier = auth.pkce_verifier;

        // Auth URL should use the custom endpoint's OIDC auth path
        assert!(auth_url.starts_with("https://auth.logto.io/oidc/auth"));
        // Auth URL should contain client_id
        assert!(auth_url.contains("client_id=logto_test_id"));
        // Auth URL should contain state
        assert!(auth_url.contains(&format!("state={state}")));
        // Auth URL should contain redirect_uri
        assert!(auth_url.contains("redirect_uri="));
        assert!(auth_url.contains("scope=openid"));
        assert!(auth_url.contains("+profile"));
        assert!(!auth_url.contains("email"));
        // Auth URL should contain PKCE code_challenge
        assert!(auth_url.contains("code_challenge="));
        assert!(auth_url.contains("code_challenge_method=S256"));
        // PKCE verifier should be non-empty
        assert!(!pkce_verifier.is_empty());
    }

    #[tokio::test]
    async fn test_new_auth_url_with_trailing_slash_endpoint() {
        let provider = LogtoProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "https://logto.example.com/",
        )
        .checked("operation should succeed");

        let auth_url = provider
            .new_auth_url(
                "state",
                Some("https://example.com/cb"),
                OAuth2AuthorizationMode::Browser,
            )
            .await
            .checked("operation should succeed")
            .auth_url;
        // Should not have double slashes
        assert!(auth_url.starts_with("https://logto.example.com/oidc/auth"));
        assert!(!auth_url.contains("//oidc"));
    }

    #[test]
    fn test_factory_valid_config() {
        let config = logto_private_config("logto_id", "logto_secret", "https://logto.example.com");
        let provider = logto_factory_from_private_config(
            &config,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        );
        assert!(provider.is_ok());
    }

    #[test]
    fn test_factory_missing_endpoint() {
        let config = logto_private_config("id", "secret", "");
        let result = logto_factory_from_private_config(
            &config,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_factory_missing_fields() {
        let guard = synctv_common::ssrf::SsrfGuard::strict_policy();

        let config = logto_private_config("", "secret", "https://logto.example.com");
        assert!(logto_factory_from_private_config(&config, &guard).is_err());

        let config = logto_private_config("id", "", "https://logto.example.com");
        assert!(logto_factory_from_private_config(&config, &guard).is_err());
    }

    #[test]
    fn test_username_from_profile_uses_provider_user_id_when_profile_names_missing() {
        assert_eq!(
            username_from_profile(None, Some("  ".to_string()), "logto-subject"),
            "logto-subject"
        );
    }

    #[test]
    fn test_logto_config_deserialize() {
        let json = serde_json::json!({
            "client_id": "logto_abc",
            "client_secret": "logto_def",
            "endpoint": "https://logto.example.com"
        });
        let config: LogtoConfig = serde_json::from_value(json).checked("operation should succeed");
        assert_eq!(config.client_id, "logto_abc");
        assert_eq!(config.client_secret, "logto_def");
        assert_eq!(config.endpoint, "https://logto.example.com");
    }
}
