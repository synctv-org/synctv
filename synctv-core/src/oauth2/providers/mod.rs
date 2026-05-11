//! `OAuth2` provider implementations
//!
//! Each provider is implemented as a separate module with:
//! 1. Its own provider struct
//! 2. A `create()` factory function
//! 3. A public factory function for registration
//!
//! Factory pattern: providers are registered once, then created multiple times with different configs.

pub mod github;
pub mod google;
pub mod logto;
pub mod oidc;

// Re-export provider structs and config structs for convenience
pub use github::{GitHubConfig, GitHubProvider};
pub use google::{GoogleConfig, GoogleProvider};
pub use logto::{LogtoConfig, LogtoProvider};
pub use oidc::{OidcConfig, OidcProvider};

use crate::{Error, InternalExt};
use oauth2::{AsyncHttpClient, HttpClientError, HttpRequest, HttpResponse};
use reqwest::Client;
#[cfg(test)]
use std::time::Duration;
use std::{future::Future, pin::Pin, sync::Arc};
use url::{Host, Url};

pub(super) struct OAuth2HttpClient {
    client: reqwest::Client,
}

impl OAuth2HttpClient {
    const fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl<'c> AsyncHttpClient<'c> for OAuth2HttpClient {
    type Error = HttpClientError<reqwest::Error>;
    type Future =
        Pin<Box<dyn Future<Output = Result<HttpResponse, Self::Error>> + Send + Sync + 'c>>;

    fn call(&'c self, request: HttpRequest) -> Self::Future {
        Box::pin(async move {
            let response = self
                .client
                .execute(request.try_into().map_err(Box::new)?)
                .await
                .map_err(Box::new)?;

            let mut builder = http::Response::builder()
                .status(response.status())
                .version(response.version());

            for (name, value) in response.headers() {
                builder = builder.header(name, value);
            }

            builder
                .body(response.bytes().await.map_err(Box::new)?.to_vec())
                .map_err(HttpClientError::Http)
        })
    }
}

#[cfg(test)]
fn build_ssrf_safe_provider_client(timeout: Duration) -> Result<reqwest::Client, Error> {
    synctv_common::http::SsrfSafeClientBuilder::new()
        .connect_timeout(Duration::from_secs(10))
        .request_timeout(timeout)
        .pool_max_idle_per_host(10)
        .build()
        .internal_with_err("Failed to build HTTP client")
}

pub(super) fn validate_provider_url(url: &str, context: &str) -> Result<Url, Error> {
    let parsed = Url::parse(url)
        .map_err(|err| Error::InvalidInput(format!("{context}: invalid URL: {err}")))?;
    let guard = synctv_common::ssrf::SsrfGuard::shared_default();

    if let Some(acl) = guard.acl() {
        if acl.is_scheme_allowed(parsed.scheme()).is_denied() {
            return Err(Error::InvalidInput(format!(
                "{context}: scheme '{}' is not allowed",
                parsed.scheme()
            )));
        }
    }

    match parsed.host() {
        Some(Host::Domain(host)) if guard.is_host_blocked(host) => {
            return Err(Error::InvalidInput(format!(
                "{context}: host '{host}' is not allowed"
            )));
        }
        Some(Host::Ipv4(ip)) if guard.is_ip_blocked(&ip.into()) => {
            return Err(Error::InvalidInput(format!(
                "{context}: ip '{ip}' is not allowed"
            )));
        }
        Some(Host::Ipv6(ip)) if guard.is_ip_blocked(&ip.into()) => {
            return Err(Error::InvalidInput(format!(
                "{context}: ip '{ip}' is not allowed"
            )));
        }
        Some(_) => {}
        None => {
            return Err(Error::InvalidInput(format!(
                "{context}: URL must include a host"
            )));
        }
    }

    if let Some(port) = parsed.port_or_known_default() {
        if let Some(acl) = guard.acl() {
            if acl.is_port_allowed(port).is_denied() {
                return Err(Error::InvalidInput(format!(
                    "{context}: port '{port}' is not allowed"
                )));
            }
        }
    }

    Ok(parsed)
}

pub(super) fn build_provider_http_client() -> Result<Arc<Client>, Error> {
    Ok(Arc::new(
        synctv_common::http::SsrfSafeClientBuilder::new()
            .connect_timeout(std::time::Duration::from_secs(10))
            .disable_request_timeout()
            .pool_max_idle_per_host(10)
            .build()
            .internal_with_err("Failed to build HTTP client")?,
    ))
}

#[cfg(test)]
pub(super) fn build_oauth2_http_client_with_timeout(
    timeout: Duration,
) -> Result<OAuth2HttpClient, Error> {
    build_ssrf_safe_provider_client(timeout)
        .map(OAuth2HttpClient::new)
        .internal_with_err("Failed to build OAuth2 HTTP client")
}

pub(super) fn build_oauth2_http_client() -> Result<Arc<OAuth2HttpClient>, Error> {
    Ok(Arc::new(
        synctv_common::http::SsrfSafeClientBuilder::new()
            .connect_timeout(std::time::Duration::from_secs(10))
            .disable_request_timeout()
            .pool_max_idle_per_host(10)
            .build()
            .internal_with_err("Failed to build OAuth2 HTTP client")
            .map(OAuth2HttpClient::new)?,
    ))
}

pub(super) fn map_provider_http_error<E>(context: &str, err: E) -> Error
where
    E: std::error::Error + 'static,
{
    let err_debug = format!("{err:?}").to_lowercase();
    let err_display = err.to_string().to_lowercase();
    if crate::resilience::retry::should_retry_error(&err)
        || err_debug.contains("timedout")
        || err_debug.contains("timeout")
        || err_display.contains("timed out")
        || err_display.contains("timeout")
    {
        Error::Timeout(format!("{context}: {err}"))
    } else {
        Error::Internal(format!("{context}: {err}"))
    }
}

/// Build a registry populated with all built-in `OAuth2` providers.
#[must_use]
pub fn provider_registry() -> crate::oauth2::ProviderRegistry {
    let registry = crate::oauth2::ProviderRegistry::new();
    registry.register("github", github::github_factory);
    registry.register("google", google::google_factory);
    registry.register("logto", logto::logto_factory);
    registry.register("oidc", oidc::oidc_factory);
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use oauth2::{
        basic::BasicClient, AuthUrl, AuthorizationCode, ClientId, ClientSecret, RedirectUrl,
        TokenUrl,
    };
    use std::io::ErrorKind;
    use tokio::time::Duration;

    #[test]
    fn validate_provider_url_allows_loopback_ips_when_default_ssrf_is_disabled() {
        let parsed =
            validate_provider_url("http://127.0.0.1:8080/userinfo", "Unsafe userinfo endpoint")
                .expect("default SSRF policy should allow loopback IPs");
        assert_eq!(parsed.as_str(), "http://127.0.0.1:8080/userinfo");
    }

    #[test]
    fn validate_provider_url_allows_localhost_when_default_ssrf_is_disabled() {
        let parsed = validate_provider_url("http://localhost:8080/token", "Unsafe token endpoint")
            .expect("default SSRF policy should allow localhost");
        assert_eq!(parsed.as_str(), "http://localhost:8080/token");
    }

    #[test]
    fn provider_http_timeout_maps_to_core_timeout_error() {
        let err = std::io::Error::new(ErrorKind::TimedOut, "timed out");
        let mapped = map_provider_http_error("Failed to fetch user info", err);
        assert!(matches!(
            mapped,
            Error::Timeout(ref msg) if msg.contains("Failed to fetch user info")
        ));
    }

    #[tokio::test]
    async fn token_exchange_client_allows_localhost_but_request_still_fails_without_server() {
        let http_client = build_oauth2_http_client_with_timeout(Duration::from_millis(50)).unwrap();

        let client = BasicClient::new(ClientId::new("client_id".to_string()))
            .set_client_secret(ClientSecret::new("client_secret".to_string()))
            .set_auth_uri(AuthUrl::new("https://example.com/auth".to_string()).unwrap())
            .set_token_uri(TokenUrl::new("http://localhost/token".to_string()).unwrap())
            .set_redirect_uri(
                RedirectUrl::new("https://example.com/callback".to_string()).unwrap(),
            );

        let err = client
            .exchange_code(AuthorizationCode::new("code".to_string()))
            .request_async(&http_client)
            .await
            .expect_err("localhost request should still fail because no token server is running");
        let mapped = map_provider_http_error("Failed to exchange code", err);
        assert!(matches!(
            mapped,
            Error::Internal(ref msg) | Error::Timeout(ref msg)
                if msg.contains("Failed to exchange code")
        ));
    }
}
