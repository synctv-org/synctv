//! Google `OAuth2` provider

use super::oidc::{OidcEndpointOverrides, OidcProvider};
use super::{require_oauth2_redirect_url, validate_required_oauth2_field};
use crate::oauth2::{OAuth2Authorization, OAuth2AuthorizationMode, OAuth2UserInfo, Provider};
use crate::service::{OAuth2GoogleProviderConfig, OAuth2ProviderPrivateConfig};
use crate::Error;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Google `OAuth2` provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleConfig {
    pub client_id: String,
    pub client_secret: String,
}

/// Google `OAuth2` provider
pub struct GoogleProvider {
    oidc: OidcProvider,
}

impl GoogleProvider {
    /// Create a new Google provider with configuration
    ///
    /// # Errors
    pub fn create(client_id: String, client_secret: String) -> Result<Self, Error> {
        Self::create_with_ssrf_guard(
            client_id,
            client_secret,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
    }

    pub fn create_with_ssrf_guard(
        client_id: String,
        client_secret: String,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Self, Error> {
        let oidc = OidcProvider::create_with_endpoints_scopes_and_ssrf_guard(
            client_id,
            client_secret,
            "https://accounts.google.com",
            OidcEndpointOverrides {
                auth_url: Some("https://accounts.google.com/o/oauth2/v2/auth".to_string()),
                token_url: Some("https://oauth2.googleapis.com/token".to_string()),
                userinfo_url: Some("https://openidconnect.googleapis.com/v1/userinfo".to_string()),
                jwks_url: Some("https://www.googleapis.com/oauth2/v3/certs".to_string()),
            },
            vec!["openid".to_string(), "profile".to_string()],
            ssrf_guard,
        )?;
        Ok(Self { oidc })
    }
}

#[async_trait]
impl Provider for GoogleProvider {
    async fn new_auth_url(
        &self,
        state: &str,
        redirect_url: Option<&str>,
        mode: OAuth2AuthorizationMode,
    ) -> Result<OAuth2Authorization, Error> {
        let redirect_url = require_oauth2_redirect_url(redirect_url, "Google OAuth2")?;
        self.oidc
            .new_auth_url(state, Some(redirect_url), mode)
            .await
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

pub fn google_factory_from_private_config(
    config: &OAuth2ProviderPrivateConfig,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    let OAuth2ProviderPrivateConfig::Google(config) = config else {
        return Err(Error::InvalidInput(
            "Google provider requires google config".to_string(),
        ));
    };
    google_factory_from_basic_config(config, ssrf_guard)
}

fn google_factory_from_basic_config(
    config: &OAuth2GoogleProviderConfig,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    validate_required_oauth2_field("Google", "client_id", &config.client_id)?;
    validate_required_oauth2_field("Google", "client_secret", &config.client_secret)?;
    Ok(Box::new(GoogleProvider::create_with_ssrf_guard(
        config.client_id.clone(),
        config.client_secret.clone(),
        ssrf_guard,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::TestResultExt;

    fn google_private_config(client_id: &str, client_secret: &str) -> OAuth2ProviderPrivateConfig {
        OAuth2ProviderPrivateConfig::Google(OAuth2GoogleProviderConfig {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
        })
    }

    #[test]
    fn test_create_provider_valid_config() {
        let provider =
            GoogleProvider::create("google_client_id".to_string(), "google_secret".to_string());
        assert!(provider.is_ok());
    }

    #[tokio::test]
    async fn test_new_auth_url_contains_required_params() {
        let provider = GoogleProvider::create(
            "test_google_client_id".to_string(),
            "test_secret".to_string(),
        )
        .checked("operation should succeed");

        let state = "google_state_123";
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

        // Auth URL should contain the Google authorize endpoint
        assert!(auth_url.starts_with("https://accounts.google.com/o/oauth2/v2/auth"));
        // Auth URL should contain client_id
        assert!(auth_url.contains("client_id=test_google_client_id"));
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

    #[test]
    fn test_factory_valid_config() {
        let config = google_private_config("google_id", "google_secret");
        let provider = google_factory_from_private_config(
            &config,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        );
        assert!(provider.is_ok());
    }

    #[test]
    fn test_factory_missing_fields() {
        let guard = synctv_common::ssrf::SsrfGuard::strict_policy();

        let config = google_private_config("", "secret");
        assert!(google_factory_from_private_config(&config, &guard).is_err());

        let config = google_private_config("id", "");
        assert!(google_factory_from_private_config(&config, &guard).is_err());
    }

    #[test]
    fn test_new_auth_url_requires_redirect_url() {
        let provider = GoogleProvider::create("id".to_string(), "secret".to_string())
            .checked("provider should be created");
        let result = futures::executor::block_on(provider.new_auth_url(
            "state",
            None,
            OAuth2AuthorizationMode::Browser,
        ));
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn test_google_config_deserialize() {
        let json = serde_json::json!({
            "client_id": "goog_abc",
            "client_secret": "goog_def"
        });
        let config: GoogleConfig = serde_json::from_value(json).checked("operation should succeed");
        assert_eq!(config.client_id, "goog_abc");
        assert_eq!(config.client_secret, "goog_def");
    }
}
