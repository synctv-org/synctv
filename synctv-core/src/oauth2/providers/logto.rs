//! Logto `OAuth2` provider

use super::{
    build_oauth2_http_client, build_provider_http_client, map_provider_http_error,
    validate_oauth2_redirect_url, validate_provider_url, validate_required_oauth2_field,
};
use crate::oauth2::{OAuth2Authorization, OAuth2UserInfo, Provider};
use crate::service::{OAuth2LogtoProviderConfig, OAuth2ProviderPrivateConfig};
use crate::{Error, InternalExt};
use async_trait::async_trait;
use oauth2::{
    basic::BasicClient, AuthUrl, ClientId, ClientSecret, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse, TokenUrl,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Logto `OAuth2` provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogtoConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
    pub endpoint: String,
}

/// Logto `OAuth2` provider
///
/// Supports multiple instances (e.g., logto1, logto2) with different endpoints.
/// Similar to Go's logtoProvider in synctv/internal/provider/providers/logto.go
pub struct LogtoProvider {
    client:
        Arc<BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>>,
    endpoint: String,
    oauth2_http_client: Arc<super::OAuth2HttpClient>,
    http_client: Arc<Client>,
}

#[derive(Deserialize)]
struct LogtoUser {
    sub: String,
    username: Option<String>,
    name: Option<String>,
    picture: Option<String>,
}

impl LogtoProvider {
    /// Create a new Logto provider with configuration
    ///
    /// # Errors
    /// Returns error if `redirect_url` or constructed endpoint URLs are not valid URLs.
    pub fn create(
        client_id: String,
        client_secret: String,
        redirect_url: String,
        endpoint: &str,
    ) -> Result<Self, Error> {
        Self::create_with_ssrf_guard(
            client_id,
            client_secret,
            redirect_url,
            endpoint,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
    }

    pub fn create_with_ssrf_guard(
        client_id: String,
        client_secret: String,
        redirect_url: String,
        endpoint: &str,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Self, Error> {
        let endpoint = endpoint.trim_end_matches('/');
        validate_provider_url(endpoint, "Invalid Logto endpoint", ssrf_guard)?;
        let auth_url_str = format!("{endpoint}/oidc/auth");
        let token_url_str = format!("{endpoint}/oidc/token");
        let userinfo_url = format!("{endpoint}/oidc/me");
        validate_provider_url(&auth_url_str, "Invalid Logto auth URL", ssrf_guard)?;
        validate_provider_url(&token_url_str, "Invalid Logto token URL", ssrf_guard)?;
        validate_provider_url(&userinfo_url, "Invalid Logto user info URL", ssrf_guard)?;
        let auth_url = AuthUrl::new(auth_url_str)
            .map_err(|e| Error::InvalidInput(format!("Invalid Logto auth URL: {e}")))?;
        let token_url = TokenUrl::new(token_url_str)
            .map_err(|e| Error::InvalidInput(format!("Invalid Logto token URL: {e}")))?;
        validate_oauth2_redirect_url(&redirect_url, "Invalid Logto redirect URL")?;
        let redirect = RedirectUrl::new(redirect_url)
            .map_err(|e| Error::InvalidInput(format!("Invalid Logto redirect URL: {e}")))?;
        let client = Arc::new(
            BasicClient::new(ClientId::new(client_id))
                .set_client_secret(ClientSecret::new(client_secret))
                .set_auth_uri(auth_url)
                .set_token_uri(token_url)
                .set_redirect_uri(redirect),
        );

        Ok(Self {
            client,
            endpoint: endpoint.to_string(),
            oauth2_http_client: build_oauth2_http_client(ssrf_guard)?,
            http_client: build_provider_http_client(ssrf_guard)?,
        })
    }
}

#[async_trait]
impl Provider for LogtoProvider {
    fn provider_type(&self) -> &'static str {
        "logto"
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
                RedirectUrl::new(redirect_url.to_string())
                    .map_err(|e| Error::InvalidInput(format!("Invalid Logto redirect URL: {e}")))?,
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
                RedirectUrl::new(redirect_url.to_string())
                    .map_err(|e| Error::InvalidInput(format!("Invalid Logto redirect URL: {e}")))?,
            ));
        }
        let token = request
            .request_async(self.oauth2_http_client.as_ref())
            .await
            .map_err(|err| map_provider_http_error("Failed to exchange code", err))?;

        // Fetch user info from Logto
        let resp = self
            .http_client
            .get(format!("{}/oidc/me", self.endpoint))
            .header(
                "Authorization",
                format!("Bearer {}", token.access_token().secret()),
            )
            .send()
            .await
            .map_err(|err| map_provider_http_error("Failed to fetch user info", err))?
            .error_for_status()
            .internal_with_err("Logto API error")?;

        let user: LogtoUser = resp
            .json()
            .await
            .internal_with_err("Failed to parse user info")?;

        let username = username_from_profile(user.username, user.name, &user.sub);

        Ok(OAuth2UserInfo {
            provider_user_id: user.sub,
            username,
            avatar: user.picture,
        })
    }
}

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
    validate_required_oauth2_field("Logto", "redirect_url", &config.redirect_url)?;
    validate_required_oauth2_field("Logto", "endpoint", &config.endpoint)?;
    Ok(Box::new(LogtoProvider::create_with_ssrf_guard(
        config.client_id.clone(),
        config.client_secret.clone(),
        config.redirect_url.clone(),
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
        redirect_url: &str,
        endpoint: &str,
    ) -> OAuth2ProviderPrivateConfig {
        OAuth2ProviderPrivateConfig::Logto(OAuth2LogtoProviderConfig {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            redirect_url: redirect_url.to_string(),
            endpoint: endpoint.to_string(),
        })
    }

    #[test]
    fn test_create_provider_valid_config() {
        let provider = LogtoProvider::create(
            "logto_client_id".to_string(),
            "logto_secret".to_string(),
            "https://example.com/callback".to_string(),
            "https://logto.example.com",
        );
        assert!(provider.is_ok());
    }

    #[test]
    fn test_create_provider_endpoint_trailing_slash_trimmed() {
        let provider = LogtoProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "https://logto.example.com/",
        )
        .checked("operation should succeed");
        // The endpoint should have trailing slash trimmed
        assert_eq!(provider.endpoint, "https://logto.example.com");
    }

    #[test]
    fn test_create_provider_invalid_redirect_url() {
        let result = LogtoProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "not a valid url".to_string(),
            "https://logto.example.com",
        );
        assert!(result.is_err());
        match result {
            Err(Error::InvalidInput(msg)) => assert!(msg.contains("redirect URL")),
            Ok(_) => std::panic::panic_any("Expected error but got Ok"),
            Err(e) => std::panic::panic_any(format!("Expected InvalidInput error, got: {e}")),
        }
    }

    #[test]
    fn test_create_provider_rejects_custom_scheme_redirect_url() {
        let result = LogtoProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "native-app://callback".to_string(),
            "https://logto.example.com",
        );
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn test_create_provider_invalid_endpoint() {
        let result = LogtoProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
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
            "https://example.com/cb".to_string(),
            "http://127.0.0.1:8443",
            &guard,
        );

        assert!(result.is_ok());
    }

    #[test]
    fn test_provider_type() {
        let provider = LogtoProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "https://logto.example.com",
        )
        .checked("operation should succeed");
        assert_eq!(provider.provider_type(), "logto");
    }

    #[tokio::test]
    async fn test_new_auth_url_contains_required_params() {
        let provider = LogtoProvider::create(
            "logto_test_id".to_string(),
            "test_secret".to_string(),
            "https://example.com/callback".to_string(),
            "https://auth.logto.io",
        )
        .checked("operation should succeed");

        let state = "logto_state_xyz";
        let auth = provider
            .new_auth_url(state, None)
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
            "https://example.com/cb".to_string(),
            "https://logto.example.com/",
        )
        .checked("operation should succeed");

        let auth_url = provider
            .new_auth_url("state", None)
            .await
            .checked("operation should succeed")
            .auth_url;
        // Should not have double slashes
        assert!(auth_url.starts_with("https://logto.example.com/oidc/auth"));
        assert!(!auth_url.contains("//oidc"));
    }

    #[test]
    fn test_factory_valid_config() {
        let config = logto_private_config(
            "logto_id",
            "logto_secret",
            "https://example.com/oauth/logto/callback",
            "https://logto.example.com",
        );
        let provider = logto_factory_from_private_config(
            &config,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        );
        assert!(provider.is_ok());
        assert_eq!(
            provider.checked("operation should succeed").provider_type(),
            "logto"
        );
    }

    #[test]
    fn test_factory_missing_endpoint() {
        let config = logto_private_config("id", "secret", "https://example.com/cb", "");
        let result = logto_factory_from_private_config(
            &config,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_factory_missing_fields() {
        let guard = synctv_common::ssrf::SsrfGuard::strict_policy();

        let config = logto_private_config(
            "",
            "secret",
            "https://example.com/cb",
            "https://logto.example.com",
        );
        assert!(logto_factory_from_private_config(&config, &guard).is_err());

        let config = logto_private_config(
            "id",
            "",
            "https://example.com/cb",
            "https://logto.example.com",
        );
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
            "redirect_url": "https://example.com/cb",
            "endpoint": "https://logto.example.com"
        });
        let config: LogtoConfig = serde_json::from_value(json).checked("operation should succeed");
        assert_eq!(config.client_id, "logto_abc");
        assert_eq!(config.client_secret, "logto_def");
        assert_eq!(config.redirect_url, "https://example.com/cb");
        assert_eq!(config.endpoint, "https://logto.example.com");
    }
}
