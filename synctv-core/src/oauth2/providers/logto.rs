//! Logto `OAuth2` provider

use crate::oauth2::{Provider, OAuth2UserInfo};
use crate::Error;
use async_trait::async_trait;
use oauth2::{
    basic::BasicClient,
    AuthUrl, ClientId, ClientSecret, EndpointSet, EndpointNotSet, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, TokenUrl, TokenResponse,
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
    client: Arc<BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>>,
    endpoint: String,
    http_client: Arc<Client>,
}

impl LogtoProvider {
    /// Create a new Logto provider with configuration
    ///
    /// # Errors
    /// Returns error if `redirect_url` or constructed endpoint URLs are not valid URLs.
    pub fn create(client_id: String, client_secret: String, redirect_url: String, endpoint: &str) -> Result<Self, Error> {
        let endpoint = endpoint.trim_end_matches('/');
        let auth_url = AuthUrl::new(format!("{endpoint}/oidc/auth"))
            .map_err(|e| Error::InvalidInput(format!("Invalid Logto auth URL: {e}")))?;
        let token_url = TokenUrl::new(format!("{endpoint}/oidc/token"))
            .map_err(|e| Error::InvalidInput(format!("Invalid Logto token URL: {e}")))?;
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
            http_client: Arc::new(
                Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|e| Error::Internal(format!("Failed to build HTTP client: {e}")))?
            ),
        })
    }
}

#[async_trait]
impl Provider for LogtoProvider {
    fn provider_type(&self) -> &'static str {
        "logto"
    }

    async fn new_auth_url(&self, state: &str) -> Result<(String, String), Error> {
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
        let (auth_url, _csrf_token) = self
            .client
            .authorize_url(|| oauth2::CsrfToken::new(state.to_string()))
            .set_pkce_challenge(pkce_challenge)
            .url();
        Ok((auth_url.to_string(), pkce_verifier.secret().to_string()))
    }

    async fn get_user_info(&self, code: &str, pkce_verifier: &str) -> Result<OAuth2UserInfo, Error> {
        // Exchange code for token with PKCE verifier
        let verifier = PkceCodeVerifier::new(pkce_verifier.to_string());
        let token = self
            .client
            .exchange_code(oauth2::AuthorizationCode::new(code.to_string()))
            .set_pkce_verifier(verifier)
            .request_async(&oauth2::reqwest::Client::new())
            .await
            .map_err(|e| Error::Internal(format!("Failed to exchange code: {e}")))?;

        // Fetch user info from Logto
        let resp = self
            .http_client
            .get(format!("{}/oidc/me", self.endpoint))
            .header("Authorization", format!("Bearer {}", token.access_token().secret()))
            .send()
            .await
            .map_err(|e| Error::Internal(format!("Failed to fetch user info: {e}")))?
            .error_for_status()
            .map_err(|e| Error::Internal(format!("Logto API error: {e}")))?;

        #[derive(Deserialize)]
        struct LogtoUser {
            sub: String,
            username: Option<String>,
            name: Option<String>,
            email: Option<String>,
            picture: Option<String>,
        }

        let user: LogtoUser = resp
            .json()
            .await
            .map_err(|e| Error::Internal(format!("Failed to parse user info: {e}")))?;

        let username = user.username.or(user.name).unwrap_or_default();

        Ok(OAuth2UserInfo {
            provider_user_id: user.sub,
            username,
            email: user.email,
            avatar: user.picture,
        })
    }
}

/// Factory function for Logto provider
pub fn logto_factory(config: &serde_json::Value) -> Result<Box<dyn Provider>, Error> {
    let config: LogtoConfig = serde_json::from_value(config.clone())
        .map_err(|e| Error::InvalidInput(format!("Invalid Logto config: {e}")))?;

    Ok(Box::new(LogtoProvider::create(
        config.client_id,
        config.client_secret,
        config.redirect_url,
        &config.endpoint,
    )?))
}
