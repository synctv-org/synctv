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

use crate::{resilience::timeout::HTTP_REQUEST_TIMEOUT, Error, InternalExt};
use reqwest::Client;
use std::{sync::Arc, time::Duration};

pub(super) fn build_provider_http_client_with_timeout(timeout: Duration) -> Result<Client, Error> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .internal_with_err("Failed to build HTTP client")
}

pub(super) fn build_provider_http_client() -> Result<Arc<Client>, Error> {
    Ok(Arc::new(build_provider_http_client_with_timeout(
        HTTP_REQUEST_TIMEOUT,
    )?))
}

pub(super) fn build_oauth2_http_client_with_timeout(
    timeout: Duration,
) -> Result<oauth2::reqwest::Client, Error> {
    oauth2::reqwest::ClientBuilder::new()
        .redirect(oauth2::reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .internal_with_err("Failed to build OAuth2 HTTP client")
}

pub(super) fn build_oauth2_http_client() -> Result<Arc<oauth2::reqwest::Client>, Error> {
    Ok(Arc::new(build_oauth2_http_client_with_timeout(
        HTTP_REQUEST_TIMEOUT,
    )?))
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
    use std::future::pending;
    use tokio::{
        io::AsyncReadExt,
        net::TcpListener,
        task::JoinHandle,
        time::{timeout, Duration},
    };

    async fn spawn_hanging_http_server() -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 1024];
            let _ = stream.read(&mut buf).await;
            pending::<()>().await;
        });

        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn provider_http_client_times_out_hanging_userinfo_requests() {
        let client = build_provider_http_client_with_timeout(Duration::from_millis(50)).unwrap();
        let (base_url, server_handle) = spawn_hanging_http_server().await;

        let result = timeout(
            Duration::from_millis(250),
            client.get(format!("{base_url}/userinfo")).send(),
        )
        .await;
        server_handle.abort();

        let err = match result {
            Ok(Err(err)) => err,
            Ok(Ok(_)) => panic!("expected request to fail with timeout"),
            Err(_) => panic!("request client did not enforce its own timeout"),
        };
        let mapped = map_provider_http_error("Failed to fetch user info", err);

        assert!(matches!(
            mapped,
            Error::Timeout(ref msg) if msg.contains("Failed to fetch user info")
        ));
    }

    #[tokio::test]
    async fn provider_http_timeout_maps_to_core_timeout_error() {
        let client = build_provider_http_client_with_timeout(Duration::from_millis(50)).unwrap();
        let (base_url, server_handle) = spawn_hanging_http_server().await;

        let err = client
            .get(format!("{base_url}/userinfo"))
            .send()
            .await
            .expect_err("request should time out");
        server_handle.abort();

        let mapped = map_provider_http_error("Failed to fetch user info", err);
        assert!(matches!(
            mapped,
            Error::Timeout(ref msg) if msg.contains("Failed to fetch user info")
        ));
    }

    #[tokio::test]
    async fn token_exchange_client_times_out_hanging_token_endpoint() {
        let http_client = build_oauth2_http_client_with_timeout(Duration::from_millis(50)).unwrap();
        let (base_url, server_handle) = spawn_hanging_http_server().await;

        let client = BasicClient::new(ClientId::new("client_id".to_string()))
            .set_client_secret(ClientSecret::new("client_secret".to_string()))
            .set_auth_uri(AuthUrl::new("https://example.com/auth".to_string()).unwrap())
            .set_token_uri(TokenUrl::new(format!("{base_url}/token")).unwrap())
            .set_redirect_uri(
                RedirectUrl::new("https://example.com/callback".to_string()).unwrap(),
            );

        let result = timeout(
            Duration::from_millis(250),
            client
                .exchange_code(AuthorizationCode::new("code".to_string()))
                .request_async(&http_client),
        )
        .await;
        server_handle.abort();

        let err = match result {
            Ok(Err(err)) => err,
            Ok(Ok(_)) => panic!("expected token exchange to fail with timeout"),
            Err(_) => panic!("token exchange client did not enforce its own timeout"),
        };
        let mapped = map_provider_http_error("Failed to exchange code", err);
        assert!(matches!(
            mapped,
            Error::Timeout(ref msg) if msg.contains("Failed to exchange code")
        ));
    }
}
