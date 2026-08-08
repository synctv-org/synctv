//! Microsoft identity platform provider.
//!
//! Microsoft exposes OIDC endpoints under a tenant-specific OAuth2 surface.
//! This module owns tenant URL construction and delegates OIDC discovery
//! semantics, token validation, and profile parsing to the generic provider.

use super::oidc::{OidcEndpointOverrides, OidcProvider};
use super::{validate_provider_url, validate_required_oauth2_field};
use crate::oauth2::{OAuth2Authorization, OAuth2AuthorizationMode, OAuth2UserInfo, Provider};
use crate::service::OAuth2ProviderPrivateConfig;
use crate::Error;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

const MICROSOFT_USERINFO_URL: &str = "https://graph.microsoft.com/oidc/userinfo";

/// Microsoft identity platform configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MicrosoftConfig {
    pub client_id: String,
    pub client_secret: String,
    /// Tenant ID, tenant domain, or one of Microsoft's multi-tenant aliases.
    pub tenant: String,
}

pub struct MicrosoftProvider {
    oidc: OidcProvider,
}

impl MicrosoftProvider {
    fn create(
        config: &MicrosoftConfig,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Self, Error> {
        let tenant = config.tenant.trim();
        validate_microsoft_tenant(tenant)?;
        let auth_url = format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/authorize");
        let token_url = format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token");
        let jwks_url = format!("https://login.microsoftonline.com/{tenant}/discovery/v2.0/keys");
        for (url, context) in [
            (&auth_url, "Invalid Microsoft authorization URL"),
            (&token_url, "Invalid Microsoft token URL"),
            (&jwks_url, "Invalid Microsoft JWKS URL"),
        ] {
            validate_provider_url(url, context, ssrf_guard)?;
        }

        let issuer_tenant = if is_multi_tenant_alias(tenant) {
            "{tenantid}"
        } else {
            tenant
        };
        let issuer = format!("https://login.microsoftonline.com/{issuer_tenant}/v2.0");
        let oidc = OidcProvider::create_with_endpoints_scopes_and_ssrf_guard(
            config.client_id.clone(),
            config.client_secret.clone(),
            &issuer,
            OidcEndpointOverrides {
                auth_url: Some(auth_url),
                token_url: Some(token_url),
                userinfo_url: Some(MICROSOFT_USERINFO_URL.to_string()),
                jwks_url: Some(jwks_url),
            },
            vec!["openid".to_string(), "profile".to_string()],
            ssrf_guard,
        )?
        .with_microsoft_tenant_issuer_policy();

        Ok(Self { oidc })
    }
}

#[async_trait]
impl Provider for MicrosoftProvider {
    fn validate_authorization_redirect_url(&self, redirect_url: Option<&str>) -> Result<(), Error> {
        self.oidc.validate_authorization_redirect_url(redirect_url)
    }

    async fn new_auth_url(
        &self,
        state: &str,
        redirect_url: Option<&str>,
        mode: OAuth2AuthorizationMode,
    ) -> Result<OAuth2Authorization, Error> {
        self.oidc.new_auth_url(state, redirect_url, mode).await
    }

    async fn get_user_info(
        &self,
        code: &str,
        redirect_url: Option<&str>,
        pkce_verifier: Option<&str>,
        nonce: Option<&str>,
        mode: OAuth2AuthorizationMode,
    ) -> Result<OAuth2UserInfo, Error> {
        self.oidc
            .get_user_info(code, redirect_url, pkce_verifier, nonce, mode)
            .await
    }
}

fn is_multi_tenant_alias(tenant: &str) -> bool {
    tenant.eq_ignore_ascii_case("common")
        || tenant.eq_ignore_ascii_case("organizations")
        || tenant.eq_ignore_ascii_case("consumers")
}

fn validate_microsoft_tenant(tenant: &str) -> Result<(), Error> {
    if tenant.is_empty() || tenant.len() > 128 {
        return Err(Error::InvalidInput(
            "Microsoft OAuth config requires a valid tenant".to_string(),
        ));
    }
    if tenant.contains('/')
        || tenant.contains('?')
        || tenant.contains('#')
        || tenant.chars().any(char::is_whitespace)
    {
        return Err(Error::InvalidInput(
            "Microsoft OAuth tenant contains invalid characters".to_string(),
        ));
    }
    Ok(())
}

pub fn microsoft_factory_from_private_config(
    config: &OAuth2ProviderPrivateConfig,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    let OAuth2ProviderPrivateConfig::Microsoft(config) = config else {
        return Err(Error::InvalidInput(
            "Microsoft provider requires microsoft config".to_string(),
        ));
    };
    let config = MicrosoftConfig {
        client_id: config.client_id.clone(),
        client_secret: config.client_secret.clone(),
        tenant: config.tenant.clone(),
    };
    validate_required_oauth2_field("Microsoft", "client_id", &config.client_id)?;
    validate_required_oauth2_field("Microsoft", "client_secret", &config.client_secret)?;
    Ok(Box::new(MicrosoftProvider::create(&config, ssrf_guard)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::OAuth2MicrosoftProviderConfig;
    use crate::test_helpers::TestResultExt;

    fn config() -> OAuth2ProviderPrivateConfig {
        OAuth2ProviderPrivateConfig::Microsoft(OAuth2MicrosoftProviderConfig {
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            tenant: "common".to_string(),
        })
    }

    #[test]
    fn factory_accepts_common_tenant() {
        let provider = microsoft_factory_from_private_config(
            &config(),
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .checked("Microsoft provider should be created");
        assert!(provider.validate_authorization_redirect_url(None).is_ok());
    }

    #[tokio::test]
    async fn authorization_uses_tenant_endpoint_and_pkce() {
        let provider = MicrosoftProvider::create(
            &MicrosoftConfig {
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                tenant: "common".to_string(),
            },
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .checked("Microsoft provider should be created");
        let auth = provider
            .new_auth_url(
                "state",
                Some("https://app.example.com/callback"),
                OAuth2AuthorizationMode::Browser,
            )
            .await
            .checked("authorization URL should be generated");
        assert!(auth
            .auth_url
            .starts_with("https://login.microsoftonline.com/common/oauth2/v2.0/authorize"));
        assert!(!auth.pkce_verifier.is_empty());
        assert!(auth.nonce.is_some());
    }
}
