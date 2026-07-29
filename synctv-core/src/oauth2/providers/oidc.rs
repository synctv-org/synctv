//! Generic OIDC provider

use super::{
    build_oauth2_http_client, build_provider_http_client, map_provider_http_error,
    validate_oauth2_redirect_url, validate_provider_url, validate_required_oauth2_field,
};
use crate::oauth2::{OAuth2Authorization, OAuth2UserInfo, Provider};
use crate::service::{
    OAuth2AppleProviderConfig, OAuth2CasdoorProviderConfig, OAuth2OidcProviderConfig,
    OAuth2ProviderPrivateConfig,
};
use crate::{Error, InternalExt};
use async_trait::async_trait;
use jsonwebtoken::{
    decode, decode_header,
    jwk::{Jwk, JwkSet, KeyOperations, PublicKeyUse},
    Algorithm, DecodingKey, Validation,
};
use oauth2::{
    basic::{BasicErrorResponse, BasicRevocationErrorResponse, BasicTokenType},
    AuthUrl, Client, ClientId, ClientSecret, EndpointNotSet, EndpointSet, ExtraTokenFields,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, StandardRevocableToken,
    StandardTokenIntrospectionResponse, StandardTokenResponse, TokenResponse, TokenUrl,
};
use reqwest::Client as ReqwestClient;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OnceCell, RwLock};

type OidcTokenResponse = StandardTokenResponse<OidcTokenExtraFields, BasicTokenType>;
type OidcClientBuilder = Client<
    BasicErrorResponse,
    OidcTokenResponse,
    StandardTokenIntrospectionResponse<OidcTokenExtraFields, BasicTokenType>,
    StandardRevocableToken,
    BasicRevocationErrorResponse,
>;
type OidcClient = Client<
    BasicErrorResponse,
    OidcTokenResponse,
    StandardTokenIntrospectionResponse<OidcTokenExtraFields, BasicTokenType>,
    StandardRevocableToken,
    BasicRevocationErrorResponse,
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointSet,
>;

const OIDC_JWKS_CACHE_TTL: Duration = Duration::from_mins(10);
const OIDC_JWKS_REFRESH_COOLDOWN: Duration = Duration::from_secs(5);
const APPLE_OIDC_ISSUER: &str = "https://appleid.apple.com";
const DEFAULT_OIDC_SCOPES: &[&str] = &["openid", "profile"];
const APPLE_OIDC_SCOPES: &[&str] = &["openid"];
const MAX_OIDC_SCOPES: usize = 32;
const MAX_OIDC_SCOPE_LEN: usize = 256;
const MAX_OIDC_SCOPES_TOTAL_LEN: usize = 2048;

fn normalize_oidc_scopes(scopes: Vec<String>, issuer: &str) -> Result<Vec<String>, Error> {
    let defaults = if issuer.eq_ignore_ascii_case(APPLE_OIDC_ISSUER) {
        APPLE_OIDC_SCOPES
    } else {
        DEFAULT_OIDC_SCOPES
    };
    let source = if scopes.is_empty() {
        defaults.iter().map(|scope| (*scope).to_string()).collect()
    } else {
        scopes
    };

    if source.len() > MAX_OIDC_SCOPES {
        return Err(Error::InvalidInput(format!(
            "OIDC scopes must contain at most {MAX_OIDC_SCOPES} entries"
        )));
    }

    let mut normalized = Vec::with_capacity(source.len());
    let mut seen = HashSet::new();
    let mut total_len = 0;
    for raw_scope in source {
        let scope = raw_scope.trim();
        let valid = !scope.is_empty()
            && scope.len() <= MAX_OIDC_SCOPE_LEN
            && scope.bytes().all(|byte| {
                byte == b'!' || (b'#'..=b'[').contains(&byte) || (b']'..=b'~').contains(&byte)
            });
        if !valid {
            return Err(Error::InvalidInput(format!(
                "OIDC scope '{scope}' is not a valid OAuth scope token"
            )));
        }
        total_len += scope.len();
        if total_len > MAX_OIDC_SCOPES_TOTAL_LEN {
            return Err(Error::InvalidInput(format!(
                "OIDC scopes must contain at most {MAX_OIDC_SCOPES_TOTAL_LEN} characters"
            )));
        }
        if seen.insert(scope.to_string()) {
            normalized.push(scope.to_string());
        }
    }

    if !seen.contains("openid") {
        return Err(Error::InvalidInput(
            "OIDC scopes must include 'openid'".to_string(),
        ));
    }
    Ok(normalized)
}

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jwks_url: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// Optional static OIDC endpoints.
#[derive(Debug, Clone)]
pub struct OidcEndpointOverrides {
    pub auth_url: Option<String>,
    pub token_url: Option<String>,
    pub userinfo_url: Option<String>,
    pub jwks_url: Option<String>,
}

/// Discovered OIDC endpoints from .well-known/openid-configuration
#[derive(Debug, Clone, Deserialize)]
struct OidcDiscoveryDocument {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    userinfo_endpoint: Option<String>,
}

/// Resolved OIDC client and endpoints, initialized lazily via discovery or static config.
struct ResolvedOidc {
    client: OidcClient,
    userinfo_url: Option<String>,
    jwks_uri: String,
}

/// Generic OIDC provider
///
/// When created via `create()` (issuer-only mode), the `OAuth2` client and endpoints
/// are resolved lazily on first use by fetching `{issuer}/.well-known/openid-configuration`.
/// When created via `create_with_endpoints()`, the provided endpoints are used directly.
pub struct OidcProvider {
    provider_type: &'static str,
    resolved: OnceCell<ResolvedOidc>,
    jwks_cache: RwLock<Option<CachedJwks>>,
    jwks_refresh_state: Mutex<JwksRefreshState>,
    /// Stored config for lazy initialization (only used in issuer-only mode)
    init_config: OidcInitConfig,
    oauth2_http_client: Arc<super::OAuth2HttpClient>,
    http_client: Arc<ReqwestClient>,
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
}

/// Internal config stored for lazy OIDC discovery
struct OidcInitConfig {
    client_id: String,
    client_secret: String,
    redirect_url: String,
    issuer: String,
    scopes: Vec<String>,
    /// If set, these are static overrides (no discovery needed)
    static_endpoints: Option<StaticEndpoints>,
}

struct StaticEndpoints {
    auth: String,
    token: String,
    userinfo: Option<String>,
    jwks: String,
}

struct CachedJwks {
    jwks_uri: String,
    jwks: Arc<JwkSet>,
    fetched_at: Instant,
    generation: u64,
}

struct JwksSnapshot {
    jwks: Arc<JwkSet>,
    generation: u64,
    may_refresh: bool,
}

#[derive(Default)]
struct JwksRefreshState {
    generation: u64,
    last_failure: Option<JwksRefreshFailure>,
}

struct JwksRefreshFailure {
    jwks_uri: String,
    generation: u64,
    failed_at: Instant,
    message: Arc<str>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
struct OidcTokenExtraFields {
    id_token: Option<String>,
}

impl ExtraTokenFields for OidcTokenExtraFields {}

#[derive(Debug, Deserialize)]
struct OidcIdTokenClaims {
    iss: String,
    sub: String,
    aud: OidcAudience,
    #[serde(rename = "iat")]
    _issued_at: i64,
    #[serde(default)]
    azp: Option<String>,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    preferred_username: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    picture: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OidcAudience {
    One(String),
    Many(Vec<String>),
}

impl OidcAudience {
    fn contains(&self, expected: &str) -> bool {
        match self {
            Self::One(aud) => aud == expected,
            Self::Many(audiences) => audiences.iter().any(|aud| aud == expected),
        }
    }

    fn is_multi(&self) -> bool {
        matches!(self, Self::Many(audiences) if audiences.len() > 1)
    }
}

#[derive(Deserialize)]
struct OidcUserInfoResponse {
    sub: String,
    #[serde(default)]
    preferred_username: Option<String>,
    name: Option<String>,
    picture: Option<String>,
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
        Self::create_with_ssrf_guard(
            client_id,
            client_secret,
            redirect_url,
            issuer,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
    }

    pub fn create_with_ssrf_guard(
        client_id: String,
        client_secret: String,
        redirect_url: String,
        issuer: &str,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Self, Error> {
        Self::create_with_scopes_and_ssrf_guard(
            client_id,
            client_secret,
            redirect_url,
            issuer,
            Vec::new(),
            ssrf_guard,
        )
    }

    pub fn create_with_scopes_and_ssrf_guard(
        client_id: String,
        client_secret: String,
        redirect_url: String,
        issuer: &str,
        scopes: Vec<String>,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Self, Error> {
        let issuer = issuer.trim_end_matches('/');
        validate_provider_url(issuer, "Invalid OIDC issuer URL", ssrf_guard)?;
        validate_oauth2_redirect_url(&redirect_url, "Invalid OIDC redirect URL")?;
        let scopes = normalize_oidc_scopes(scopes, issuer)?;
        Ok(Self {
            provider_type: "oidc",
            resolved: OnceCell::new(),
            jwks_cache: RwLock::new(None),
            jwks_refresh_state: Mutex::new(JwksRefreshState::default()),
            init_config: OidcInitConfig {
                client_id,
                client_secret,
                redirect_url,
                issuer: issuer.to_string(),
                scopes,
                static_endpoints: None,
            },
            oauth2_http_client: build_oauth2_http_client(ssrf_guard)?,
            http_client: build_provider_http_client(ssrf_guard)?,
            ssrf_guard: ssrf_guard.clone(),
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
        endpoints: OidcEndpointOverrides,
    ) -> Result<Self, Error> {
        Self::create_with_endpoints_and_ssrf_guard(
            client_id,
            client_secret,
            redirect_url,
            issuer,
            endpoints,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
    }

    pub fn create_with_endpoints_and_ssrf_guard(
        client_id: String,
        client_secret: String,
        redirect_url: String,
        issuer: &str,
        endpoints: OidcEndpointOverrides,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Self, Error> {
        Self::create_with_endpoints_scopes_and_ssrf_guard(
            client_id,
            client_secret,
            redirect_url,
            issuer,
            endpoints,
            Vec::new(),
            ssrf_guard,
        )
    }

    pub fn create_with_endpoints_scopes_and_ssrf_guard(
        client_id: String,
        client_secret: String,
        redirect_url: String,
        issuer: &str,
        endpoints: OidcEndpointOverrides,
        scopes: Vec<String>,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Self, Error> {
        let issuer_trimmed = issuer.trim_end_matches('/');
        if issuer_trimmed.is_empty() {
            return Err(Error::InvalidInput(
                "OIDC provider requires a non-empty 'issuer' URL".to_string(),
            ));
        }
        validate_provider_url(issuer_trimmed, "Invalid OIDC issuer URL", ssrf_guard)?;
        validate_oauth2_redirect_url(&redirect_url, "Invalid OIDC redirect URL")?;
        let scopes = normalize_oidc_scopes(scopes, issuer_trimmed)?;
        let auth = endpoints.auth_url.ok_or_else(|| {
            Error::InvalidInput(
                "OIDC static endpoint mode requires explicit 'auth_url'".to_string(),
            )
        })?;
        let token = endpoints.token_url.ok_or_else(|| {
            Error::InvalidInput(
                "OIDC static endpoint mode requires explicit 'token_url'".to_string(),
            )
        })?;
        let jwks = endpoints.jwks_url.ok_or_else(|| {
            Error::InvalidInput(
                "OIDC static endpoint mode requires explicit 'jwks_url'".to_string(),
            )
        })?;
        validate_provider_url(&auth, "Invalid OIDC auth URL", ssrf_guard)?;
        validate_provider_url(&token, "Invalid OIDC token URL", ssrf_guard)?;
        validate_provider_url(&jwks, "Invalid OIDC JWKS URL", ssrf_guard)?;
        if let Some(userinfo) = endpoints.userinfo_url.as_deref() {
            validate_provider_url(userinfo, "Invalid OIDC userinfo URL", ssrf_guard)?;
        }
        Ok(Self {
            provider_type: "oidc",
            resolved: OnceCell::new(),
            jwks_cache: RwLock::new(None),
            jwks_refresh_state: Mutex::new(JwksRefreshState::default()),
            init_config: OidcInitConfig {
                client_id,
                client_secret,
                redirect_url,
                issuer: issuer_trimmed.to_string(),
                scopes,
                static_endpoints: Some(StaticEndpoints {
                    auth,
                    token,
                    userinfo: endpoints.userinfo_url,
                    jwks,
                }),
            },
            oauth2_http_client: build_oauth2_http_client(ssrf_guard)?,
            http_client: build_provider_http_client(ssrf_guard)?,
            ssrf_guard: ssrf_guard.clone(),
        })
    }

    #[must_use]
    fn with_provider_type(mut self, provider_type: &'static str) -> Self {
        self.provider_type = provider_type;
        self
    }

    /// Resolve the `OAuth2` client, performing .well-known discovery if needed.
    async fn get_resolved(&self) -> Result<&ResolvedOidc, Error> {
        self.resolved
            .get_or_try_init(|| async {
                let config = &self.init_config;

                let (auth_url_str, token_url_str, userinfo_url, jwks_uri) = if let Some(static_ep) =
                    &config.static_endpoints
                {
                    (
                        static_ep.auth.clone(),
                        static_ep.token.clone(),
                        static_ep.userinfo.clone(),
                        static_ep.jwks.clone(),
                    )
                } else {
                    // Perform .well-known/openid-configuration discovery
                    let discovery_url =
                        format!("{}/.well-known/openid-configuration", config.issuer);
                    validate_provider_url(
                        &discovery_url,
                        "Invalid OIDC discovery document URL",
                        &self.ssrf_guard,
                    )?;
                    tracing::info!("OIDC: fetching discovery document from {}", discovery_url);

                    let resp = self
                        .http_client
                        .get(&discovery_url)
                        .send()
                        .await
                        .map_err(|err| {
                            map_provider_http_error(
                                &format!(
                                    "Failed to fetch OIDC discovery document from {discovery_url}"
                                ),
                                err,
                            )
                        })?
                        .error_for_status()
                        .map_err(|e| {
                            Error::Internal(format!("OIDC discovery endpoint returned error: {e}"))
                        })?;

                    let doc: OidcDiscoveryDocument = resp.json().await.map_err(|e| {
                        Error::Internal(format!("Failed to parse OIDC discovery document: {e}"))
                    })?;

                    if doc.issuer.trim_end_matches('/') != config.issuer {
                        return Err(Error::InvalidInput(format!(
                            "OIDC discovery issuer '{}' does not match configured issuer '{}'",
                            doc.issuer, config.issuer
                        )));
                    }

                    tracing::info!(
                        "OIDC: discovered endpoints: auth={}, token={}, userinfo={:?}, jwks={}",
                        doc.authorization_endpoint,
                        doc.token_endpoint,
                        doc.userinfo_endpoint,
                        doc.jwks_uri
                    );

                    validate_provider_url(
                        &doc.authorization_endpoint,
                        "Invalid OIDC auth URL",
                        &self.ssrf_guard,
                    )?;
                    validate_provider_url(
                        &doc.token_endpoint,
                        "Invalid OIDC token URL",
                        &self.ssrf_guard,
                    )?;
                    validate_provider_url(
                        &doc.jwks_uri,
                        "Invalid OIDC JWKS URL",
                        &self.ssrf_guard,
                    )?;
                    if let Some(userinfo) = doc.userinfo_endpoint.as_deref() {
                        validate_provider_url(
                            userinfo,
                            "Invalid OIDC userinfo URL",
                            &self.ssrf_guard,
                        )?;
                    }

                    (
                        doc.authorization_endpoint,
                        doc.token_endpoint,
                        doc.userinfo_endpoint,
                        doc.jwks_uri,
                    )
                };

                let auth = AuthUrl::new(auth_url_str)
                    .map_err(|e| Error::InvalidInput(format!("Invalid OIDC auth URL: {e}")))?;
                let token = TokenUrl::new(token_url_str)
                    .map_err(|e| Error::InvalidInput(format!("Invalid OIDC token URL: {e}")))?;
                let redirect = RedirectUrl::new(config.redirect_url.clone())
                    .map_err(|e| Error::InvalidInput(format!("Invalid OIDC redirect URL: {e}")))?;

                let client = OidcClientBuilder::new(ClientId::new(config.client_id.clone()))
                    .set_client_secret(ClientSecret::new(config.client_secret.clone()))
                    .set_auth_uri(auth)
                    .set_token_uri(token)
                    .set_redirect_uri(redirect);

                Ok(ResolvedOidc {
                    client,
                    userinfo_url,
                    jwks_uri,
                })
            })
            .await
    }

    async fn fetch_jwks(&self, jwks_uri: &str) -> Result<JwkSet, Error> {
        validate_provider_url(jwks_uri, "Invalid OIDC JWKS URL", &self.ssrf_guard)?;
        self.http_client
            .get(jwks_uri)
            .send()
            .await
            .map_err(|err| map_provider_http_error("Failed to fetch OIDC JWKS", err))?
            .error_for_status()
            .internal_with_err("OIDC JWKS endpoint returned error")?
            .json()
            .await
            .internal_with_err("Failed to parse OIDC JWKS")
    }

    async fn fetch_and_store_jwks(
        &self,
        jwks_uri: &str,
        refresh_state: &mut JwksRefreshState,
    ) -> Result<JwksSnapshot, Error> {
        let jwks = match self.fetch_jwks(jwks_uri).await {
            Ok(jwks) => Arc::new(jwks),
            Err(err) => {
                refresh_state.last_failure = Some(JwksRefreshFailure {
                    jwks_uri: jwks_uri.to_string(),
                    generation: refresh_state.generation,
                    failed_at: Instant::now(),
                    message: err.to_string().into(),
                });
                return Err(err);
            }
        };
        let fetched_at = Instant::now();
        refresh_state.generation += 1;
        refresh_state.last_failure = None;
        let generation = refresh_state.generation;
        let mut cache = self.jwks_cache.write().await;
        *cache = Some(CachedJwks {
            jwks_uri: jwks_uri.to_string(),
            jwks: jwks.clone(),
            fetched_at,
            generation,
        });
        Ok(JwksSnapshot {
            jwks,
            generation,
            may_refresh: false,
        })
    }

    fn recent_jwks_failure(
        refresh_state: &JwksRefreshState,
        jwks_uri: &str,
        now: Instant,
    ) -> Option<Error> {
        let failure = refresh_state.last_failure.as_ref()?;
        (failure.jwks_uri == jwks_uri
            && failure.generation == refresh_state.generation
            && now.saturating_duration_since(failure.failed_at) < OIDC_JWKS_REFRESH_COOLDOWN)
            .then(|| Error::ServiceUnavailable(failure.message.to_string()))
    }

    fn jwks_snapshot(cached: &CachedJwks, may_refresh: bool) -> JwksSnapshot {
        JwksSnapshot {
            jwks: Arc::clone(&cached.jwks),
            generation: cached.generation,
            may_refresh,
        }
    }

    async fn cached_jwks(&self, jwks_uri: &str) -> Result<JwksSnapshot, Error> {
        let now = Instant::now();
        {
            let cache = self.jwks_cache.read().await;
            if let Some(cached) = cache.as_ref() {
                if cached_jwks_is_fresh(cached, jwks_uri, now) {
                    return Ok(Self::jwks_snapshot(cached, true));
                }
            }
        }

        let mut refresh_state = self.jwks_refresh_state.lock().await;
        let now = Instant::now();
        let cache = self.jwks_cache.read().await;
        if let Some(cached) = cache.as_ref() {
            if cached_jwks_is_fresh(cached, jwks_uri, now) {
                return Ok(Self::jwks_snapshot(cached, false));
            }
        }
        drop(cache);
        if let Some(err) = Self::recent_jwks_failure(&refresh_state, jwks_uri, now) {
            return Err(err);
        }
        self.fetch_and_store_jwks(jwks_uri, &mut refresh_state)
            .await
    }

    async fn refresh_cached_jwks(
        &self,
        jwks_uri: &str,
        observed: JwksSnapshot,
    ) -> Result<JwksSnapshot, Error> {
        if !observed.may_refresh {
            return Ok(observed);
        }

        let mut refresh_state = self.jwks_refresh_state.lock().await;
        let now = Instant::now();
        let cache = self.jwks_cache.read().await;
        if let Some(cached) = cache.as_ref() {
            if cached.jwks_uri == jwks_uri && cached.generation != observed.generation {
                return Ok(Self::jwks_snapshot(cached, false));
            }
            if cached.jwks_uri == jwks_uri
                && now.saturating_duration_since(cached.fetched_at) < OIDC_JWKS_REFRESH_COOLDOWN
            {
                return Ok(Self::jwks_snapshot(cached, false));
            }
        }
        drop(cache);
        if let Some(err) = Self::recent_jwks_failure(&refresh_state, jwks_uri, now) {
            return Err(err);
        }
        self.fetch_and_store_jwks(jwks_uri, &mut refresh_state)
            .await
    }

    async fn validate_id_token(
        &self,
        resolved: &ResolvedOidc,
        id_token: &str,
        expected_nonce: &str,
    ) -> Result<OidcIdTokenClaims, Error> {
        let header = decode_header(id_token)
            .map_err(|err| Error::Authentication(format!("Invalid OIDC ID Token header: {err}")))?;
        if !is_supported_oidc_id_token_algorithm(header.alg) {
            return Err(Error::Authentication(
                "OIDC ID Token uses an unsupported signing algorithm".to_string(),
            ));
        }
        let claims = if let Some(kid) = header.kid.as_deref() {
            let jwk = self.jwk_for_kid(&resolved.jwks_uri, kid).await?;
            self.decode_id_token_with_jwk(id_token, &jwk, header.alg)?
        } else {
            self.decode_id_token_without_kid(&resolved.jwks_uri, id_token, header.alg)
                .await?
        };

        if claims.iss.trim_end_matches('/') != self.init_config.issuer {
            return Err(Error::Authentication(
                "OIDC ID Token issuer does not match configured issuer".to_string(),
            ));
        }
        if !claims.aud.contains(&self.init_config.client_id) {
            return Err(Error::Authentication(
                "OIDC ID Token audience does not include configured client_id".to_string(),
            ));
        }
        if claims.aud.is_multi()
            && claims.azp.as_deref() != Some(self.init_config.client_id.as_str())
        {
            return Err(Error::Authentication(
                "OIDC ID Token authorized party does not match configured client_id".to_string(),
            ));
        }
        match claims.nonce.as_deref() {
            Some(actual) if actual == expected_nonce => {}
            Some(_) => {
                return Err(Error::Authentication(
                    "OIDC ID Token nonce does not match authorization request".to_string(),
                ));
            }
            None => {
                return Err(Error::Authentication(
                    "OIDC ID Token is missing nonce".to_string(),
                ));
            }
        }

        Ok(claims)
    }

    fn id_token_validation(&self, alg: Algorithm) -> Validation {
        let mut validation = Validation::new(alg);
        validation.set_issuer(&[self.init_config.issuer.as_str()]);
        validation.set_audience(&[self.init_config.client_id.as_str()]);
        validation.set_required_spec_claims(&["exp", "iat", "iss", "sub", "aud"]);
        validation
    }

    fn decode_id_token_with_jwk(
        &self,
        id_token: &str,
        jwk: &Jwk,
        alg: Algorithm,
    ) -> Result<OidcIdTokenClaims, Error> {
        validate_jwk_for_id_token(jwk, alg)?;
        let decoding_key = DecodingKey::from_jwk(jwk).map_err(|err| {
            Error::Authentication(format!("Invalid OIDC ID Token signing key: {err}"))
        })?;
        let validation = self.id_token_validation(alg);
        let token = decode::<OidcIdTokenClaims>(id_token, &decoding_key, &validation)
            .map_err(|err| Error::Authentication(format!("Invalid OIDC ID Token: {err}")))?;
        Ok(token.claims)
    }

    async fn decode_id_token_without_kid(
        &self,
        jwks_uri: &str,
        id_token: &str,
        alg: Algorithm,
    ) -> Result<OidcIdTokenClaims, Error> {
        let jwks = self.cached_jwks(jwks_uri).await?;
        if let Ok(claims) = self.try_decode_id_token_with_jwks(&jwks.jwks, id_token, alg) {
            return Ok(claims);
        }

        let jwks = self.refresh_cached_jwks(jwks_uri, jwks).await?;
        self.try_decode_id_token_with_jwks(&jwks.jwks, id_token, alg)
    }

    fn try_decode_id_token_with_jwks(
        &self,
        jwks: &JwkSet,
        id_token: &str,
        alg: Algorithm,
    ) -> Result<OidcIdTokenClaims, Error> {
        let mut last_decode_error = None;

        for jwk in &jwks.keys {
            if validate_jwk_for_id_token(jwk, alg).is_err() {
                continue;
            }

            match self.decode_id_token_with_jwk(id_token, jwk, alg) {
                Ok(claims) => return Ok(claims),
                Err(err) => last_decode_error = Some(err),
            }
        }

        Err(last_decode_error.unwrap_or_else(|| {
            Error::Authentication("OIDC ID Token signing key was not found in JWKS".to_string())
        }))
    }

    async fn jwk_for_kid(&self, jwks_uri: &str, kid: &str) -> Result<Jwk, Error> {
        let jwks = self.cached_jwks(jwks_uri).await?;
        if let Some(jwk) = jwks.jwks.find(kid) {
            return Ok(jwk.clone());
        }

        let jwks = self.refresh_cached_jwks(jwks_uri, jwks).await?;
        jwks.jwks.find(kid).cloned().ok_or_else(|| {
            Error::Authentication("OIDC ID Token signing key was not found in JWKS".to_string())
        })
    }

    fn user_info_from_id_token_claims(claims: OidcIdTokenClaims) -> OAuth2UserInfo {
        let provider_user_id = claims.sub;
        let username = first_non_empty([claims.preferred_username, claims.name])
            .unwrap_or_else(|| provider_user_id.clone());

        OAuth2UserInfo {
            provider_user_id,
            username,
            avatar: claims.picture,
        }
    }

    fn user_info_from_userinfo_response(
        user: OidcUserInfoResponse,
        id_token_claims: OidcIdTokenClaims,
    ) -> OAuth2UserInfo {
        let provider_user_id = user.sub;
        let username = first_non_empty([
            user.preferred_username,
            user.name,
            id_token_claims.preferred_username,
            id_token_claims.name,
        ])
        .unwrap_or_else(|| provider_user_id.clone());

        OAuth2UserInfo {
            provider_user_id,
            username,
            avatar: user.picture.or(id_token_claims.picture),
        }
    }
}

fn first_non_empty(values: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    values
        .into_iter()
        .flatten()
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

fn cached_jwks_is_fresh(cached: &CachedJwks, jwks_uri: &str, now: Instant) -> bool {
    cached.jwks_uri == jwks_uri && now.duration_since(cached.fetched_at) < OIDC_JWKS_CACHE_TTL
}

fn is_supported_oidc_id_token_algorithm(algorithm: Algorithm) -> bool {
    !matches!(
        algorithm,
        Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512
    )
}

fn validate_jwk_for_id_token(jwk: &Jwk, algorithm: Algorithm) -> Result<(), Error> {
    if matches!(
        jwk.common.public_key_use,
        Some(PublicKeyUse::Encryption | PublicKeyUse::Other(_))
    ) {
        return Err(Error::Authentication(
            "OIDC ID Token signing key is not intended for signatures".to_string(),
        ));
    }
    if let Some(operations) = jwk.common.key_operations.as_ref() {
        if !operations
            .iter()
            .any(|operation| operation == &KeyOperations::Verify)
        {
            return Err(Error::Authentication(
                "OIDC ID Token signing key cannot verify signatures".to_string(),
            ));
        }
    }
    if let Some(key_algorithm) = jwk.common.key_algorithm {
        let key_algorithm =
            Algorithm::from_str(key_algorithm.to_string().as_str()).map_err(|_| {
                Error::Authentication(
                    "OIDC ID Token signing key uses an unsupported algorithm".to_string(),
                )
            })?;
        if key_algorithm != algorithm {
            return Err(Error::Authentication(
                "OIDC ID Token signing key algorithm does not match token header".to_string(),
            ));
        }
    }

    Ok(())
}

#[async_trait]
impl Provider for OidcProvider {
    fn provider_type(&self) -> &'static str {
        self.provider_type
    }

    async fn new_auth_url(
        &self,
        state: &str,
        redirect_url: Option<&str>,
    ) -> Result<OAuth2Authorization, Error> {
        let resolved = self.get_resolved().await?;
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
        let nonce = synctv_common::snanoid!(32);
        let mut request = resolved
            .client
            .authorize_url(|| oauth2::CsrfToken::new(state.to_string()));
        for scope in &self.init_config.scopes {
            request = request.add_scope(Scope::new(scope.clone()));
        }
        let mut request = request
            .add_extra_param("nonce", nonce.as_str())
            .set_pkce_challenge(pkce_challenge);
        if let Some(redirect_url) = redirect_url {
            request = request.set_redirect_uri(std::borrow::Cow::Owned(
                RedirectUrl::new(redirect_url.to_string())
                    .map_err(|e| Error::InvalidInput(format!("Invalid OIDC redirect URL: {e}")))?,
            ));
        }
        let (auth_url, _csrf_token) = request.url();
        Ok(
            OAuth2Authorization::new(auth_url.to_string(), pkce_verifier.secret().clone())
                .with_nonce(nonce),
        )
    }

    async fn get_user_info(
        &self,
        code: &str,
        redirect_url: Option<&str>,
        pkce_verifier: &str,
        nonce: Option<&str>,
    ) -> Result<OAuth2UserInfo, Error> {
        let resolved = self.get_resolved().await?;
        let nonce = nonce.ok_or_else(|| {
            Error::Authentication(
                "OIDC callback is missing nonce from authorization state".to_string(),
            )
        })?;

        // Exchange code for token with PKCE verifier
        let verifier = PkceCodeVerifier::new(pkce_verifier.to_string());
        let mut request = resolved
            .client
            .exchange_code(oauth2::AuthorizationCode::new(code.to_string()))
            .set_pkce_verifier(verifier);
        if let Some(redirect_url) = redirect_url {
            request = request.set_redirect_uri(std::borrow::Cow::Owned(
                RedirectUrl::new(redirect_url.to_string())
                    .map_err(|e| Error::InvalidInput(format!("Invalid OIDC redirect URL: {e}")))?,
            ));
        }
        let token = request
            .request_async(self.oauth2_http_client.as_ref())
            .await
            .map_err(|err| map_provider_http_error("Failed to exchange code", err))?;
        let id_token = token.extra_fields().id_token.as_deref().ok_or_else(|| {
            Error::Authentication("OIDC token response is missing id_token".to_string())
        })?;
        let id_token_claims = self.validate_id_token(resolved, id_token, nonce).await?;

        let Some(userinfo_url) = resolved.userinfo_url.as_ref() else {
            return Ok(Self::user_info_from_id_token_claims(id_token_claims));
        };

        let resp = self
            .http_client
            .get(userinfo_url)
            .header(
                "Authorization",
                format!("Bearer {}", token.access_token().secret()),
            )
            .send()
            .await
            .map_err(|err| map_provider_http_error("Failed to fetch user info", err))?
            .error_for_status()
            .internal_with_err("OIDC API error")?;

        let user: OidcUserInfoResponse = resp
            .json()
            .await
            .internal_with_err("Failed to parse user info")?;
        if user.sub != id_token_claims.sub {
            return Err(Error::Authentication(
                "OIDC UserInfo subject does not match ID Token subject".to_string(),
            ));
        }

        Ok(Self::user_info_from_userinfo_response(
            user,
            id_token_claims,
        ))
    }
}

pub fn oidc_factory_from_private_config(
    config: &OAuth2ProviderPrivateConfig,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    let OAuth2ProviderPrivateConfig::Oidc(config) = config else {
        return Err(Error::InvalidInput(
            "OIDC provider requires oidc config".to_string(),
        ));
    };
    oidc_factory_from_typed_config(config, ssrf_guard)
}

pub fn casdoor_factory_from_private_config(
    config: &OAuth2ProviderPrivateConfig,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    let OAuth2ProviderPrivateConfig::Casdoor(config) = config else {
        return Err(Error::InvalidInput(
            "Casdoor provider requires casdoor config".to_string(),
        ));
    };
    casdoor_factory_from_typed_config(config, ssrf_guard)
}

pub fn apple_factory_from_private_config(
    config: &OAuth2ProviderPrivateConfig,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    let OAuth2ProviderPrivateConfig::Apple(config) = config else {
        return Err(Error::InvalidInput(
            "Apple provider requires apple config".to_string(),
        ));
    };
    apple_factory_from_typed_config(config, ssrf_guard)
}

fn oidc_factory_from_typed_config(
    config: &OAuth2OidcProviderConfig,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    validate_required_oauth2_field("OIDC", "client_id", &config.client_id)?;
    validate_required_oauth2_field("OIDC", "client_secret", &config.client_secret)?;
    validate_required_oauth2_field("OIDC", "redirect_url", &config.redirect_url)?;
    oidc_factory_from_config(
        OidcConfig {
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
            redirect_url: config.redirect_url.clone(),
            issuer: config.issuer.clone(),
            auth_url: config.auth_url.clone(),
            token_url: config.token_url.clone(),
            userinfo_url: config.userinfo_url.clone(),
            jwks_url: config.jwks_url.clone(),
            scopes: config.scopes.clone(),
        },
        "oidc",
        ssrf_guard,
    )
}

fn casdoor_factory_from_typed_config(
    config: &OAuth2CasdoorProviderConfig,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    validate_required_oauth2_field("Casdoor", "client_id", &config.client_id)?;
    validate_required_oauth2_field("Casdoor", "client_secret", &config.client_secret)?;
    validate_required_oauth2_field("Casdoor", "redirect_url", &config.redirect_url)?;
    oidc_factory_from_config(
        OidcConfig {
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
            redirect_url: config.redirect_url.clone(),
            issuer: config.issuer.clone(),
            auth_url: config.auth_url.clone(),
            token_url: config.token_url.clone(),
            userinfo_url: config.userinfo_url.clone(),
            jwks_url: config.jwks_url.clone(),
            scopes: DEFAULT_OIDC_SCOPES
                .iter()
                .map(|scope| (*scope).to_string())
                .collect(),
        },
        "casdoor",
        ssrf_guard,
    )
}

fn apple_factory_from_typed_config(
    config: &OAuth2AppleProviderConfig,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    validate_required_oauth2_field("Apple", "client_id", &config.client_id)?;
    validate_required_oauth2_field("Apple", "client_secret", &config.client_secret)?;
    validate_required_oauth2_field("Apple", "redirect_url", &config.redirect_url)?;
    let provider = OidcProvider::create_with_scopes_and_ssrf_guard(
        config.client_id.clone(),
        config.client_secret.clone(),
        config.redirect_url.clone(),
        APPLE_OIDC_ISSUER,
        APPLE_OIDC_SCOPES
            .iter()
            .map(|scope| (*scope).to_string())
            .collect(),
        ssrf_guard,
    )?
    .with_provider_type("apple");
    Ok(Box::new(provider))
}

fn oidc_factory_from_config(
    config: OidcConfig,
    provider_type: &'static str,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    let has_custom_endpoints = config.auth_url.is_some()
        || config.token_url.is_some()
        || config.userinfo_url.is_some()
        || config.jwks_url.is_some();
    if config.issuer.trim().is_empty() {
        return Err(Error::InvalidInput(
            "OIDC provider requires a non-empty 'issuer' URL".to_string(),
        ));
    }

    if has_custom_endpoints
        && (config.auth_url.is_none() || config.token_url.is_none() || config.jwks_url.is_none())
    {
        return Err(Error::InvalidInput(
            "OIDC static endpoint mode requires 'auth_url', 'token_url', and 'jwks_url'; \
             omit all custom endpoints to use .well-known discovery"
                .to_string(),
        ));
    }

    let provider = if has_custom_endpoints {
        OidcProvider::create_with_endpoints_scopes_and_ssrf_guard(
            config.client_id,
            config.client_secret,
            config.redirect_url,
            &config.issuer,
            OidcEndpointOverrides {
                auth_url: config.auth_url,
                token_url: config.token_url,
                userinfo_url: config.userinfo_url,
                jwks_url: config.jwks_url,
            },
            config.scopes,
            ssrf_guard,
        )?
    } else {
        OidcProvider::create_with_scopes_and_ssrf_guard(
            config.client_id,
            config.client_secret,
            config.redirect_url,
            &config.issuer,
            config.scopes,
            ssrf_guard,
        )?
    };

    Ok(Box::new(provider.with_provider_type(provider_type)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{TestOptionExt, TestResultExt};
    use jsonwebtoken::jwk::{
        AlgorithmParameters, CommonParameters, KeyAlgorithm, RSAKeyParameters, RSAKeyType,
    };
    use jsonwebtoken::{EncodingKey, Header};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn oidc_private_config(
        client_id: &str,
        client_secret: &str,
        redirect_url: &str,
        issuer: &str,
        endpoints: OidcEndpointOverrides,
    ) -> OAuth2ProviderPrivateConfig {
        OAuth2ProviderPrivateConfig::Oidc(OAuth2OidcProviderConfig {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            redirect_url: redirect_url.to_string(),
            issuer: issuer.to_string(),
            auth_url: endpoints.auth_url,
            token_url: endpoints.token_url,
            userinfo_url: endpoints.userinfo_url,
            jwks_url: endpoints.jwks_url,
            scopes: Vec::new(),
        })
    }

    fn oidc_factory_for_test(
        config: &OAuth2ProviderPrivateConfig,
    ) -> Result<Box<dyn Provider>, Error> {
        oidc_factory_from_private_config(config, &synctv_common::ssrf::SsrfGuard::strict_policy())
    }

    fn no_oidc_endpoint_overrides() -> OidcEndpointOverrides {
        OidcEndpointOverrides {
            auth_url: None,
            token_url: None,
            userinfo_url: None,
            jwks_url: None,
        }
    }

    const TEST_RSA_PRIVATE_KEY: &[u8] = br"-----BEGIN RSA PRIVATE KEY-----
MIIEpAIBAAKCAQEAyRE6rHuNR0QbHO3H3Kt2pOKGVhQqGZXInOduQNxXzuKlvQTL
UTv4l4sggh5/CYYi/cvI+SXVT9kPWSKXxJXBXd/4LkvcPuUakBoAkfh+eiFVMh2V
rUyWyj3MFl0HTVF9KwRXLAcwkREiS3npThHRyIxuy0ZMeZfxVL5arMhw1SRELB8H
oGfG/AtH89BIE9jDBHZ9dLelK9a184zAf8LwoPLxvJb3Il5nncqPcSfKDDodMFBI
Mc4lQzDKL5gvmiXLXB1AGLm8KBjfE8s3L5xqi+yUod+j8MtvIj812dkS4QMiRVN/
by2h3ZY8LYVGrqZXZTcgn2ujn8uKjXLZVD5TdQIDAQABAoIBAHREk0I0O9DvECKd
WUpAmF3mY7oY9PNQiu44Yaf+AoSuyRpRUGTMIgc3u3eivOE8ALX0BmYUO5JtuRNZ
Dpvt4SAwqCnVUinIf6C+eH/wSurCpapSM0BAHp4aOA7igptyOMgMPYBHNA1e9A7j
E0dCxKWMl3DSWNyjQTk4zeRGEAEfbNjHrq6YCtjHSZSLmWiG80hnfnYos9hOr5Jn
LnyS7ZmFE/5P3XVrxLc/tQ5zum0R4cbrgzHiQP5RgfxGJaEi7XcgherCCOgurJSS
bYH29Gz8u5fFbS+Yg8s+OiCss3cs1rSgJ9/eHZuzGEdUZVARH6hVMjSuwvqVTFaE
8AgtleECgYEA+uLMn4kNqHlJS2A5uAnCkj90ZxEtNm3E8hAxUrhssktY5XSOAPBl
xyf5RuRGIImGtUVIr4HuJSa5TX48n3Vdt9MYCprO/iYl6moNRSPt5qowIIOJmIjY
2mqPDfDt/zw+fcDD3lmCJrFlzcnh0uea1CohxEbQnL3cypeLt+WbU6kCgYEAzSp1
9m1ajieFkqgoB0YTpt/OroDx38vvI5unInJlEeOjQ+oIAQdN2wpxBvTrRorMU6P0
7mFUbt1j+Co6CbNiw+X8HcCaqYLR5clbJOOWNR36PuzOpQLkfK8woupBxzW9B8gZ
mY8rB1mbJ+/WTPrEJy6YGmIEBkWylQ2VpW8O4O0CgYEApdbvvfFBlwD9YxbrcGz7
MeNCFbMz+MucqQntIKoKJ91ImPxvtc0y6e/Rhnv0oyNlaUOwJVu0yNgNG117w0g4
t/+Q38mvVC5xV7/cn7x9UMFk6MkqVir3dYGEqIl/OP1grY2Tq9HtB5iyG9L8NIam
QOLMyUqqMUILxdthHyFmiGkCgYEAn9+PjpjGMPHxL0gj8Q8VbzsFtou6b1deIRRA
2CHmSltltR1gYVTMwXxQeUhPMmgkMqUXzs4/WijgpthY44hK1TaZEKIuoxrS70nJ
4WQLf5a9k1065fDsFZD6yGjdGxvwEmlGMZgTwqV7t1I4X0Ilqhav5hcs5apYL7gn
PYPeRz0CgYALHCj/Ji8XSsDoF/MhVhnGdIs2P99NNdmo3R2Pv0CuZbDKMU559LJH
UvrKS8WkuWRDuKrz1W/EQKApFjDGpdqToZqriUFQzwy7mR3ayIiogzNtHcvbDHx8
oFnGY0OFksX/ye0/XGpy2SFxYRwGU98HPYeBvAQQrVjdkzfy7BmXQQ==
-----END RSA PRIVATE KEY-----
";

    fn jwk_with_algorithm(key_algorithm: Option<KeyAlgorithm>) -> Jwk {
        jwk_with_kid_and_algorithm("test-kid", key_algorithm)
    }

    fn jwk_with_kid_and_algorithm(kid: &str, key_algorithm: Option<KeyAlgorithm>) -> Jwk {
        Jwk {
            common: CommonParameters {
                public_key_use: Some(PublicKeyUse::Signature),
                key_algorithm,
                key_id: Some(kid.to_string()),
                ..Default::default()
            },
            algorithm: AlgorithmParameters::RSA(RSAKeyParameters {
                key_type: RSAKeyType::RSA,
                n: "sXchDaQ1dPhzDYu9TPcL2m7W9uXk3qf8UUNl7fE6jZpb9fBRmG6u42Rn_G8kdR1nRUe8XgUXjS3oKPVNhF9kS6IuZ7Xmb6M3N5Lhlh3Pf4GHY_fAQiNnNLlGXf-6eFjAMj1N0yRu9n5cS7KZkQ7P4_VGf2L9Vy6V5O4H3M".to_string(),
                e: "AQAB".to_string(),
            }),
        }
    }

    fn jwk_set_with_key(jwk: Jwk) -> JwkSet {
        JwkSet { keys: vec![jwk] }
    }

    fn test_signing_jwk(kid: Option<&str>) -> Jwk {
        Jwk {
            common: CommonParameters {
                public_key_use: Some(PublicKeyUse::Signature),
                key_algorithm: Some(KeyAlgorithm::RS256),
                key_id: kid.map(ToString::to_string),
                ..Default::default()
            },
            algorithm: AlgorithmParameters::RSA(RSAKeyParameters {
                key_type: RSAKeyType::RSA,
                n: "yRE6rHuNR0QbHO3H3Kt2pOKGVhQqGZXInOduQNxXzuKlvQTLUTv4l4sggh5_CYYi_cvI-SXVT9kPWSKXxJXBXd_4LkvcPuUakBoAkfh-eiFVMh2VrUyWyj3MFl0HTVF9KwRXLAcwkREiS3npThHRyIxuy0ZMeZfxVL5arMhw1SRELB8HoGfG_AtH89BIE9jDBHZ9dLelK9a184zAf8LwoPLxvJb3Il5nncqPcSfKDDodMFBIMc4lQzDKL5gvmiXLXB1AGLm8KBjfE8s3L5xqi-yUod-j8MtvIj812dkS4QMiRVN_by2h3ZY8LYVGrqZXZTcgn2ujn8uKjXLZVD5TdQ".to_string(),
                e: "AQAB".to_string(),
            }),
        }
    }

    async fn spawn_jwks_server(jwks: JwkSet) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .checked("test JWKS server should bind");
        let addr = listener
            .local_addr()
            .checked("test JWKS server should expose local addr");
        let body = serde_json::to_string(&jwks).checked("JWKS should serialize");

        tokio::spawn(async move {
            let (mut socket, _) = listener
                .accept()
                .await
                .checked("test JWKS server should accept one connection");
            let mut request = [0; 1024];
            socket
                .read(&mut request)
                .await
                .checked("test JWKS server should read request");
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket
                .write_all(response.as_bytes())
                .await
                .checked("test JWKS server should write response");
        });

        format!("http://{addr}/jwks")
    }

    async fn spawn_counting_jwks_server(
        jwks: JwkSet,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let body = serde_json::to_string(&jwks).checked("JWKS should serialize");
        spawn_counting_http_server("200 OK", body).await
    }

    async fn spawn_counting_http_server(
        status: &str,
        body: String,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .checked("counting JWKS server should bind");
        let addr = listener
            .local_addr()
            .checked("counting JWKS server should expose local addr");
        let status = status.to_string();
        let request_count = Arc::new(AtomicUsize::new(0));
        let server_request_count = Arc::clone(&request_count);

        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener
                    .accept()
                    .await
                    .checked("counting JWKS server should accept connection");
                server_request_count.fetch_add(1, Ordering::SeqCst);
                let mut request = [0; 1024];
                socket
                    .read(&mut request)
                    .await
                    .checked("counting JWKS server should read request");
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket
                    .write_all(response.as_bytes())
                    .await
                    .checked("counting JWKS server should write response");
            }
        });

        (format!("http://{addr}/jwks"), request_count, server)
    }

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
    fn test_create_provider_allows_loopback_issuer_when_ssrf_is_explicitly_disabled() {
        let guard = synctv_common::ssrf::SsrfGuard::disabled();
        let provider = OidcProvider::create_with_ssrf_guard(
            "oidc_client_id".to_string(),
            "oidc_secret".to_string(),
            "https://example.com/callback".to_string(),
            "http://127.0.0.1:8443",
            &guard,
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
        .checked("operation should succeed");
        assert_eq!(provider.init_config.issuer, "https://issuer.example.com");
    }

    #[test]
    fn test_create_provider_rejects_custom_scheme_redirect_url() {
        let result = OidcProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "native-app://callback".to_string(),
            "https://issuer.example.com",
        );
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn test_create_with_endpoints_all_specified() {
        let provider = OidcProvider::create_with_endpoints(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "https://issuer.example.com",
            OidcEndpointOverrides {
                auth_url: Some("https://issuer.example.com/authorize".to_string()),
                token_url: Some("https://issuer.example.com/token".to_string()),
                userinfo_url: Some("https://issuer.example.com/userinfo".to_string()),
                jwks_url: Some("https://issuer.example.com/jwks".to_string()),
            },
        );
        assert!(provider.is_ok());
        let p = provider.checked("operation should succeed");
        let endpoints = p
            .init_config
            .static_endpoints
            .as_ref()
            .checked("operation should succeed");
        assert_eq!(endpoints.auth, "https://issuer.example.com/authorize");
        assert_eq!(endpoints.token, "https://issuer.example.com/token");
        assert_eq!(
            endpoints.userinfo.as_deref(),
            Some("https://issuer.example.com/userinfo")
        );
        assert_eq!(endpoints.jwks, "https://issuer.example.com/jwks");
    }

    #[test]
    fn test_create_with_endpoints_allows_loopback_token_url_when_ssrf_is_explicitly_disabled() {
        let guard = synctv_common::ssrf::SsrfGuard::disabled();
        let provider = OidcProvider::create_with_endpoints_and_ssrf_guard(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "https://issuer.example.com",
            OidcEndpointOverrides {
                auth_url: Some("https://issuer.example.com/authorize".to_string()),
                token_url: Some("http://127.0.0.1:8443/token".to_string()),
                userinfo_url: Some("https://issuer.example.com/userinfo".to_string()),
                jwks_url: Some("https://issuer.example.com/jwks".to_string()),
            },
            &guard,
        );

        assert!(provider.is_ok());
    }

    #[test]
    fn test_create_with_endpoints_rejects_missing_required_static_endpoints() {
        let result = OidcProvider::create_with_endpoints(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "https://issuer.example.com/",
            OidcEndpointOverrides {
                auth_url: None,
                token_url: Some("https://issuer.example.com/token".to_string()),
                userinfo_url: None,
                jwks_url: Some("https://issuer.example.com/jwks".to_string()),
            },
        );

        assert!(
            matches!(result, Err(Error::InvalidInput(message)) if message.contains("auth_url"))
        );
    }

    #[test]
    fn test_provider_type() {
        let provider = OidcProvider::create(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "https://issuer.example.com",
        )
        .checked("operation should succeed");
        assert_eq!(provider.provider_type(), "oidc");
    }

    #[test]
    fn test_discovery_document_accepts_standard_oidc_fields() {
        let doc: OidcDiscoveryDocument = serde_json::from_str(
            r#"{
                "issuer": "https://issuer.example.com",
                "authorization_endpoint": "https://issuer.example.com/authorize",
                "token_endpoint": "https://issuer.example.com/token",
                "userinfo_endpoint": "https://issuer.example.com/userinfo",
                "jwks_uri": "https://issuer.example.com/jwks"
            }"#,
        )
        .checked("standard OIDC discovery document should deserialize");

        assert_eq!(doc.issuer, "https://issuer.example.com");
        assert_eq!(
            doc.authorization_endpoint,
            "https://issuer.example.com/authorize"
        );
        assert_eq!(doc.token_endpoint, "https://issuer.example.com/token");
        assert_eq!(
            doc.userinfo_endpoint.as_deref(),
            Some("https://issuer.example.com/userinfo")
        );
        assert_eq!(doc.jwks_uri, "https://issuer.example.com/jwks");
    }

    #[test]
    fn test_oidc_token_extra_fields_extracts_id_token() {
        let token: OidcTokenResponse = serde_json::from_value(serde_json::json!({
            "access_token": "access",
            "token_type": "Bearer",
            "id_token": "header.payload.signature"
        }))
        .checked("OIDC token response should parse id_token");

        assert_eq!(
            token.extra_fields().id_token.as_deref(),
            Some("header.payload.signature")
        );
    }

    #[test]
    fn test_oidc_audience_detects_multiple_audiences() {
        let single = OidcAudience::One("client".to_string());
        let multiple = OidcAudience::Many(vec!["client".to_string(), "api".to_string()]);

        assert!(single.contains("client"));
        assert!(!single.is_multi());
        assert!(multiple.contains("client"));
        assert!(multiple.is_multi());
    }

    #[test]
    fn test_first_non_empty_trims_and_skips_blank_values() {
        assert_eq!(
            first_non_empty([
                Some("   ".to_string()),
                None,
                Some(" preferred ".to_string())
            ])
            .as_deref(),
            Some("preferred")
        );
    }

    #[test]
    fn test_user_info_from_id_token_claims_supports_missing_userinfo_endpoint() {
        let claims = OidcIdTokenClaims {
            iss: "https://issuer.example.com".to_string(),
            sub: "subject-123".to_string(),
            aud: OidcAudience::One("client".to_string()),
            _issued_at: 1_700_000_000,
            azp: None,
            nonce: Some("nonce".to_string()),
            preferred_username: Some("preferred_user".to_string()),
            name: Some("Display Name".to_string()),
            picture: Some("https://example.com/avatar.png".to_string()),
        };

        let user = OidcProvider::user_info_from_id_token_claims(claims);

        assert_eq!(user.provider_user_id, "subject-123");
        assert_eq!(user.username, "preferred_user");
        assert_eq!(
            user.avatar.as_deref(),
            Some("https://example.com/avatar.png")
        );
    }

    #[test]
    fn test_user_info_from_id_token_claims_uses_subject_when_profile_names_missing() {
        let claims = OidcIdTokenClaims {
            iss: "https://issuer.example.com".to_string(),
            sub: "subject-123".to_string(),
            aud: OidcAudience::One("client".to_string()),
            _issued_at: 1_700_000_000,
            azp: None,
            nonce: Some("nonce".to_string()),
            preferred_username: None,
            name: Some("   ".to_string()),
            picture: None,
        };

        let user = OidcProvider::user_info_from_id_token_claims(claims);

        assert_eq!(user.provider_user_id, "subject-123");
        assert_eq!(user.username, "subject-123");
    }

    #[test]
    fn test_userinfo_response_overrides_id_token_profile_claims() {
        let id_token_claims = OidcIdTokenClaims {
            iss: "https://issuer.example.com".to_string(),
            sub: "subject-123".to_string(),
            aud: OidcAudience::One("client".to_string()),
            _issued_at: 1_700_000_000,
            azp: None,
            nonce: Some("nonce".to_string()),
            preferred_username: Some("token_user".to_string()),
            name: Some("Token Name".to_string()),
            picture: Some("https://example.com/token.png".to_string()),
        };
        let userinfo = OidcUserInfoResponse {
            sub: "subject-123".to_string(),
            preferred_username: Some("userinfo_user".to_string()),
            name: None,
            picture: Some("https://example.com/userinfo.png".to_string()),
        };

        let user = OidcProvider::user_info_from_userinfo_response(userinfo, id_token_claims);

        assert_eq!(user.provider_user_id, "subject-123");
        assert_eq!(user.username, "userinfo_user");
        assert_eq!(
            user.avatar.as_deref(),
            Some("https://example.com/userinfo.png")
        );
    }

    #[test]
    fn test_userinfo_response_uses_subject_when_profile_names_missing() {
        let id_token_claims = OidcIdTokenClaims {
            iss: "https://issuer.example.com".to_string(),
            sub: "subject-123".to_string(),
            aud: OidcAudience::One("client".to_string()),
            _issued_at: 1_700_000_000,
            azp: None,
            nonce: Some("nonce".to_string()),
            preferred_username: None,
            name: None,
            picture: None,
        };
        let userinfo = OidcUserInfoResponse {
            sub: "subject-123".to_string(),
            preferred_username: Some(" ".to_string()),
            name: None,
            picture: None,
        };

        let user = OidcProvider::user_info_from_userinfo_response(userinfo, id_token_claims);

        assert_eq!(user.provider_user_id, "subject-123");
        assert_eq!(user.username, "subject-123");
    }

    #[test]
    fn test_oidc_id_token_algorithm_rejects_hmac() {
        assert!(!is_supported_oidc_id_token_algorithm(Algorithm::HS256));
        assert!(is_supported_oidc_id_token_algorithm(Algorithm::RS256));
    }

    #[test]
    fn test_cached_jwks_is_fresh_requires_same_uri_and_ttl() {
        let now = Instant::now();
        let cached = CachedJwks {
            jwks_uri: "https://issuer.example.com/jwks".to_string(),
            jwks: Arc::new(jwk_set_with_key(jwk_with_algorithm(Some(
                KeyAlgorithm::RS256,
            )))),
            fetched_at: now
                .checked_sub(Duration::from_mins(1))
                .checked("operation should succeed"),
            generation: 1,
        };

        assert!(cached_jwks_is_fresh(
            &cached,
            "https://issuer.example.com/jwks",
            now
        ));
        assert!(!cached_jwks_is_fresh(
            &cached,
            "https://other.example.com/jwks",
            now
        ));

        let expired = CachedJwks {
            jwks_uri: cached.jwks_uri.clone(),
            jwks: cached.jwks.clone(),
            fetched_at: now
                .checked_sub(OIDC_JWKS_CACHE_TTL)
                .checked("operation should succeed")
                .checked_sub(Duration::from_secs(1))
                .checked("operation should succeed"),
            generation: cached.generation,
        };
        assert!(!cached_jwks_is_fresh(
            &expired,
            "https://issuer.example.com/jwks",
            now
        ));
    }

    #[tokio::test]
    async fn test_jwk_for_kid_uses_fresh_cache_without_fetching() {
        let provider = OidcProvider::create_with_endpoints_and_ssrf_guard(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "http://issuer.example.com",
            OidcEndpointOverrides {
                auth_url: Some("http://issuer.example.com/authorize".to_string()),
                token_url: Some("http://issuer.example.com/token".to_string()),
                userinfo_url: None,
                jwks_url: Some("http://127.0.0.1:9/jwks".to_string()),
            },
            &synctv_common::ssrf::SsrfGuard::disabled(),
        )
        .checked("operation should succeed");
        let jwks_uri = "http://127.0.0.1:9/jwks";
        let cached_key = jwk_with_kid_and_algorithm("cached-kid", Some(KeyAlgorithm::RS256));
        *provider.jwks_cache.write().await = Some(CachedJwks {
            jwks_uri: jwks_uri.to_string(),
            jwks: Arc::new(jwk_set_with_key(cached_key.clone())),
            fetched_at: Instant::now(),
            generation: 0,
        });

        let jwk = provider
            .jwk_for_kid(jwks_uri, "cached-kid")
            .await
            .checked("operation should succeed");

        assert_eq!(jwk.common.key_id.as_deref(), Some("cached-kid"));
    }

    #[tokio::test]
    async fn test_jwk_for_kid_refreshes_cache_on_kid_miss() {
        let rotated_key = jwk_with_kid_and_algorithm("rotated-kid", Some(KeyAlgorithm::RS256));
        let jwks_uri = spawn_jwks_server(jwk_set_with_key(rotated_key)).await;
        let provider = OidcProvider::create_with_endpoints_and_ssrf_guard(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "http://issuer.example.com",
            OidcEndpointOverrides {
                auth_url: Some("http://issuer.example.com/authorize".to_string()),
                token_url: Some("http://issuer.example.com/token".to_string()),
                userinfo_url: None,
                jwks_url: Some(jwks_uri.clone()),
            },
            &synctv_common::ssrf::SsrfGuard::disabled(),
        )
        .checked("operation should succeed");
        *provider.jwks_cache.write().await = Some(CachedJwks {
            jwks_uri: jwks_uri.clone(),
            jwks: Arc::new(jwk_set_with_key(jwk_with_kid_and_algorithm(
                "old-kid",
                Some(KeyAlgorithm::RS256),
            ))),
            fetched_at: Instant::now()
                .checked_sub(OIDC_JWKS_REFRESH_COOLDOWN)
                .checked("operation should succeed"),
            generation: 0,
        });

        let jwk = provider
            .jwk_for_kid(&jwks_uri, "rotated-kid")
            .await
            .checked("operation should succeed");

        assert_eq!(jwk.common.key_id.as_deref(), Some("rotated-kid"));
        let cache = provider.jwks_cache.read().await;
        let cached = cache.as_ref().checked("JWKS cache should be refreshed");
        assert!(cached.jwks.find("rotated-kid").is_some());
    }

    #[tokio::test]
    async fn test_concurrent_jwks_cache_misses_share_one_fetch() {
        let key = jwk_with_kid_and_algorithm("cached-kid", Some(KeyAlgorithm::RS256));
        let (jwks_uri, request_count, server) =
            spawn_counting_jwks_server(jwk_set_with_key(key)).await;
        let provider = OidcProvider::create_with_endpoints_and_ssrf_guard(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "http://issuer.example.com",
            OidcEndpointOverrides {
                auth_url: Some("http://issuer.example.com/authorize".to_string()),
                token_url: Some("http://issuer.example.com/token".to_string()),
                userinfo_url: None,
                jwks_url: Some(jwks_uri.clone()),
            },
            &synctv_common::ssrf::SsrfGuard::disabled(),
        )
        .checked("operation should succeed");

        let results =
            futures::future::join_all((0..32).map(|_| provider.cached_jwks(jwks_uri.as_str())))
                .await;

        for result in results {
            let jwks = result.checked("concurrent JWKS lookup should succeed");
            assert!(jwks.jwks.find("cached-kid").is_some());
        }
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn test_cold_cache_unknown_kid_fetches_once() {
        let key = jwk_with_kid_and_algorithm("known-kid", Some(KeyAlgorithm::RS256));
        let (jwks_uri, request_count, server) =
            spawn_counting_jwks_server(jwk_set_with_key(key)).await;
        let provider = OidcProvider::create_with_endpoints_and_ssrf_guard(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "http://issuer.example.com",
            OidcEndpointOverrides {
                auth_url: Some("http://issuer.example.com/authorize".to_string()),
                token_url: Some("http://issuer.example.com/token".to_string()),
                userinfo_url: None,
                jwks_url: Some(jwks_uri.clone()),
            },
            &synctv_common::ssrf::SsrfGuard::disabled(),
        )
        .checked("operation should succeed");

        let result = provider.jwk_for_kid(&jwks_uri, "unknown-kid").await;
        let second_result = provider.jwk_for_kid(&jwks_uri, "another-unknown-kid").await;

        assert!(matches!(result, Err(Error::Authentication(_))));
        assert!(matches!(second_result, Err(Error::Authentication(_))));
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn test_concurrent_jwks_failures_share_one_fetch_attempt() {
        let (jwks_uri, request_count, server) =
            spawn_counting_http_server("503 Service Unavailable", "unavailable".to_string()).await;
        let provider = OidcProvider::create_with_endpoints_and_ssrf_guard(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "http://issuer.example.com",
            OidcEndpointOverrides {
                auth_url: Some("http://issuer.example.com/authorize".to_string()),
                token_url: Some("http://issuer.example.com/token".to_string()),
                userinfo_url: None,
                jwks_url: Some(jwks_uri.clone()),
            },
            &synctv_common::ssrf::SsrfGuard::disabled(),
        )
        .checked("operation should succeed");

        let results =
            futures::future::join_all((0..32).map(|_| provider.cached_jwks(jwks_uri.as_str())))
                .await;

        assert!(results.into_iter().all(|result| result.is_err()));
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn test_concurrent_kid_misses_share_one_refresh() {
        let rotated_key = jwk_with_kid_and_algorithm("rotated-kid", Some(KeyAlgorithm::RS256));
        let (jwks_uri, request_count, server) =
            spawn_counting_jwks_server(jwk_set_with_key(rotated_key)).await;
        let provider = OidcProvider::create_with_endpoints_and_ssrf_guard(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "http://issuer.example.com",
            OidcEndpointOverrides {
                auth_url: Some("http://issuer.example.com/authorize".to_string()),
                token_url: Some("http://issuer.example.com/token".to_string()),
                userinfo_url: None,
                jwks_url: Some(jwks_uri.clone()),
            },
            &synctv_common::ssrf::SsrfGuard::disabled(),
        )
        .checked("operation should succeed");
        *provider.jwks_cache.write().await = Some(CachedJwks {
            jwks_uri: jwks_uri.clone(),
            jwks: Arc::new(jwk_set_with_key(jwk_with_kid_and_algorithm(
                "old-kid",
                Some(KeyAlgorithm::RS256),
            ))),
            fetched_at: Instant::now()
                .checked_sub(OIDC_JWKS_REFRESH_COOLDOWN)
                .checked("operation should succeed"),
            generation: 0,
        });

        let results = futures::future::join_all(
            (0..32).map(|_| provider.jwk_for_kid(jwks_uri.as_str(), "rotated-kid")),
        )
        .await;

        for result in results {
            let jwk = result.checked("concurrent JWKS refresh should succeed");
            assert_eq!(jwk.common.key_id.as_deref(), Some("rotated-kid"));
        }
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn test_validate_id_token_accepts_missing_kid_with_single_jwks_key() {
        crate::install_process_crypto_provider();

        let jwks_uri = spawn_jwks_server(jwk_set_with_key(test_signing_jwk(None))).await;
        let provider = OidcProvider::create_with_endpoints_and_ssrf_guard(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "http://issuer.example.com",
            OidcEndpointOverrides {
                auth_url: Some("http://issuer.example.com/authorize".to_string()),
                token_url: Some("http://issuer.example.com/token".to_string()),
                userinfo_url: None,
                jwks_url: Some(jwks_uri),
            },
            &synctv_common::ssrf::SsrfGuard::disabled(),
        )
        .checked("operation should succeed");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .checked("system clock should be after epoch")
            .as_secs();
        let claims = serde_json::json!({
            "iss": "http://issuer.example.com",
            "sub": "subject-123",
            "aud": "id",
            "iat": now,
            "exp": now + 300,
            "nonce": "nonce-123"
        });
        let token = jsonwebtoken::encode(
            &Header::new(Algorithm::RS256),
            &claims,
            &EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_KEY).checked("operation should succeed"),
        )
        .checked("operation should succeed");
        let resolved = provider
            .get_resolved()
            .await
            .checked("operation should succeed");

        let id_token_claims = provider
            .validate_id_token(resolved, &token, "nonce-123")
            .await
            .checked("ID token without kid should validate against the single JWKS key");

        assert_eq!(id_token_claims.sub, "subject-123");
    }

    #[test]
    fn test_validate_jwk_for_id_token_rejects_algorithm_mismatch() {
        let jwk = jwk_with_algorithm(Some(KeyAlgorithm::RS512));
        let err = validate_jwk_for_id_token(&jwk, Algorithm::RS256).failed("operation should fail");

        assert!(
            matches!(err, Error::Authentication(message) if message.contains("algorithm")),
            "expected authentication error for mismatched key algorithm"
        );
    }

    #[test]
    fn test_validate_jwk_for_id_token_rejects_encryption_key_use() {
        let mut jwk = jwk_with_algorithm(Some(KeyAlgorithm::RS256));
        jwk.common.public_key_use = Some(PublicKeyUse::Encryption);

        let err = validate_jwk_for_id_token(&jwk, Algorithm::RS256).failed("operation should fail");

        assert!(
            matches!(err, Error::Authentication(message) if message.contains("signatures")),
            "expected authentication error for encryption-only key"
        );
    }

    #[test]
    fn test_discovery_document_rejects_nonstandard_endpoint_field_names() {
        let err = serde_json::from_str::<OidcDiscoveryDocument>(
            r#"{
                "issuer": "https://issuer.example.com",
                "authorization": "https://issuer.example.com/authorize",
                "token": "https://issuer.example.com/token",
                "userinfo": "https://issuer.example.com/userinfo",
                "jwks": "https://issuer.example.com/jwks"
            }"#,
        )
        .failed("nonstandard endpoint field names should not deserialize");

        assert!(err.to_string().contains("authorization_endpoint"));
    }

    #[tokio::test]
    async fn test_new_auth_url_with_static_endpoints() {
        let provider = OidcProvider::create_with_endpoints(
            "oidc_test_id".to_string(),
            "secret".to_string(),
            "https://example.com/callback".to_string(),
            "https://issuer.example.com",
            OidcEndpointOverrides {
                auth_url: Some("https://issuer.example.com/authorize".to_string()),
                token_url: Some("https://issuer.example.com/token".to_string()),
                userinfo_url: Some("https://issuer.example.com/userinfo".to_string()),
                jwks_url: Some("https://issuer.example.com/jwks".to_string()),
            },
        )
        .checked("operation should succeed");

        let state = "oidc_state_123";
        let auth = provider
            .new_auth_url(state, None)
            .await
            .checked("operation should succeed");
        let auth_url = auth.auth_url;
        let pkce_verifier = auth.pkce_verifier;

        // Auth URL should use the custom auth endpoint
        assert!(auth_url.starts_with("https://issuer.example.com/authorize"));
        // Should contain client_id
        assert!(auth_url.contains("client_id=oidc_test_id"));
        // Should contain state
        assert!(auth_url.contains(&format!("state={state}")));
        // Should contain redirect_uri
        assert!(auth_url.contains("redirect_uri="));
        assert!(auth_url.contains("scope=openid"));
        assert!(auth_url.contains("+profile"));
        assert!(!auth_url.contains("email"));
        assert!(auth_url.contains("nonce="));
        assert!(auth.nonce.is_some());
        // Should contain PKCE
        assert!(auth_url.contains("code_challenge="));
        assert!(auth_url.contains("code_challenge_method=S256"));
        // PKCE verifier should be non-empty
        assert!(!pkce_verifier.is_empty());
    }

    #[tokio::test]
    async fn test_apple_auth_url_uses_supported_scopes() {
        let provider = OidcProvider::create_with_endpoints(
            "org.synctv.app.web".to_string(),
            "secret".to_string(),
            "https://syncs.tv/oauth2/callback".to_string(),
            "https://appleid.apple.com",
            OidcEndpointOverrides {
                auth_url: Some("https://appleid.apple.com/auth/authorize".to_string()),
                token_url: Some("https://appleid.apple.com/auth/token".to_string()),
                userinfo_url: None,
                jwks_url: Some("https://appleid.apple.com/auth/keys".to_string()),
            },
        )
        .checked("operation should succeed");

        let auth = provider
            .new_auth_url("apple_state", None)
            .await
            .checked("operation should succeed");

        assert!(auth.auth_url.contains("scope=openid"));
        assert!(!auth.auth_url.contains("name"));
        assert!(!auth.auth_url.contains("email"));
        assert!(!auth.auth_url.contains("+profile"));
    }

    #[tokio::test]
    async fn test_custom_oidc_scopes_are_used_in_authorization_url() {
        let provider = OidcProvider::create_with_endpoints_scopes_and_ssrf_guard(
            "client".to_string(),
            "secret".to_string(),
            "https://app.example.com/callback".to_string(),
            "https://issuer.example.com",
            OidcEndpointOverrides {
                auth_url: Some("https://issuer.example.com/authorize".to_string()),
                token_url: Some("https://issuer.example.com/token".to_string()),
                userinfo_url: Some("https://issuer.example.com/userinfo".to_string()),
                jwks_url: Some("https://issuer.example.com/jwks".to_string()),
            },
            vec!["openid".to_string(), "groups".to_string()],
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .checked("custom OIDC scopes should be accepted");

        let auth = provider
            .new_auth_url("custom_scope_state", None)
            .await
            .checked("authorization URL should be generated");

        assert!(auth.auth_url.contains("scope=openid"));
        assert!(auth.auth_url.contains("+groups"));
        assert!(!auth.auth_url.contains("profile"));
        assert!(!auth.auth_url.contains("email"));
    }

    #[test]
    fn test_normalize_oidc_scopes_trims_deduplicates_and_requires_openid() {
        assert_eq!(
            normalize_oidc_scopes(
                vec![
                    " openid ".to_string(),
                    "groups".to_string(),
                    "groups".to_string(),
                ],
                "https://issuer.example.com",
            )
            .checked("valid scopes should normalize"),
            vec!["openid".to_string(), "groups".to_string()]
        );
        assert!(
            normalize_oidc_scopes(vec!["profile".to_string()], "https://issuer.example.com")
                .is_err()
        );
        assert!(normalize_oidc_scopes(
            vec!["openid".to_string(), "bad scope".to_string()],
            "https://issuer.example.com"
        )
        .is_err());
    }

    #[test]
    fn test_factory_with_issuer_only() {
        let config = oidc_private_config(
            "oidc_id",
            "oidc_secret",
            "https://example.com/oauth/oidc/callback",
            "https://issuer.example.com",
            no_oidc_endpoint_overrides(),
        );
        let provider = oidc_factory_for_test(&config);
        assert!(provider.is_ok());
        assert_eq!(
            provider.checked("operation should succeed").provider_type(),
            "oidc"
        );
    }

    #[test]
    fn test_dedicated_provider_factories_report_stable_types() {
        let guard = synctv_common::ssrf::SsrfGuard::strict_policy();
        let apple = OAuth2ProviderPrivateConfig::Apple(OAuth2AppleProviderConfig {
            client_id: "org.example.app.web".to_string(),
            client_secret: "secret".to_string(),
            redirect_url: "https://app.example.com/oauth2/callback".to_string(),
        });
        assert_eq!(
            apple_factory_from_private_config(&apple, &guard)
                .checked("Apple provider should be created")
                .provider_type(),
            "apple"
        );

        let casdoor = OAuth2ProviderPrivateConfig::Casdoor(OAuth2CasdoorProviderConfig {
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            redirect_url: "https://app.example.com/oauth2/callback".to_string(),
            issuer: "https://casdoor.example.com".to_string(),
            auth_url: None,
            token_url: None,
            userinfo_url: None,
            jwks_url: None,
        });
        assert_eq!(
            casdoor_factory_from_private_config(&casdoor, &guard)
                .checked("Casdoor provider should be created")
                .provider_type(),
            "casdoor"
        );
    }

    #[test]
    fn test_factory_with_custom_endpoints() {
        let config = oidc_private_config(
            "oidc_id",
            "oidc_secret",
            "https://example.com/cb",
            "https://issuer.example.com",
            OidcEndpointOverrides {
                auth_url: Some("https://issuer.example.com/custom/authorize".to_string()),
                token_url: Some("https://issuer.example.com/custom/token".to_string()),
                userinfo_url: Some("https://issuer.example.com/custom/userinfo".to_string()),
                jwks_url: Some("https://issuer.example.com/custom/jwks".to_string()),
            },
        );
        let provider = oidc_factory_for_test(&config);
        assert!(provider.is_ok());
    }

    #[test]
    fn test_factory_with_partial_endpoints() {
        let config = oidc_private_config(
            "id",
            "secret",
            "https://example.com/cb",
            "https://issuer.example.com",
            OidcEndpointOverrides {
                auth_url: Some("https://issuer.example.com/auth".to_string()),
                token_url: None,
                userinfo_url: None,
                jwks_url: None,
            },
        );
        let provider = oidc_factory_for_test(&config);
        assert!(
            matches!(provider, Err(Error::InvalidInput(message)) if message.contains("auth_url") && message.contains("token_url") && message.contains("jwks_url"))
        );
    }

    #[test]
    fn test_factory_missing_fields() {
        let config = oidc_private_config(
            "",
            "secret",
            "https://example.com/cb",
            "https://issuer.example.com",
            no_oidc_endpoint_overrides(),
        );
        assert!(oidc_factory_for_test(&config).is_err());

        let config = oidc_private_config(
            "id",
            "",
            "https://example.com/cb",
            "https://issuer.example.com",
            no_oidc_endpoint_overrides(),
        );
        assert!(oidc_factory_for_test(&config).is_err());

        let config = oidc_private_config(
            "id",
            "secret",
            "",
            "https://issuer.example.com",
            no_oidc_endpoint_overrides(),
        );
        assert!(oidc_factory_for_test(&config).is_err());
    }

    #[test]
    fn test_factory_default_empty_issuer_rejected() {
        let config = oidc_private_config(
            "id",
            "secret",
            "https://example.com/cb",
            "",
            no_oidc_endpoint_overrides(),
        );
        let result = oidc_factory_for_test(&config);
        assert!(result.is_err());
        assert!(matches!(result.err(), Some(Error::InvalidInput(_))));
    }

    #[test]
    fn test_factory_empty_issuer_with_custom_endpoints_rejected() {
        let config = oidc_private_config(
            "id",
            "secret",
            "https://example.com/cb",
            "",
            OidcEndpointOverrides {
                auth_url: Some("https://provider.example.com/authorize".to_string()),
                token_url: Some("https://provider.example.com/token".to_string()),
                userinfo_url: None,
                jwks_url: Some("https://provider.example.com/jwks".to_string()),
            },
        );
        let result = oidc_factory_for_test(&config);
        assert!(matches!(result, Err(Error::InvalidInput(message)) if message.contains("issuer")));
    }

    #[test]
    fn test_oidc_config_deserialize_full() {
        let json = serde_json::json!({
            "client_id": "oidc_abc",
            "client_secret": "oidc_def",
            "redirect_url": "https://example.com/cb",
            "issuer": "https://issuer.example.com",
            "auth_url": "https://issuer.example.com/auth",
            "token_url": "https://issuer.example.com/token",
            "userinfo_url": "https://issuer.example.com/userinfo",
            "jwks_url": "https://issuer.example.com/jwks"
        });
        let config: OidcConfig = serde_json::from_value(json).checked("operation should succeed");
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
        assert_eq!(
            config.jwks_url.as_deref(),
            Some("https://issuer.example.com/jwks")
        );
    }

    #[test]
    fn test_oidc_config_deserialize_minimal() {
        let json = serde_json::json!({
            "client_id": "id",
            "client_secret": "secret",
            "redirect_url": "https://example.com/cb"
        });
        let config: OidcConfig = serde_json::from_value(json).checked("operation should succeed");
        assert_eq!(config.issuer, ""); // Default
        assert!(config.auth_url.is_none());
        assert!(config.token_url.is_none());
        assert!(config.userinfo_url.is_none());
        assert!(config.jwks_url.is_none());
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
            jwks_url: None,
            scopes: Vec::new(),
        };
        let json = serde_json::to_value(&config).checked("operation should succeed");
        // Optional fields with skip_serializing_if should not appear
        assert!(json.get("auth_url").is_none());
        assert!(json.get("token_url").is_none());
        assert!(json.get("userinfo_url").is_none());
        assert!(json.get("jwks_url").is_none());
    }

    #[tokio::test]
    async fn test_get_resolved_static_endpoints_succeeds() {
        // With static endpoints, get_resolved should succeed without network
        let provider = OidcProvider::create_with_endpoints(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "https://issuer.example.com",
            OidcEndpointOverrides {
                auth_url: Some("https://issuer.example.com/authorize".to_string()),
                token_url: Some("https://issuer.example.com/token".to_string()),
                userinfo_url: Some("https://issuer.example.com/userinfo".to_string()),
                jwks_url: Some("https://issuer.example.com/jwks".to_string()),
            },
        )
        .checked("operation should succeed");

        let resolved = provider.get_resolved().await;
        assert!(resolved.is_ok());
        let r = resolved.checked("operation should succeed");
        assert_eq!(
            r.userinfo_url.as_deref(),
            Some("https://issuer.example.com/userinfo")
        );
        assert_eq!(r.jwks_uri, "https://issuer.example.com/jwks");
    }

    #[tokio::test]
    async fn test_get_resolved_caches_result() {
        // Calling get_resolved twice with static endpoints should return the same ref
        let provider = OidcProvider::create_with_endpoints(
            "id".to_string(),
            "secret".to_string(),
            "https://example.com/cb".to_string(),
            "https://issuer.example.com",
            OidcEndpointOverrides {
                auth_url: Some("https://issuer.example.com/authorize".to_string()),
                token_url: Some("https://issuer.example.com/token".to_string()),
                userinfo_url: None,
                jwks_url: Some("https://issuer.example.com/jwks".to_string()),
            },
        )
        .checked("operation should succeed");

        let r1 = provider
            .get_resolved()
            .await
            .checked("operation should succeed");
        let r2 = provider
            .get_resolved()
            .await
            .checked("operation should succeed");
        // Same pointer (OnceCell caches the result)
        assert!(std::ptr::eq(r1, r2));
    }
}
