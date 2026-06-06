//! GitHub `OAuth2` provider

use super::{
    build_oauth2_http_client, build_provider_http_client, map_provider_http_error,
    validate_oauth2_redirect_url,
};
use crate::oauth2::{OAuth2Authorization, OAuth2UserInfo, Provider};
use crate::{Error, InternalExt};
use async_trait::async_trait;
use oauth2::{
    basic::BasicClient, AuthUrl, ClientId, ClientSecret, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse, TokenUrl,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// GitHub `OAuth2` provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
}

/// GitHub `OAuth2` provider
pub struct GitHubProvider {
    client:
        Arc<BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>>,
    oauth2_http_client: Arc<super::OAuth2HttpClient>,
    http_client: Arc<Client>,
}

#[derive(Deserialize)]
struct GitHubUser {
    login: String,
    id: u64,
    email: Option<String>,
    avatar_url: Option<String>,
}

impl GitHubProvider {
    /// Create a new GitHub provider with configuration
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
        validate_oauth2_redirect_url(&redirect_url, "Invalid GitHub OAuth2 redirect URL")?;
        let redirect = RedirectUrl::new(redirect_url)
            .map_err(|e| Error::InvalidInput(format!("Invalid GitHub OAuth2 redirect URL: {e}")))?;
        let client = Arc::new(
            BasicClient::new(ClientId::new(client_id))
                .set_client_secret(ClientSecret::new(client_secret))
                .set_auth_uri(
                    AuthUrl::new("https://github.com/login/oauth/authorize".to_string()).map_err(
                        |e| Error::InvalidInput(format!("Invalid GitHub auth URL: {e}")),
                    )?,
                )
                .set_token_uri(
                    TokenUrl::new("https://github.com/login/oauth/access_token".to_string())
                        .map_err(|e| {
                            Error::InvalidInput(format!("Invalid GitHub token URL: {e}"))
                        })?,
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

impl GitHubProvider {
    /// Fetch the user's primary verified email from the GitHub `/user/emails` API.
    ///
    /// Returns `(Some(email), true)` if a primary+verified email is found,
    /// `(Some(email), false)` if only a primary (unverified) email is found,
    /// or an error if the API call fails.
    async fn fetch_verified_email(
        &self,
        access_token: &str,
    ) -> Result<(Option<String>, bool), Error> {
        #[derive(Deserialize)]
        struct GitHubEmail {
            email: String,
            primary: bool,
            verified: bool,
        }

        let resp = self
            .http_client
            .get("https://api.github.com/user/emails")
            .header("Authorization", format!("Bearer {access_token}"))
            .header("User-Agent", "synctv-rs")
            .send()
            .await
            .map_err(|err| map_provider_http_error("Failed to fetch user emails", err))?
            .error_for_status()
            .internal_with_err("GitHub emails API error")?;

        let emails: Vec<GitHubEmail> = resp
            .json()
            .await
            .internal_with_err("Failed to parse email list")?;

        // Prefer the primary + verified email
        if let Some(e) = emails.iter().find(|e| e.primary && e.verified) {
            return Ok((Some(e.email.clone()), true));
        }

        // Fall back to primary (unverified)
        if let Some(e) = emails.iter().find(|e| e.primary) {
            return Ok((Some(e.email.clone()), false));
        }

        // No primary email found
        Ok((None, false))
    }
}

#[async_trait]
impl Provider for GitHubProvider {
    fn provider_type(&self) -> &'static str {
        "github"
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
            .add_scope(Scope::new("user:email".to_string()))
            .set_pkce_challenge(pkce_challenge);
        if let Some(redirect_url) = redirect_url {
            request = request.set_redirect_uri(std::borrow::Cow::Owned(
                RedirectUrl::new(redirect_url.to_string()).map_err(|e| {
                    Error::InvalidInput(format!("Invalid GitHub OAuth2 redirect URL: {e}"))
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
                    Error::InvalidInput(format!("Invalid GitHub OAuth2 redirect URL: {e}"))
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
            .get("https://api.github.com/user")
            .header(
                "Authorization",
                format!("Bearer {}", token.access_token().secret()),
            )
            .header("User-Agent", "synctv-rs")
            .send()
            .await
            .map_err(|err| map_provider_http_error("Failed to fetch user info", err))?
            .error_for_status()
            .internal_with_err("GitHub API error")?;

        let user: GitHubUser = resp
            .json()
            .await
            .internal_with_err("Failed to parse user info")?;
        // Fetch verified email from the /user/emails endpoint.
        // The /user endpoint may return an email, but does not indicate
        // whether it is verified. We must call /user/emails to get the
        // actual verification status.
        // Fallback rules:
        // - API succeeds and a verified email exists: use it as verified.
        // - API succeeds but no verified email exists: use the primary email as unverified.
        // - API fails and the profile has no email: return an error.
        // - API fails and the profile has an email: use it as unverified and warn.
        let (email, email_verified) = match self
            .fetch_verified_email(token.access_token().secret())
            .await
        {
            Ok((maybe_email, verified)) => (maybe_email, verified),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    github_user_id = %user.id,
                    "Failed to fetch GitHub verified email from /user/emails API"
                );
                match user.email {
                    None => {
                        // Cannot determine email ownership — refuse to create an account.
                        return Err(Error::Internal(
                            "Could not retrieve GitHub email address. \
                             Please ensure your GitHub account has a public or verified \
                             email address and try again."
                                .to_string(),
                        ));
                    }
                    Some(fallback_email) => {
                        // Profile email is available but GitHub did not provide a trusted
                        // primary email claim for this response.
                        tracing::warn!(
                            "Using GitHub profile email as unverified fallback — \
                             email bind may be required."
                        );
                        (Some(fallback_email), false)
                    }
                }
            }
        };

        Ok(OAuth2UserInfo {
            provider_user_id: user.id.to_string(),
            username: user.login,
            email,
            avatar: user.avatar_url,
            email_verified,
        })
    }
}

/// Factory function for GitHub provider
pub fn github_factory(config: &serde_json::Value) -> Result<Box<dyn Provider>, Error> {
    github_factory_with_ssrf_guard(config, &synctv_common::ssrf::SsrfGuard::strict_policy())
}

pub fn github_factory_with_ssrf_guard(
    config: &serde_json::Value,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    let config: GitHubConfig = serde_json::from_value(config.clone())
        .map_err(|e| Error::InvalidInput(format!("Invalid GitHub config: {e}")))?;

    Ok(Box::new(GitHubProvider::create_with_ssrf_guard(
        config.client_id,
        config.client_secret,
        config.redirect_url,
        ssrf_guard,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_provider_valid_config() {
        let provider = GitHubProvider::create(
            "my_client_id".to_string(),
            "my_secret".to_string(),
            "https://example.com/callback".to_string(),
        );
        assert!(provider.is_ok());
    }

    #[test]
    fn test_create_provider_invalid_redirect_url() {
        let result = GitHubProvider::create(
            "my_client_id".to_string(),
            "my_secret".to_string(),
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
        let result = GitHubProvider::create("id".to_string(), "secret".to_string(), String::new());
        assert!(result.is_err());
        assert!(matches!(result.err(), Some(Error::InvalidInput(_))));
    }

    #[test]
    fn test_create_provider_rejects_custom_scheme_redirect_url() {
        let result = GitHubProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "native-app://callback".to_string(),
        );
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn test_provider_type() {
        let provider = GitHubProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
        )
        .unwrap();
        assert_eq!(provider.provider_type(), "github");
    }

    #[tokio::test]
    async fn test_new_auth_url_contains_required_params() {
        let provider = GitHubProvider::create(
            "test_client_id".to_string(),
            "test_secret".to_string(),
            "https://example.com/callback".to_string(),
        )
        .unwrap();

        let state = "random_state_value";
        let auth = provider.new_auth_url(state, None).await.unwrap();
        let auth_url = auth.auth_url;
        let pkce_verifier = auth.pkce_verifier;

        // Auth URL should contain the GitHub authorize endpoint
        assert!(auth_url.starts_with("https://github.com/login/oauth/authorize"));
        // Auth URL should contain client_id
        assert!(auth_url.contains("client_id=test_client_id"));
        // Auth URL should contain state
        assert!(auth_url.contains(&format!("state={state}")));
        // Auth URL should contain redirect_uri (URL-encoded)
        assert!(auth_url.contains("redirect_uri="));
        assert!(auth_url.contains("scope=user%3Aemail"));
        // Auth URL should contain PKCE code_challenge
        assert!(auth_url.contains("code_challenge="));
        assert!(auth_url.contains("code_challenge_method=S256"));
        // PKCE verifier should be non-empty
        assert!(!pkce_verifier.is_empty());
    }

    #[tokio::test]
    async fn test_new_auth_url_different_states_produce_different_urls() {
        let provider = GitHubProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
        )
        .unwrap();

        let auth1 = provider.new_auth_url("state1", None).await.unwrap();
        let auth2 = provider.new_auth_url("state2", None).await.unwrap();

        // Different states should produce different URLs
        assert_ne!(auth1.auth_url, auth2.auth_url);
        // Each call generates a new random PKCE verifier
        assert_ne!(auth1.pkce_verifier, auth2.pkce_verifier);
    }

    #[test]
    fn test_factory_valid_config() {
        let config = serde_json::json!({
            "client_id": "gh_id",
            "client_secret": "gh_secret",
            "redirect_url": "https://example.com/oauth/github/callback"
        });
        let provider = github_factory(&config);
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().provider_type(), "github");
    }

    #[test]
    fn test_factory_missing_client_id() {
        let config = serde_json::json!({
            "client_secret": "secret",
            "redirect_url": "https://example.com/cb"
        });
        let result = github_factory(&config);
        assert!(result.is_err());
        assert!(matches!(result.err(), Some(Error::InvalidInput(_))));
    }

    #[test]
    fn test_factory_missing_client_secret() {
        let config = serde_json::json!({
            "client_id": "id",
            "redirect_url": "https://example.com/cb"
        });
        let result = github_factory(&config);
        assert!(result.is_err());
    }

    #[test]
    fn test_factory_missing_redirect_url() {
        let config = serde_json::json!({
            "client_id": "id",
            "client_secret": "secret"
        });
        let result = github_factory(&config);
        assert!(result.is_err());
    }

    #[test]
    fn test_factory_empty_json() {
        let config = serde_json::json!({});
        let result = github_factory(&config);
        assert!(result.is_err());
        assert!(matches!(result.err(), Some(Error::InvalidInput(_))));
    }

    #[test]
    fn test_factory_invalid_redirect_url() {
        let config = serde_json::json!({
            "client_id": "id",
            "client_secret": "secret",
            "redirect_url": "://invalid"
        });
        let result = github_factory(&config);
        assert!(result.is_err());
    }

    #[test]
    fn test_github_config_deserialize() {
        let json = serde_json::json!({
            "client_id": "abc123",
            "client_secret": "def456",
            "redirect_url": "https://example.com/cb"
        });
        let config: GitHubConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.client_id, "abc123");
        assert_eq!(config.client_secret, "def456");
        assert_eq!(config.redirect_url, "https://example.com/cb");
    }
}
