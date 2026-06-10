use synctv_common::ExecutionControl;
use tracing::debug;

use crate::{
    models::UserId,
    service::oauth2::{OAuth2Service, OAuth2State, PreparedOAuth2Authorization},
    Error, InternalExt, Result,
};

impl OAuth2Service {
    pub async fn get_authorization_url(
        &self,
        instance_name: &str,
        redirect_url: Option<String>,
    ) -> Result<(String, String)> {
        self.get_authorization_url_with_control(instance_name, redirect_url, None)
            .await
    }

    pub async fn get_authorization_url_with_control(
        &self,
        instance_name: &str,
        redirect_url: Option<String>,
        control: Option<&ExecutionControl>,
    ) -> Result<(String, String)> {
        self.build_authorization_url(instance_name, redirect_url, None, control)
            .await
    }

    pub async fn get_authorization_url_with_user(
        &self,
        instance_name: &str,
        redirect_url: Option<String>,
        user_id: Option<UserId>,
    ) -> Result<(String, String)> {
        self.get_authorization_url_with_user_with_control(
            instance_name,
            redirect_url,
            user_id,
            None,
        )
        .await
    }

    pub async fn get_authorization_url_with_user_with_control(
        &self,
        instance_name: &str,
        redirect_url: Option<String>,
        user_id: Option<UserId>,
        control: Option<&ExecutionControl>,
    ) -> Result<(String, String)> {
        self.build_authorization_url(instance_name, redirect_url, user_id, control)
            .await
    }

    pub async fn prepare_authorization_url_with_control(
        &self,
        instance_name: &str,
        redirect_url: Option<String>,
        bind_user_id: Option<UserId>,
        control: Option<&ExecutionControl>,
    ) -> Result<PreparedOAuth2Authorization> {
        if let Some(ref url) = redirect_url {
            Self::validate_redirect_url_with_allowlist(url, &self.allowed_redirect_domains)?;
        }

        let provider = self.provider_entry(instance_name).await?.provider;

        let state_token = synctv_common::snanoid!(32);
        let auth_redirect_url = redirect_url.as_deref();
        let auth = Self::run_with_control(control, async {
            provider
                .new_auth_url(&state_token, auth_redirect_url)
                .await
                .internal_with_err("Failed to generate authorization URL")
        })
        .await?;

        let oauth_state = OAuth2State {
            instance_name: instance_name.to_string(),
            redirect_url,
            created_at: chrono::Utc::now(),
            bind_user_id,
            pkce_verifier: auth.pkce_verifier,
            nonce: auth.nonce,
        };

        Ok(PreparedOAuth2Authorization {
            auth_url: auth.auth_url,
            state_token,
            oauth_state,
        })
    }

    pub async fn store_prepared_authorization_with_control(
        &self,
        prepared: &PreparedOAuth2Authorization,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        self.store_state_with_control(&prepared.state_token, &prepared.oauth_state, control)
            .await
    }

    async fn build_authorization_url(
        &self,
        instance_name: &str,
        redirect_url: Option<String>,
        bind_user_id: Option<UserId>,
        control: Option<&ExecutionControl>,
    ) -> Result<(String, String)> {
        let prepared = self
            .prepare_authorization_url_with_control(
                instance_name,
                redirect_url,
                bind_user_id,
                control,
            )
            .await?;
        self.store_prepared_authorization_with_control(&prepared, control)
            .await?;

        debug!(
            "Generated OAuth2 authorization URL for provider {}",
            instance_name
        );

        Ok((prepared.auth_url, prepared.state_token))
    }

    pub(super) fn validate_redirect_url_with_allowlist(
        url: &str,
        allowed_domains: &[String],
    ) -> Result<()> {
        if url.trim().is_empty() {
            return Err(Error::InvalidInput(
                "Redirect URL cannot be empty".to_string(),
            ));
        }

        match url::Url::parse(url) {
            Ok(parsed_url) => {
                let scheme = parsed_url.scheme();

                if parsed_url.username() != "" || parsed_url.password().is_some() {
                    return Err(Error::InvalidInput(
                        "URLs with embedded credentials are not allowed".to_string(),
                    ));
                }

                if scheme != "http" && scheme != "https" {
                    return Err(Error::InvalidInput(format!(
                        "Invalid URL scheme: {scheme}. Only http and https are allowed"
                    )));
                }

                let host = parsed_url.host_str().ok_or_else(|| {
                    Error::InvalidInput("Redirect URL must include a host".to_string())
                })?;
                if Self::is_loopback_host(host) {
                    return Ok(());
                }
                if scheme != "https" {
                    return Err(Error::InvalidInput(
                        "Only HTTPS redirect URLs are allowed for non-loopback hosts".to_string(),
                    ));
                }
                if allowed_domains.is_empty() {
                    return Err(Error::InvalidInput(
                        "Redirect URL domain allowlist is empty and the host is not loopback."
                            .to_string(),
                    ));
                }
                if !Self::redirect_host_matches_allowlist(host, allowed_domains) {
                    return Err(Error::InvalidInput(format!(
                        "Redirect URL domain '{host}' is not in the allowed domains list"
                    )));
                }

                Ok(())
            }
            Err(_) => Err(Error::InvalidInput(
                "Redirect URL must be an absolute http(s) URL with a host".to_string(),
            )),
        }
    }

    fn is_loopback_host(host: &str) -> bool {
        matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
    }

    fn redirect_host_matches_allowlist(host: &str, allowed_domains: &[String]) -> bool {
        allowed_domains
            .iter()
            .any(|domain| Self::redirect_domain_matches(host, domain))
    }

    fn redirect_domain_matches(host: &str, allowed_domain: &str) -> bool {
        if !allowed_domain.contains('.') {
            return false;
        }
        if host == allowed_domain {
            return true;
        }

        let suffix = format!(".{allowed_domain}");
        host.strip_suffix(&suffix)
            .is_some_and(|prefix| !prefix.contains('.'))
    }
}
