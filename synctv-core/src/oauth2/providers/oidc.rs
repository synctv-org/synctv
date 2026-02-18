//! Generic OIDC provider

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
                .internal_with_err("Failed to build HTTP client")?,
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
                .internal_with_err("Failed to build HTTP client")?,
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
            .internal_with_err("Failed to exchange code")?;

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
            .internal_with_err("Failed to fetch user info")?
            .error_for_status()
            .internal_with_err("OIDC API error")?;

        #[derive(Deserialize)]
        struct OidcUser {
            sub: String,
            name: Option<String>,
            email: Option<String>,
            #[serde(default)]
            email_verified: bool,
            picture: Option<String>,
        }

        let user: OidcUser = resp
            .json()
            .await
            .internal_with_err("Failed to parse user info")?;

        Ok(OAuth2UserInfo {
            provider_user_id: user.sub,
            username: user.name.unwrap_or_default(),
            email: user.email,
            avatar: user.picture,
            email_verified: user.email_verified,
        })
    }
}

/// Factory function for OIDC provider
pub fn oidc_factory(config: &serde_json::Value) -> Result<Box<dyn Provider>, Error> {
    let config: OidcConfig = serde_json::from_value(config.clone())
        .map_err(|e| Error::InvalidInput(format!("Invalid OIDC config: {e}")))?;

    // Validate issuer is not empty when no custom endpoints are provided.
    // An empty issuer means .well-known discovery will fail at runtime with
    // an unhelpful "/.well-known/openid-configuration" URL.
    let has_custom_endpoints = config.auth_url.is_some()
        || config.token_url.is_some()
        || config.userinfo_url.is_some();
    if config.issuer.is_empty() && !has_custom_endpoints {
        return Err(Error::InvalidInput(
            "OIDC provider requires a non-empty 'issuer' URL for .well-known discovery, \
             or explicit 'auth_url' and 'token_url' endpoints"
                .to_string(),
        ));
    }

    // Use create_with_endpoints if any custom endpoint is specified
    let provider = if has_custom_endpoints
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

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== Provider Creation (Issuer-Only / Discovery Mode) ====================

    #[test]
    fn test_create_provider_issuer_only() {
        let provider = OidcProvider::create(
            "oidc_client_id".to_string(),
            "oidc_secret".to_string(),
            "https://example.com/callback".to_string(),
            "https://issuer.example.com",
        );
        assert!(provider.is_ok());
    }

    #[test]
    fn test_create_provider_issuer_trailing_slash_trimmed() {
        let provider = OidcProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "https://issuer.example.com/",
        )
        .unwrap();
        assert_eq!(provider.init_config.issuer, "https://issuer.example.com");
    }

    // ==================== Provider Creation (Static Endpoints) ====================

    #[test]
    fn test_create_with_endpoints_all_specified() {
        let provider = OidcProvider::create_with_endpoints(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "https://issuer.example.com",
            Some("https://issuer.example.com/authorize".to_string()),
            Some("https://issuer.example.com/token".to_string()),
            Some("https://issuer.example.com/userinfo".to_string()),
        );
        assert!(provider.is_ok());
        let p = provider.unwrap();
        let endpoints = p.init_config.static_endpoints.as_ref().unwrap();
        assert_eq!(endpoints.auth_url, "https://issuer.example.com/authorize");
        assert_eq!(endpoints.token_url, "https://issuer.example.com/token");
        assert_eq!(
            endpoints.userinfo_url.as_deref(),
            Some("https://issuer.example.com/userinfo")
        );
    }

    #[test]
    fn test_create_with_endpoints_defaults_from_issuer() {
        let provider = OidcProvider::create_with_endpoints(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "https://issuer.example.com/",
            None, // Should default to {issuer}/authorize
            None, // Should default to {issuer}/token
            None, // No userinfo
        )
        .unwrap();
        let endpoints = provider.init_config.static_endpoints.as_ref().unwrap();
        // Issuer trailing slash is trimmed, so defaults use trimmed version
        assert_eq!(endpoints.auth_url, "https://issuer.example.com/authorize");
        assert_eq!(endpoints.token_url, "https://issuer.example.com/token");
        assert!(endpoints.userinfo_url.is_none());
    }

    // ==================== Provider Type ====================

    #[test]
    fn test_provider_type() {
        let provider = OidcProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "https://issuer.example.com",
        )
        .unwrap();
        assert_eq!(provider.provider_type(), "oidc");
    }

    // ==================== Auth URL Generation (Static Endpoints) ====================

    #[tokio::test]
    async fn test_new_auth_url_with_static_endpoints() {
        let provider = OidcProvider::create_with_endpoints(
            "oidc_test_id".to_string(),
            "secret".to_string(),
            "https://example.com/callback".to_string(),
            "https://issuer.example.com",
            Some("https://issuer.example.com/authorize".to_string()),
            Some("https://issuer.example.com/token".to_string()),
            Some("https://issuer.example.com/userinfo".to_string()),
        )
        .unwrap();

        let state = "oidc_state_123";
        let (auth_url, pkce_verifier) = provider.new_auth_url(state).await.unwrap();

        // Auth URL should use the custom auth endpoint
        assert!(auth_url.starts_with("https://issuer.example.com/authorize"));
        // Should contain client_id
        assert!(auth_url.contains("client_id=oidc_test_id"));
        // Should contain state
        assert!(auth_url.contains(&format!("state={state}")));
        // Should contain redirect_uri
        assert!(auth_url.contains("redirect_uri="));
        // Should contain PKCE
        assert!(auth_url.contains("code_challenge="));
        assert!(auth_url.contains("code_challenge_method=S256"));
        // PKCE verifier should be non-empty
        assert!(!pkce_verifier.is_empty());
    }

    #[tokio::test]
    async fn test_new_auth_url_different_states() {
        let provider = OidcProvider::create_with_endpoints(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "https://issuer.example.com",
            Some("https://issuer.example.com/authorize".to_string()),
            Some("https://issuer.example.com/token".to_string()),
            None,
        )
        .unwrap();

        let (url1, v1) = provider.new_auth_url("state_a").await.unwrap();
        let (url2, v2) = provider.new_auth_url("state_b").await.unwrap();

        assert_ne!(url1, url2);
        assert_ne!(v1, v2);
    }

    // ==================== Factory Function ====================

    #[test]
    fn test_factory_with_issuer_only() {
        let config = serde_json::json!({
            "client_id": "oidc_id",
            "client_secret": "oidc_secret",
            "redirect_url": "https://example.com/oauth/oidc/callback",
            "issuer": "https://issuer.example.com"
        });
        let provider = oidc_factory(&config);
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().provider_type(), "oidc");
    }

    #[test]
    fn test_factory_with_custom_endpoints() {
        let config = serde_json::json!({
            "client_id": "oidc_id",
            "client_secret": "oidc_secret",
            "redirect_url": "https://example.com/cb",
            "issuer": "https://issuer.example.com",
            "auth_url": "https://issuer.example.com/custom/authorize",
            "token_url": "https://issuer.example.com/custom/token",
            "userinfo_url": "https://issuer.example.com/custom/userinfo"
        });
        let provider = oidc_factory(&config);
        assert!(provider.is_ok());
    }

    #[test]
    fn test_factory_with_partial_endpoints() {
        // Providing only auth_url should trigger create_with_endpoints path
        let config = serde_json::json!({
            "client_id": "id",
            "client_secret": "secret",
            "redirect_url": "https://example.com/cb",
            "issuer": "https://issuer.example.com",
            "auth_url": "https://issuer.example.com/auth"
        });
        let provider = oidc_factory(&config);
        assert!(provider.is_ok());
    }

    #[test]
    fn test_factory_missing_fields() {
        // Missing client_id
        let config = serde_json::json!({
            "client_secret": "secret",
            "redirect_url": "https://example.com/cb",
            "issuer": "https://issuer.example.com"
        });
        assert!(oidc_factory(&config).is_err());

        // Missing client_secret
        let config = serde_json::json!({
            "client_id": "id",
            "redirect_url": "https://example.com/cb",
            "issuer": "https://issuer.example.com"
        });
        assert!(oidc_factory(&config).is_err());

        // Missing redirect_url
        let config = serde_json::json!({
            "client_id": "id",
            "client_secret": "secret",
            "issuer": "https://issuer.example.com"
        });
        assert!(oidc_factory(&config).is_err());
    }

    #[test]
    fn test_factory_empty_json() {
        let config = serde_json::json!({});
        let result = oidc_factory(&config);
        assert!(result.is_err());
        assert!(matches!(result.err(), Some(Error::InvalidInput(_))));
    }

    #[test]
    fn test_factory_default_empty_issuer_rejected() {
        // issuer defaults to "" via #[serde(default)]
        let config = serde_json::json!({
            "client_id": "id",
            "client_secret": "secret",
            "redirect_url": "https://example.com/cb"
        });
        // Should fail at creation time with a clear error (no issuer, no custom endpoints)
        let result = oidc_factory(&config);
        assert!(result.is_err());
        assert!(matches!(result.err(), Some(Error::InvalidInput(_))));
    }

    #[test]
    fn test_factory_empty_issuer_with_custom_endpoints_ok() {
        // Empty issuer is allowed when custom endpoints are provided
        let config = serde_json::json!({
            "client_id": "id",
            "client_secret": "secret",
            "redirect_url": "https://example.com/cb",
            "auth_url": "https://provider.example.com/authorize",
            "token_url": "https://provider.example.com/token"
        });
        let result = oidc_factory(&config);
        assert!(result.is_ok());
    }

    // ==================== Config Deserialization ====================

    #[test]
    fn test_oidc_config_deserialize_full() {
        let json = serde_json::json!({
            "client_id": "oidc_abc",
            "client_secret": "oidc_def",
            "redirect_url": "https://example.com/cb",
            "issuer": "https://issuer.example.com",
            "auth_url": "https://issuer.example.com/auth",
            "token_url": "https://issuer.example.com/token",
            "userinfo_url": "https://issuer.example.com/userinfo"
        });
        let config: OidcConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.client_id, "oidc_abc");
        assert_eq!(config.client_secret, "oidc_def");
        assert_eq!(config.redirect_url, "https://example.com/cb");
        assert_eq!(config.issuer, "https://issuer.example.com");
        assert_eq!(
            config.auth_url.as_deref(),
            Some("https://issuer.example.com/auth")
        );
        assert_eq!(
            config.token_url.as_deref(),
            Some("https://issuer.example.com/token")
        );
        assert_eq!(
            config.userinfo_url.as_deref(),
            Some("https://issuer.example.com/userinfo")
        );
    }

    #[test]
    fn test_oidc_config_deserialize_minimal() {
        let json = serde_json::json!({
            "client_id": "id",
            "client_secret": "secret",
            "redirect_url": "https://example.com/cb"
        });
        let config: OidcConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.issuer, ""); // Default
        assert!(config.auth_url.is_none());
        assert!(config.token_url.is_none());
        assert!(config.userinfo_url.is_none());
    }

    #[test]
    fn test_oidc_config_serialize_skips_none_urls() {
        let config = OidcConfig {
            client_id: "id".to_string(),
            client_secret: "secret".to_string(),
            redirect_url: "https://example.com/cb".to_string(),
            issuer: "https://issuer.example.com".to_string(),
            auth_url: None,
            token_url: None,
            userinfo_url: None,
        };
        let json = serde_json::to_value(&config).unwrap();
        // Optional fields with skip_serializing_if should not appear
        assert!(json.get("auth_url").is_none());
        assert!(json.get("token_url").is_none());
        assert!(json.get("userinfo_url").is_none());
    }

    #[test]
    fn test_oidc_config_roundtrip() {
        let config = OidcConfig {
            client_id: "id".to_string(),
            client_secret: "secret".to_string(),
            redirect_url: "https://example.com/cb".to_string(),
            issuer: "https://issuer.example.com".to_string(),
            auth_url: Some("https://issuer.example.com/auth".to_string()),
            token_url: Some("https://issuer.example.com/token".to_string()),
            userinfo_url: Some("https://issuer.example.com/userinfo".to_string()),
        };
        let json = serde_json::to_value(&config).unwrap();
        let deserialized: OidcConfig = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.client_id, config.client_id);
        assert_eq!(deserialized.issuer, config.issuer);
        assert_eq!(deserialized.auth_url, config.auth_url);
        assert_eq!(deserialized.token_url, config.token_url);
        assert_eq!(deserialized.userinfo_url, config.userinfo_url);
    }

    // ==================== Lazy Resolution ====================

    #[tokio::test]
    async fn test_get_resolved_static_endpoints_succeeds() {
        // With static endpoints, get_resolved should succeed without network
        let provider = OidcProvider::create_with_endpoints(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "https://issuer.example.com",
            Some("https://issuer.example.com/authorize".to_string()),
            Some("https://issuer.example.com/token".to_string()),
            Some("https://issuer.example.com/userinfo".to_string()),
        )
        .unwrap();

        let resolved = provider.get_resolved().await;
        assert!(resolved.is_ok());
        let r = resolved.unwrap();
        assert_eq!(
            r.userinfo_url.as_deref(),
            Some("https://issuer.example.com/userinfo")
        );
    }

    #[tokio::test]
    async fn test_get_resolved_caches_result() {
        // Calling get_resolved twice with static endpoints should return the same ref
        let provider = OidcProvider::create_with_endpoints(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "https://issuer.example.com",
            Some("https://issuer.example.com/authorize".to_string()),
            Some("https://issuer.example.com/token".to_string()),
            None,
        )
        .unwrap();

        let r1 = provider.get_resolved().await.unwrap();
        let r2 = provider.get_resolved().await.unwrap();
        // Same pointer (OnceCell caches the result)
        assert!(std::ptr::eq(r1, r2));
    }
}
