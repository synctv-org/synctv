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
use tokio::sync::OnceCell;

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

/// Discovered OIDC endpoints from .well-known/openid-configuration
#[derive(Debug, Clone, Deserialize)]
struct OidcDiscoveryDocument {
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    userinfo_endpoint: Option<String>,
}

/// Resolved OIDC client and endpoints, initialized lazily via discovery or static config.
struct ResolvedOidc {
    client: BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>,
    userinfo_url: Option<String>,
}

/// Generic OIDC provider
///
/// When created via `create()` (issuer-only mode), the OAuth2 client and endpoints
/// are resolved lazily on first use by fetching `{issuer}/.well-known/openid-configuration`.
/// When created via `create_with_endpoints()`, the provided endpoints are used directly.
pub struct OidcProvider {
    resolved: OnceCell<ResolvedOidc>,
    /// Stored config for lazy initialization (only used in issuer-only mode)
    init_config: OidcInitConfig,
    http_client: Arc<Client>,
}

/// Internal config stored for lazy OIDC discovery
struct OidcInitConfig {
    client_id: String,
    client_secret: String,
    redirect_url: String,
    issuer: String,
    /// If set, these are static overrides (no discovery needed)
    static_endpoints: Option<StaticEndpoints>,
}

struct StaticEndpoints {
    auth_url: String,
    token_url: String,
    userinfo_url: Option<String>,
}

impl OidcProvider {
    /// Create a new OIDC provider with issuer (uses .well-known discovery)
    ///
    /// Endpoints are discovered lazily on first use by fetching
    /// `{issuer}/.well-known/openid-configuration`.
    ///
    /// # Errors
    /// Returns error if the HTTP client cannot be built.
    pub fn create(
        client_id: String,
        client_secret: String,
        redirect_url: String,
        issuer: &str,
    ) -> Result<Self, Error> {
        let http_client = Arc::new(
            Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| Error::Internal(format!("Failed to build HTTP client: {e}")))?,
        );

        Ok(Self {
            resolved: OnceCell::new(),
            init_config: OidcInitConfig {
                client_id,
                client_secret,
                redirect_url,
                issuer: issuer.trim_end_matches('/').to_string(),
                static_endpoints: None,
            },
            http_client,
        })
    }

    /// Create a new OIDC provider with custom endpoints
    ///
    /// # Errors
    /// Returns error if the HTTP client cannot be built.
    pub fn create_with_endpoints(
        client_id: String,
        client_secret: String,
        redirect_url: String,
        issuer: &str,
        auth_url: Option<String>,
        token_url: Option<String>,
        userinfo_url: Option<String>,
    ) -> Result<Self, Error> {
        let issuer_trimmed = issuer.trim_end_matches('/');
        let http_client = Arc::new(
            Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| Error::Internal(format!("Failed to build HTTP client: {e}")))?,
        );

        Ok(Self {
            resolved: OnceCell::new(),
            init_config: OidcInitConfig {
                client_id,
                client_secret,
                redirect_url,
                issuer: issuer_trimmed.to_string(),
                static_endpoints: Some(StaticEndpoints {
                    auth_url: auth_url
                        .unwrap_or_else(|| format!("{issuer_trimmed}/authorize")),
                    token_url: token_url
                        .unwrap_or_else(|| format!("{issuer_trimmed}/token")),
                    userinfo_url,
                }),
            },
            http_client,
        })
    }

    /// Resolve the OAuth2 client, performing .well-known discovery if needed.
    async fn get_resolved(&self) -> Result<&ResolvedOidc, Error> {
        self.resolved
            .get_or_try_init(|| async {
                let config = &self.init_config;

                let (auth_url_str, token_url_str, userinfo_url) =
                    if let Some(static_ep) = &config.static_endpoints {
                        (
                            static_ep.auth_url.clone(),
                            static_ep.token_url.clone(),
                            static_ep.userinfo_url.clone(),
                        )
                    } else {
                        // Perform .well-known/openid-configuration discovery
                        let discovery_url = format!(
                            "{}/.well-known/openid-configuration",
                            config.issuer
                        );
                        tracing::info!(
                            "OIDC: fetching discovery document from {}",
                            discovery_url
                        );

                        let resp = self
                            .http_client
                            .get(&discovery_url)
                            .send()
                            .await
                            .map_err(|e| {
                                Error::Internal(format!(
                                    "Failed to fetch OIDC discovery document from {discovery_url}: {e}"
                                ))
                            })?
                            .error_for_status()
                            .map_err(|e| {
                                Error::Internal(format!(
                                    "OIDC discovery endpoint returned error: {e}"
                                ))
                            })?;

                        let doc: OidcDiscoveryDocument =
                            resp.json().await.map_err(|e| {
                                Error::Internal(format!(
                                    "Failed to parse OIDC discovery document: {e}"
                                ))
                            })?;

                        tracing::info!(
                            "OIDC: discovered endpoints: auth={}, token={}, userinfo={:?}",
                            doc.authorization_endpoint,
                            doc.token_endpoint,
                            doc.userinfo_endpoint
                        );

                        (
                            doc.authorization_endpoint,
                            doc.token_endpoint,
                            doc.userinfo_endpoint,
                        )
                    };

                let auth = AuthUrl::new(auth_url_str)
                    .map_err(|e| Error::InvalidInput(format!("Invalid OIDC auth URL: {e}")))?;
                let token = TokenUrl::new(token_url_str)
                    .map_err(|e| Error::InvalidInput(format!("Invalid OIDC token URL: {e}")))?;
                let redirect = RedirectUrl::new(config.redirect_url.clone())
                    .map_err(|e| Error::InvalidInput(format!("Invalid OIDC redirect URL: {e}")))?;

                let client = BasicClient::new(ClientId::new(config.client_id.clone()))
                    .set_client_secret(ClientSecret::new(config.client_secret.clone()))
                    .set_auth_uri(auth)
                    .set_token_uri(token)
                    .set_redirect_uri(redirect);

                Ok(ResolvedOidc {
                    client,
                    userinfo_url,
                })
            })
            .await
    }
}

#[async_trait]
impl Provider for OidcProvider {
    fn provider_type(&self) -> &'static str {
        "oidc"
    }

    async fn new_auth_url(&self, state: &str) -> Result<(String, String), Error> {
        let resolved = self.get_resolved().await?;
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
        let (auth_url, _csrf_token) = resolved
            .client
            .authorize_url(|| oauth2::CsrfToken::new(state.to_string()))
            .set_pkce_challenge(pkce_challenge)
            .url();
        Ok((auth_url.to_string(), pkce_verifier.secret().to_string()))
    }

    async fn get_user_info(&self, code: &str, pkce_verifier: &str) -> Result<OAuth2UserInfo, Error> {
        let resolved = self.get_resolved().await?;

        // Exchange code for token with PKCE verifier
        let verifier = PkceCodeVerifier::new(pkce_verifier.to_string());
        let token = resolved
            .client
            .exchange_code(oauth2::AuthorizationCode::new(code.to_string()))
            .set_pkce_verifier(verifier)
            .request_async(&oauth2::reqwest::Client::new())
            .await
            .map_err(|e| Error::Internal(format!("Failed to exchange code: {e}")))?;

        // Fetch user info from userinfo endpoint
        let userinfo_url = resolved
            .userinfo_url
            .as_ref()
            .ok_or_else(|| Error::Internal("userinfo_url not configured and not found in OIDC discovery".to_string()))?;

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
