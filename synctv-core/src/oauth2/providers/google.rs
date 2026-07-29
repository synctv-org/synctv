//! Google `OAuth2` provider

use super::{
    build_oauth2_http_client, build_provider_http_client, map_provider_http_error,
    validate_oauth2_redirect_url, validate_required_oauth2_field,
};
use crate::oauth2::{OAuth2Authorization, OAuth2UserInfo, Provider};
use crate::service::{OAuth2GoogleProviderConfig, OAuth2ProviderPrivateConfig};
use crate::{Error, InternalExt};
use async_trait::async_trait;
use oauth2::{
    basic::BasicClient, AuthUrl, ClientId, ClientSecret, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse, TokenUrl,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Google `OAuth2` provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
}

/// Google `OAuth2` provider
pub struct GoogleProvider {
    client:
        Arc<BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>>,
    oauth2_http_client: Arc<super::OAuth2HttpClient>,
    http_client: Arc<Client>,
}

#[derive(Deserialize)]
struct GoogleUser {
    id: String,
    name: String,
    picture: Option<String>,
}

impl GoogleProvider {
    /// Create a new Google provider with configuration
    ///
    /// # Errors
    /// Returns error if `redirect_url` is not a valid URL.
    pub fn create(
        client_id: String,
        client_secret: String,
        redirect_url: String,
    ) -> Result<Self, Error> {
        Self::create_with_ssrf_guard(
            client_id,
            client_secret,
            redirect_url,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
    }

    pub fn create_with_ssrf_guard(
        client_id: String,
        client_secret: String,
        redirect_url: String,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Self, Error> {
        validate_oauth2_redirect_url(&redirect_url, "Invalid Google OAuth2 redirect URL")?;
        let redirect = RedirectUrl::new(redirect_url)
            .map_err(|e| Error::InvalidInput(format!("Invalid Google OAuth2 redirect URL: {e}")))?;
        let client = Arc::new(
            BasicClient::new(ClientId::new(client_id))
                .set_client_secret(ClientSecret::new(client_secret))
                .set_auth_uri(
                    AuthUrl::new("https://accounts.google.com/o/oauth2/v2/auth".to_string())
                        .map_err(|e| {
                            Error::InvalidInput(format!("Invalid Google auth URL: {e}"))
                        })?,
                )
                .set_token_uri(
                    TokenUrl::new("https://oauth2.googleapis.com/token".to_string()).map_err(
                        |e| Error::InvalidInput(format!("Invalid Google token URL: {e}")),
                    )?,
                )
                .set_redirect_uri(redirect),
        );

        Ok(Self {
            client,
            oauth2_http_client: build_oauth2_http_client(ssrf_guard)?,
            http_client: build_provider_http_client(ssrf_guard)?,
        })
    }
}

#[async_trait]
impl Provider for GoogleProvider {
    fn provider_type(&self) -> &'static str {
        "google"
    }

    async fn new_auth_url(
        &self,
        state: &str,
        redirect_url: Option<&str>,
    ) -> Result<OAuth2Authorization, Error> {
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
        let mut request = self
            .client
            .authorize_url(|| oauth2::CsrfToken::new(state.to_string()))
            .add_scope(Scope::new("openid".to_string()))
            .add_scope(Scope::new("profile".to_string()))
            .set_pkce_challenge(pkce_challenge);
        if let Some(redirect_url) = redirect_url {
            request = request.set_redirect_uri(std::borrow::Cow::Owned(
                RedirectUrl::new(redirect_url.to_string()).map_err(|e| {
                    Error::InvalidInput(format!("Invalid Google OAuth2 redirect URL: {e}"))
                })?,
            ));
        }
        let (auth_url, _csrf_token) = request.url();
        Ok(OAuth2Authorization::new(
            auth_url.to_string(),
            pkce_verifier.secret().clone(),
        ))
    }

    async fn get_user_info(
        &self,
        code: &str,
        redirect_url: Option<&str>,
        pkce_verifier: &str,
        _nonce: Option<&str>,
    ) -> Result<OAuth2UserInfo, Error> {
        // Exchange code for token with PKCE verifier
        let verifier = PkceCodeVerifier::new(pkce_verifier.to_string());
        let mut request = self
            .client
            .exchange_code(oauth2::AuthorizationCode::new(code.to_string()))
            .set_pkce_verifier(verifier);
        if let Some(redirect_url) = redirect_url {
            request = request.set_redirect_uri(std::borrow::Cow::Owned(
                RedirectUrl::new(redirect_url.to_string()).map_err(|e| {
                    Error::InvalidInput(format!("Invalid Google OAuth2 redirect URL: {e}"))
                })?,
            ));
        }
        let token = request
            .request_async(self.oauth2_http_client.as_ref())
            .await
            .map_err(|err| map_provider_http_error("Failed to exchange code", err))?;

        // Fetch user info
        let resp = self
            .http_client
            .get("https://www.googleapis.com/oauth2/v2/userinfo")
            .header(
                "Authorization",
                format!("Bearer {}", token.access_token().secret()),
            )
            .send()
            .await
            .map_err(|err| map_provider_http_error("Failed to fetch user info", err))?
            .error_for_status()
            .internal_with_err("Google API error")?;

        let user: GoogleUser = resp
            .json()
            .await
            .internal_with_err("Failed to parse user info")?;

        Ok(OAuth2UserInfo {
            provider_user_id: user.id,
            username: user.name,
            avatar: user.picture,
        })
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
    validate_required_oauth2_field("Google", "redirect_url", &config.redirect_url)?;
    Ok(Box::new(GoogleProvider::create_with_ssrf_guard(
        config.client_id.clone(),
        config.client_secret.clone(),
        config.redirect_url.clone(),
        ssrf_guard,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::TestResultExt;

    fn google_private_config(
        client_id: &str,
        client_secret: &str,
        redirect_url: &str,
    ) -> OAuth2ProviderPrivateConfig {
        OAuth2ProviderPrivateConfig::Google(OAuth2GoogleProviderConfig {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            redirect_url: redirect_url.to_string(),
        })
    }

    #[test]
    fn test_create_provider_valid_config() {
        let provider = GoogleProvider::create(
            "google_client_id".to_string(),
            "google_secret".to_string(),
            "https://example.com/callback".to_string(),
        );
        assert!(provider.is_ok());
    }

    #[test]
    fn test_create_provider_invalid_redirect_url() {
        let result = GoogleProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "not a valid url".to_string(),
        );
        assert!(result.is_err());
        match result {
            Err(Error::InvalidInput(msg)) => assert!(msg.contains("redirect URL")),
            Ok(_) => std::panic::panic_any("Expected error but got Ok"),
            Err(e) => std::panic::panic_any(format!("Expected InvalidInput error, got: {e}")),
        }
    }

    #[test]
    fn test_create_provider_empty_redirect_url() {
        let result = GoogleProvider::create("id".to_string(), "secret".to_string(), String::new());
        assert!(result.is_err());
        assert!(matches!(result.err(), Some(Error::InvalidInput(_))));
    }

    #[test]
    fn test_create_provider_rejects_custom_scheme_redirect_url() {
        let result = GoogleProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "native-app://callback".to_string(),
        );
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn test_provider_type() {
        let provider = GoogleProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
        )
        .checked("operation should succeed");
        assert_eq!(provider.provider_type(), "google");
    }

    #[tokio::test]
    async fn test_new_auth_url_contains_required_params() {
        let provider = GoogleProvider::create(
            "test_google_client_id".to_string(),
            "test_secret".to_string(),
            "https://example.com/callback".to_string(),
        )
        .checked("operation should succeed");

        let state = "google_state_123";
        let auth = provider
            .new_auth_url(state, None)
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
        let config = google_private_config(
            "google_id",
            "google_secret",
            "https://example.com/oauth/google/callback",
        );
        let provider = google_factory_from_private_config(
            &config,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        );
        assert!(provider.is_ok());
        assert_eq!(
            provider.checked("operation should succeed").provider_type(),
            "google"
        );
    }

    #[test]
    fn test_factory_missing_fields() {
        let guard = synctv_common::ssrf::SsrfGuard::strict_policy();

        let config = google_private_config("", "secret", "https://example.com/cb");
        assert!(google_factory_from_private_config(&config, &guard).is_err());

        let config = google_private_config("id", "", "https://example.com/cb");
        assert!(google_factory_from_private_config(&config, &guard).is_err());

        let config = google_private_config("id", "secret", "");
        assert!(google_factory_from_private_config(&config, &guard).is_err());
    }

    #[test]
    fn test_factory_invalid_redirect_url() {
        let config = google_private_config("id", "secret", "://bad");
        assert!(google_factory_from_private_config(
            &config,
            &synctv_common::ssrf::SsrfGuard::strict_policy()
        )
        .is_err());
    }

    #[test]
    fn test_google_config_deserialize() {
        let json = serde_json::json!({
            "client_id": "goog_abc",
            "client_secret": "goog_def",
            "redirect_url": "https://example.com/cb"
        });
        let config: GoogleConfig = serde_json::from_value(json).checked("operation should succeed");
        assert_eq!(config.client_id, "goog_abc");
        assert_eq!(config.client_secret, "goog_def");
        assert_eq!(config.redirect_url, "https://example.com/cb");
    }
}
