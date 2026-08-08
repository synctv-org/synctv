//! Casdoor OAuth2 provider.
//!
//! Casdoor exposes a standard OIDC interface. This module owns Casdoor's
//! configuration boundary while delegating discovery, token validation, and
//! profile handling to the generic [`super::oidc::OidcProvider`].

use super::oidc::{create_from_config, OidcConfig, OidcProvider};
use super::validate_required_oauth2_field;
use crate::oauth2::{OAuth2Authorization, OAuth2AuthorizationMode, OAuth2UserInfo, Provider};
use crate::service::OAuth2ProviderPrivateConfig;
use crate::Error;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Casdoor provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CasdoorConfig {
    pub client_id: String,
    pub client_secret: String,
    pub issuer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub userinfo_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jwks_url: Option<String>,
}

/// Casdoor provider backed by the generic OIDC implementation.
pub struct CasdoorProvider {
    oidc: OidcProvider,
}

impl CasdoorProvider {
    fn create(
        config: &CasdoorConfig,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Self, Error> {
        let oidc = create_from_config(
            OidcConfig {
                client_id: config.client_id.clone(),
                client_secret: config.client_secret.clone(),
                issuer: config.issuer.clone(),
                auth_url: config.auth_url.clone(),
                token_url: config.token_url.clone(),
                userinfo_url: config.userinfo_url.clone(),
                jwks_url: config.jwks_url.clone(),
                scopes: Vec::new(),
            },
            ssrf_guard,
        )?;
        Ok(Self { oidc })
    }
}

#[async_trait]
impl Provider for CasdoorProvider {
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

pub fn casdoor_factory_from_private_config(
    config: &OAuth2ProviderPrivateConfig,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    let OAuth2ProviderPrivateConfig::Casdoor(config) = config else {
        return Err(Error::InvalidInput(
            "Casdoor provider requires casdoor config".to_string(),
        ));
    };
    let config = CasdoorConfig {
        client_id: config.client_id.clone(),
        client_secret: config.client_secret.clone(),
        issuer: config.issuer.clone(),
        auth_url: config.auth_url.clone(),
        token_url: config.token_url.clone(),
        userinfo_url: config.userinfo_url.clone(),
        jwks_url: config.jwks_url.clone(),
    };
    validate_required_oauth2_field("Casdoor", "client_id", &config.client_id)?;
    validate_required_oauth2_field("Casdoor", "client_secret", &config.client_secret)?;
    validate_required_oauth2_field("Casdoor", "issuer", &config.issuer)?;
    Ok(Box::new(CasdoorProvider::create(&config, ssrf_guard)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::OAuth2CasdoorProviderConfig;
    use crate::test_helpers::TestResultExt;

    fn config() -> OAuth2ProviderPrivateConfig {
        OAuth2ProviderPrivateConfig::Casdoor(OAuth2CasdoorProviderConfig {
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            issuer: "https://casdoor.example.com".to_string(),
            auth_url: None,
            token_url: None,
            userinfo_url: None,
            jwks_url: None,
        })
    }

    #[test]
    fn factory_accepts_casdoor_config() {
        let provider = casdoor_factory_from_private_config(
            &config(),
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .checked("Casdoor provider should be created");
        assert!(provider.validate_authorization_redirect_url(None).is_ok());
    }

    #[test]
    fn factory_rejects_non_casdoor_config() {
        let config = OAuth2ProviderPrivateConfig::Oidc(crate::service::OAuth2OidcProviderConfig {
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            issuer: "https://issuer.example.com".to_string(),
            auth_url: None,
            token_url: None,
            userinfo_url: None,
            jwks_url: None,
            scopes: Vec::new(),
        });
        let result = casdoor_factory_from_private_config(
            &config,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        );
        assert!(
            matches!(result, Err(Error::InvalidInput(message)) if message.contains("Casdoor provider requires casdoor config"))
        );
    }
}
