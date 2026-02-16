//! Generic OIDC provider

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

/// OIDC provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OidcConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
    #[serde(default)]
    pub issuer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub userinfo_url: Option<String>,
}

/// Generic OIDC provider
pub struct OidcProvider {
    client: Arc<BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>>,
    userinfo_url: Option<String>,
    http_client: Arc<Client>,
}

impl OidcProvider {
    /// Create a new OIDC provider with issuer (uses .well-known discovery)
    ///
    /// # Errors
    /// Returns error if `redirect_url` or constructed issuer URLs are not valid URLs.
    pub fn create(
        client_id: String,
        client_secret: String,
        redirect_url: String,
        issuer: &str,
    ) -> Result<Self, Error> {
        let issuer = issuer.trim_end_matches('/');
        let auth_url = AuthUrl::new(format!("{issuer}/authorize"))
            .map_err(|e| Error::InvalidInput(format!("Invalid OIDC auth URL: {e}")))?;
        let token_url = TokenUrl::new(format!("{issuer}/token"))
            .map_err(|e| Error::InvalidInput(format!("Invalid OIDC token URL: {e}")))?;
        let redirect = RedirectUrl::new(redirect_url)
            .map_err(|e| Error::InvalidInput(format!("Invalid OIDC redirect URL: {e}")))?;
        let client = Arc::new(
            BasicClient::new(ClientId::new(client_id))
                .set_client_secret(ClientSecret::new(client_secret))
                .set_auth_uri(auth_url)
                .set_token_uri(token_url)
                .set_redirect_uri(redirect),
        );

        Ok(Self {
            client,
            userinfo_url: Some(format!("{issuer}/userinfo")),
            http_client: Arc::new(Client::new()),
        })
    }

    /// Create a new OIDC provider with custom endpoints
    ///
    /// # Errors
    /// Returns error if any of the provided URLs are not valid.
    pub fn create_with_endpoints(
        client_id: String,
        client_secret: String,
        redirect_url: String,
        issuer: &str,
        auth_url: Option<String>,
        token_url: Option<String>,
        userinfo_url: Option<String>,
    ) -> Result<Self, Error> {
        let auth = AuthUrl::new(
            auth_url.unwrap_or_else(|| format!("{}/authorize", issuer.trim_end_matches('/'))),
        )
        .map_err(|e| Error::InvalidInput(format!("Invalid OIDC auth URL: {e}")))?;
        let token = TokenUrl::new(
            token_url.unwrap_or_else(|| format!("{}/token", issuer.trim_end_matches('/'))),
        )
        .map_err(|e| Error::InvalidInput(format!("Invalid OIDC token URL: {e}")))?;
        let redirect = RedirectUrl::new(redirect_url)
            .map_err(|e| Error::InvalidInput(format!("Invalid OIDC redirect URL: {e}")))?;
        let client = Arc::new(
            BasicClient::new(ClientId::new(client_id))
                .set_client_secret(ClientSecret::new(client_secret))
                .set_auth_uri(auth)
                .set_token_uri(token)
                .set_redirect_uri(redirect),
        );

        Ok(Self {
            client,
            userinfo_url,
            http_client: Arc::new(Client::new()),
        })
    }
}

#[async_trait]
impl Provider for OidcProvider {
    fn provider_type(&self) -> &'static str {
        "oidc"
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

        // Fetch user info from userinfo endpoint
        let userinfo_url = self
            .userinfo_url
            .as_ref()
            .ok_or_else(|| Error::Internal("userinfo_url not configured".to_string()))?;

        let resp = self
            .http_client
            .get(userinfo_url)
            .header("Authorization", format!("Bearer {}", token.access_token().secret()))
            .send()
            .await
            .map_err(|e| Error::Internal(format!("Failed to fetch user info: {e}")))?
            .error_for_status()
            .map_err(|e| Error::Internal(format!("OIDC API error: {e}")))?;

        #[derive(Deserialize)]
        struct OidcUser {
            sub: String,
            name: Option<String>,
            email: Option<String>,
            picture: Option<String>,
        }

        let user: OidcUser = resp
            .json()
            .await
            .map_err(|e| Error::Internal(format!("Failed to parse user info: {e}")))?;

        Ok(OAuth2UserInfo {
            provider_user_id: user.sub,
            username: user.name.unwrap_or_default(),
            email: user.email,
            avatar: user.picture,
        })
    }
}

/// Factory function for OIDC provider
pub fn oidc_factory(config: &serde_json::Value) -> Result<Box<dyn Provider>, Error> {
    let config: OidcConfig = serde_json::from_value(config.clone())
        .map_err(|e| Error::InvalidInput(format!("Invalid OIDC config: {e}")))?;

    // Use create_with_endpoints if any custom endpoint is specified
    let provider = if config.auth_url.is_some()
        || config.token_url.is_some()
        || config.userinfo_url.is_some()
    {
        OidcProvider::create_with_endpoints(
            config.client_id,
            config.client_secret,
            config.redirect_url,
            &config.issuer,
            config.auth_url,
            config.token_url,
            config.userinfo_url,
        )?
    } else {
        OidcProvider::create(
            config.client_id,
            config.client_secret,
            config.redirect_url,
            &config.issuer,
        )?
    };

    Ok(Box::new(provider))
}
