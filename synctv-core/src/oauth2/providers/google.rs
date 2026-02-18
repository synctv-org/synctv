//! Google `OAuth2` provider

use crate::oauth2::{Provider, OAuth2UserInfo};
use crate::{Error, InternalExt};
use async_trait::async_trait;
use oauth2::{
    basic::BasicClient,
    AuthUrl, ClientId, ClientSecret, EndpointSet, EndpointNotSet, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, TokenUrl, TokenResponse,
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
    client: Arc<BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>>,
    http_client: Arc<Client>,
}

impl GoogleProvider {
    /// Create a new Google provider with configuration
    ///
    /// # Errors
    /// Returns error if `redirect_url` is not a valid URL.
    pub fn create(client_id: String, client_secret: String, redirect_url: String) -> Result<Self, Error> {
        let redirect = RedirectUrl::new(redirect_url)
            .map_err(|e| Error::InvalidInput(format!("Invalid Google OAuth2 redirect URL: {e}")))?;
        let client = Arc::new(
            BasicClient::new(ClientId::new(client_id))
                .set_client_secret(ClientSecret::new(client_secret))
                .set_auth_uri(AuthUrl::new("https://accounts.google.com/o/oauth2/v2/auth".to_string()).expect("valid Google auth URL"))
                .set_token_uri(TokenUrl::new("https://oauth2.googleapis.com/token".to_string()).expect("valid Google token URL"))
                .set_redirect_uri(redirect),
        );

        Ok(Self {
            client,
            http_client: Arc::new(
                Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .internal_with_err("Failed to build HTTP client")?
            ),
        })
    }
}

#[async_trait]
impl Provider for GoogleProvider {
    fn provider_type(&self) -> &'static str {
        "google"
    }

    async fn new_auth_url(&self, state: &str) -> Result<(String, String), Error> {
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
        let (auth_url, _csrf_token) = self
            .client
            .authorize_url(|| oauth2::CsrfToken::new(state.to_string()))
            .set_pkce_challenge(pkce_challenge)
            .url();
        Ok((auth_url.to_string(), pkce_verifier.secret().clone()))
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
            .internal_with_err("Failed to exchange code")?;

        // Fetch user info
        let resp = self
            .http_client
            .get("https://www.googleapis.com/oauth2/v2/userinfo")
            .header("Authorization", format!("Bearer {}", token.access_token().secret()))
            .send()
            .await
            .internal_with_err("Failed to fetch user info")?
            .error_for_status()
            .internal_with_err("Google API error")?;

        #[derive(Deserialize)]
        struct GoogleUser {
            id: String,
            name: String,
            email: String,
            #[serde(default)]
            verified_email: bool,
            picture: Option<String>,
        }

        let user: GoogleUser = resp
            .json()
            .await
            .internal_with_err("Failed to parse user info")?;

        Ok(OAuth2UserInfo {
            provider_user_id: user.id,
            username: user.name,
            email: Some(user.email),
            avatar: user.picture,
            email_verified: user.verified_email,
        })
    }
}

/// Factory function for Google provider
pub fn google_factory(config: &serde_json::Value) -> Result<Box<dyn Provider>, Error> {
    let config: GoogleConfig = serde_json::from_value(config.clone())
        .map_err(|e| Error::InvalidInput(format!("Invalid Google config: {e}")))?;

    Ok(Box::new(GoogleProvider::create(
        config.client_id,
        config.client_secret,
        config.redirect_url,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== Provider Creation ====================

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
            Ok(_) => panic!("Expected error but got Ok"),
            Err(e) => panic!("Expected InvalidInput error, got: {e}"),
        }
    }

    #[test]
    fn test_create_provider_empty_redirect_url() {
        let result = GoogleProvider::create(
            "id".to_string(),
            "secret".to_string(),
            String::new(),
        );
        assert!(result.is_err());
        assert!(matches!(result.err(), Some(Error::InvalidInput(_))));
    }

    // ==================== Provider Type ====================

    #[test]
    fn test_provider_type() {
        let provider = GoogleProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
        )
        .unwrap();
        assert_eq!(provider.provider_type(), "google");
    }

    // ==================== Auth URL Generation ====================

    #[tokio::test]
    async fn test_new_auth_url_contains_required_params() {
        let provider = GoogleProvider::create(
            "test_google_client_id".to_string(),
            "test_secret".to_string(),
            "https://example.com/callback".to_string(),
        )
        .unwrap();

        let state = "google_state_123";
        let (auth_url, pkce_verifier) = provider.new_auth_url(state).await.unwrap();

        // Auth URL should contain the Google authorize endpoint
        assert!(auth_url.starts_with("https://accounts.google.com/o/oauth2/v2/auth"));
        // Auth URL should contain client_id
        assert!(auth_url.contains("client_id=test_google_client_id"));
        // Auth URL should contain state
        assert!(auth_url.contains(&format!("state={state}")));
        // Auth URL should contain redirect_uri
        assert!(auth_url.contains("redirect_uri="));
        // Auth URL should contain PKCE code_challenge
        assert!(auth_url.contains("code_challenge="));
        assert!(auth_url.contains("code_challenge_method=S256"));
        // PKCE verifier should be non-empty
        assert!(!pkce_verifier.is_empty());
    }

    #[tokio::test]
    async fn test_new_auth_url_different_states() {
        let provider = GoogleProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
        )
        .unwrap();

        let (url1, v1) = provider.new_auth_url("state_a").await.unwrap();
        let (url2, v2) = provider.new_auth_url("state_b").await.unwrap();

        assert_ne!(url1, url2);
        assert_ne!(v1, v2);
    }

    // ==================== Factory Function ====================

    #[test]
    fn test_factory_valid_config() {
        let config = serde_json::json!({
            "client_id": "google_id",
            "client_secret": "google_secret",
            "redirect_url": "https://example.com/oauth/google/callback"
        });
        let provider = google_factory(&config);
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().provider_type(), "google");
    }

    #[test]
    fn test_factory_missing_fields() {
        // Missing client_id
        let config = serde_json::json!({
            "client_secret": "secret",
            "redirect_url": "https://example.com/cb"
        });
        assert!(google_factory(&config).is_err());

        // Missing client_secret
        let config = serde_json::json!({
            "client_id": "id",
            "redirect_url": "https://example.com/cb"
        });
        assert!(google_factory(&config).is_err());

        // Missing redirect_url
        let config = serde_json::json!({
            "client_id": "id",
            "client_secret": "secret"
        });
        assert!(google_factory(&config).is_err());
    }

    #[test]
    fn test_factory_empty_json() {
        let config = serde_json::json!({});
        let result = google_factory(&config);
        assert!(result.is_err());
        assert!(matches!(result.err(), Some(Error::InvalidInput(_))));
    }

    #[test]
    fn test_factory_invalid_redirect_url() {
        let config = serde_json::json!({
            "client_id": "id",
            "client_secret": "secret",
            "redirect_url": "://bad"
        });
        assert!(google_factory(&config).is_err());
    }

    // ==================== Config Deserialization ====================

    #[test]
    fn test_google_config_deserialize() {
        let json = serde_json::json!({
            "client_id": "goog_abc",
            "client_secret": "goog_def",
            "redirect_url": "https://example.com/cb"
        });
        let config: GoogleConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.client_id, "goog_abc");
        assert_eq!(config.client_secret, "goog_def");
        assert_eq!(config.redirect_url, "https://example.com/cb");
    }

    #[test]
    fn test_google_config_serialize_roundtrip() {
        let config = GoogleConfig {
            client_id: "id".to_string(),
            client_secret: "secret".to_string(),
            redirect_url: "https://example.com/cb".to_string(),
        };
        let json = serde_json::to_value(&config).unwrap();
        let deserialized: GoogleConfig = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.client_id, config.client_id);
        assert_eq!(deserialized.client_secret, config.client_secret);
        assert_eq!(deserialized.redirect_url, config.redirect_url);
    }
}
