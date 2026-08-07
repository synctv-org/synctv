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
use std::time::Duration;
use std::{future::Future, pin::Pin, sync::Arc};
use url::{Host, Url};

const OAUTH2_PROVIDER_HTTP_TIMEOUT: std::time::Duration =
    crate::resilience::timeout::HTTP_REQUEST_TIMEOUT;

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

            let status = response.status();
            let version = response.version();
            let headers = response.headers().clone();
            let body = response.bytes().await.map_err(Box::new)?.into();
            let mut response = HttpResponse::new(body);
            *response.status_mut() = status;
            *response.version_mut() = version;
            *response.headers_mut() = headers;
            Ok(response)
        })
    }
}

fn build_ssrf_safe_provider_client(
    timeout: Duration,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<reqwest::Client, Error> {
    synctv_common::http::SsrfSafeClientBuilder::new()
        .ssrf_guard(ssrf_guard.clone())
        .connect_timeout(Duration::from_secs(10))
        .request_timeout(timeout)
        .pool_max_idle_per_host(10)
        .build()
        .internal_with_err("Failed to build HTTP client")
}

pub(super) fn validate_provider_url(
    url: &str,
    context: &str,
    guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Url, Error> {
    let parsed = Url::parse(url)
        .map_err(|err| Error::InvalidInput(format!("{context}: invalid URL: {err}")))?;

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
                let port_allowed_for_ip = match parsed.host() {
                    Some(Host::Ipv4(ip)) => !guard.is_port_blocked_for_ip(port, &ip.into()),
                    Some(Host::Ipv6(ip)) => !guard.is_port_blocked_for_ip(port, &ip.into()),
                    _ => false,
                };
                if !port_allowed_for_ip {
                    return Err(Error::InvalidInput(format!(
                        "{context}: port '{port}' is not allowed"
                    )));
                }
            }
        }
    }

    Ok(parsed)
}

pub(super) fn validate_oauth2_redirect_url(url: &str, context: &str) -> Result<(), Error> {
    let parsed = Url::parse(url)
        .map_err(|err| Error::InvalidInput(format!("{context}: invalid URL: {err}")))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(Error::InvalidInput(format!(
            "{context}: scheme '{scheme}' is not allowed"
        )));
    }
    if parsed.host().is_none() {
        return Err(Error::InvalidInput(format!(
            "{context}: URL must include a host"
        )));
    }
    Ok(())
}

pub(super) fn validate_required_oauth2_field(
    provider: &str,
    field: &str,
    value: &str,
) -> Result<(), Error> {
    if value.trim().is_empty() {
        return Err(Error::InvalidInput(format!(
            "{provider} OAuth2 config requires {field}"
        )));
    }
    Ok(())
}

pub(super) fn build_provider_http_client(
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Arc<Client>, Error> {
    build_ssrf_safe_provider_client(OAUTH2_PROVIDER_HTTP_TIMEOUT, ssrf_guard)
        .map(Arc::new)
        .internal_with_err("Failed to build HTTP client")
}

#[cfg(test)]
pub(super) fn build_oauth2_http_client_with_timeout(
    timeout: Duration,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<OAuth2HttpClient, Error> {
    build_ssrf_safe_provider_client(timeout, ssrf_guard)
        .map(OAuth2HttpClient::new)
        .internal_with_err("Failed to build OAuth2 HTTP client")
}

pub(super) fn build_oauth2_http_client(
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Arc<OAuth2HttpClient>, Error> {
    build_ssrf_safe_provider_client(OAUTH2_PROVIDER_HTTP_TIMEOUT, ssrf_guard)
        .map(OAuth2HttpClient::new)
        .map(Arc::new)
        .internal_with_err("Failed to build OAuth2 HTTP client")
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
pub fn provider_registry(
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
) -> crate::oauth2::ProviderRegistry {
    let registry = crate::oauth2::ProviderRegistry::new();
    let github_guard = ssrf_guard.clone();
    registry.register(
        "github",
        Arc::new(move |config| github::github_factory_from_private_config(config, &github_guard)),
    );
    let google_guard = ssrf_guard.clone();
    registry.register(
        "google",
        Arc::new(move |config| google::google_factory_from_private_config(config, &google_guard)),
    );
    let logto_guard = ssrf_guard.clone();
    registry.register(
        "logto",
        Arc::new(move |config| logto::logto_factory_from_private_config(config, &logto_guard)),
    );
    let casdoor_guard = ssrf_guard.clone();
    registry.register(
        "casdoor",
        Arc::new(move |config| oidc::casdoor_factory_from_private_config(config, &casdoor_guard)),
    );
    let apple_guard = ssrf_guard.clone();
    registry.register(
        "apple",
        Arc::new(move |config| oidc::apple_factory_from_private_config(config, &apple_guard)),
    );
    let oidc_guard = ssrf_guard;
    registry.register(
        "oidc",
        Arc::new(move |config| oidc::oidc_factory_from_private_config(config, &oidc_guard)),
    );
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::TestResultExt;
    use oauth2::{
        basic::BasicClient, AuthUrl, AuthorizationCode, ClientId, ClientSecret, RedirectUrl,
        TokenUrl,
    };
    use std::io::ErrorKind;
    use tokio::time::Duration;

    #[test]
    fn validate_provider_url_allows_loopback_ips_when_ssrf_is_explicitly_disabled() {
        let guard = synctv_common::ssrf::SsrfGuard::disabled();
        let parsed = validate_provider_url(
            "http://127.0.0.1:8080/userinfo",
            "Unsafe userinfo endpoint",
            &guard,
        )
        .checked("disabled SSRF policy should allow loopback IPs");
        assert_eq!(parsed.as_str(), "http://127.0.0.1:8080/userinfo");
    }

    #[test]
    fn validate_provider_url_allows_localhost_when_ssrf_is_explicitly_disabled() {
        let guard = synctv_common::ssrf::SsrfGuard::disabled();
        let parsed = validate_provider_url(
            "http://localhost:8080/token",
            "Unsafe token endpoint",
            &guard,
        )
        .checked("disabled SSRF policy should allow localhost");
        assert_eq!(parsed.as_str(), "http://localhost:8080/token");
    }

    #[test]
    fn validate_provider_url_allows_private_ip_non_default_port_when_configured() {
        let guard = synctv_common::ssrf::SsrfGuard::builder()
            .allow_private_network_targets(true)
            .build();
        let parsed = validate_provider_url(
            "http://127.0.0.1:18000/.well-known/openid-configuration",
            "OIDC issuer URL",
            &guard,
        )
        .checked("private-network policy should allow loopback provider ports");
        assert_eq!(
            parsed.as_str(),
            "http://127.0.0.1:18000/.well-known/openid-configuration"
        );
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
        let guard = synctv_common::ssrf::SsrfGuard::disabled();
        let http_client = build_oauth2_http_client_with_timeout(Duration::from_millis(50), &guard)
            .checked("operation should succeed");

        let client = BasicClient::new(ClientId::new("client_id".to_string()))
            .set_client_secret(ClientSecret::new("client_secret".to_string()))
            .set_auth_uri(
                AuthUrl::new("https://example.com/auth".to_string())
                    .checked("operation should succeed"),
            )
            .set_token_uri(
                TokenUrl::new("http://localhost/token".to_string())
                    .checked("operation should succeed"),
            )
            .set_redirect_uri(
                RedirectUrl::new("https://example.com/callback".to_string())
                    .checked("operation should succeed"),
            );

        let err = client
            .exchange_code(AuthorizationCode::new("code".to_string()))
            .request_async(&http_client)
            .await
            .failed("localhost request should still fail because no token server is running");
        let mapped = map_provider_http_error("Failed to exchange code", err);
        assert!(matches!(
            mapped,
            Error::Internal(ref msg) | Error::Timeout(ref msg)
                if msg.contains("Failed to exchange code")
        ));
    }
}
