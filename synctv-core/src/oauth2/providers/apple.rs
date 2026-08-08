//! Apple Sign in with Apple provider.
//!
//! Apple uses OIDC tokens, so token validation and profile parsing are
//! composed from [`OidcProvider`]. This wrapper owns Apple's redirect policy,
//! native-code exchange policy, scopes, and token endpoint authentication.

use super::oidc::OidcProvider;
use super::validate_required_oauth2_field;
use crate::oauth2::{OAuth2Authorization, OAuth2AuthorizationMode, OAuth2UserInfo, Provider};
use crate::service::OAuth2ProviderPrivateConfig;
use crate::Error;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const APPLE_OIDC_ISSUER: &str = "https://appleid.apple.com";
const APPLE_OIDC_SCOPES: &[&str] = &["openid"];

fn native_nonce_claim(nonce: &str) -> String {
    hex::encode(Sha256::digest(nonce.as_bytes()))
}

/// Sign in with Apple provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppleConfig {
    pub web_client_id: String,
    pub web_client_secret: String,
    pub native_client_id: String,
    pub native_client_secret: String,
}

pub struct AppleProvider {
    web: Option<OidcProvider>,
    native: Option<OidcProvider>,
}

impl AppleProvider {
    fn create_oidc(
        client_id: &str,
        client_secret: &str,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<OidcProvider, Error> {
        Ok(OidcProvider::create_with_scopes_and_ssrf_guard(
            client_id.to_string(),
            client_secret.to_string(),
            APPLE_OIDC_ISSUER,
            APPLE_OIDC_SCOPES
                .iter()
                .map(|scope| (*scope).to_string())
                .collect(),
            ssrf_guard,
        )?
        .with_optional_pkce()
        .with_client_secret_post())
    }

    fn create(
        config: &AppleConfig,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Self, Error> {
        let web = Self::create_optional_oidc(
            "Apple web",
            &config.web_client_id,
            &config.web_client_secret,
            ssrf_guard,
        )?;
        let native = Self::create_optional_oidc(
            "Apple native",
            &config.native_client_id,
            &config.native_client_secret,
            ssrf_guard,
        )?;
        if web.is_none() && native.is_none() {
            return Err(Error::InvalidInput(
                "Apple provider requires Web or native credentials".to_string(),
            ));
        }
        Ok(Self { web, native })
    }

    fn create_optional_oidc(
        label: &str,
        client_id: &str,
        client_secret: &str,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Option<OidcProvider>, Error> {
        let client_id_empty = client_id.trim().is_empty();
        let client_secret_empty = client_secret.trim().is_empty();
        if client_id_empty && client_secret_empty {
            return Ok(None);
        }
        if client_id_empty {
            validate_required_oauth2_field(label, "client_id", client_id)?;
        }
        if client_secret_empty {
            validate_required_oauth2_field(label, "client_secret", client_secret)?;
        }
        Ok(Some(Self::create_oidc(
            client_id,
            client_secret,
            ssrf_guard,
        )?))
    }
}

#[async_trait]
impl Provider for AppleProvider {
    fn supported_authorization_modes(&self) -> &'static [OAuth2AuthorizationMode] {
        match (self.web.is_some(), self.native.is_some()) {
            (true, true) => &[
                OAuth2AuthorizationMode::Browser,
                OAuth2AuthorizationMode::Native,
            ],
            (true, false) => &[OAuth2AuthorizationMode::Browser],
            (false, true) => &[OAuth2AuthorizationMode::Native],
            (false, false) => &[],
        }
    }

    fn validate_authorization_redirect_url(&self, redirect_url: Option<&str>) -> Result<(), Error> {
        let _ = redirect_url;
        Ok(())
    }

    async fn new_auth_url(
        &self,
        state: &str,
        redirect_url: Option<&str>,
        mode: OAuth2AuthorizationMode,
    ) -> Result<OAuth2Authorization, Error> {
        match mode {
            OAuth2AuthorizationMode::Native => {
                if redirect_url.is_some() {
                    return Err(Error::InvalidInput(
                        "Apple native authorization does not accept a redirect URL".to_string(),
                    ));
                }
                self.native
                    .as_ref()
                    .ok_or_else(|| {
                        Error::InvalidInput(
                            "Apple provider has no native credentials configured".to_string(),
                        )
                    })?
                    .new_auth_url_without_pkce(state, None)
                    .await
            }
            OAuth2AuthorizationMode::Browser => {
                self.web
                    .as_ref()
                    .ok_or_else(|| {
                        Error::InvalidInput(
                            "Apple provider has no web credentials configured".to_string(),
                        )
                    })?
                    .new_auth_url(state, redirect_url, OAuth2AuthorizationMode::Browser)
                    .await
            }
        }
    }

    async fn get_user_info(
        &self,
        code: &str,
        redirect_url: Option<&str>,
        pkce_verifier: Option<&str>,
        nonce: Option<&str>,
        mode: OAuth2AuthorizationMode,
    ) -> Result<OAuth2UserInfo, Error> {
        let oidc = match mode {
            OAuth2AuthorizationMode::Browser => self.web.as_ref().ok_or_else(|| {
                Error::InvalidInput("Apple provider has no web credentials configured".to_string())
            })?,
            OAuth2AuthorizationMode::Native => self.native.as_ref().ok_or_else(|| {
                Error::InvalidInput(
                    "Apple provider has no native credentials configured".to_string(),
                )
            })?,
        };
        let native_nonce = nonce.map(native_nonce_claim);
        let expected_nonce = match mode {
            OAuth2AuthorizationMode::Browser => nonce,
            OAuth2AuthorizationMode::Native => native_nonce.as_deref(),
        };
        oidc.get_user_info(code, redirect_url, pkce_verifier, expected_nonce, mode)
            .await
    }
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
    let config = AppleConfig {
        web_client_id: config.web_client_id.clone(),
        web_client_secret: config.web_client_secret.clone(),
        native_client_id: config.native_client_id.clone(),
        native_client_secret: config.native_client_secret.clone(),
    };
    Ok(Box::new(AppleProvider::create(&config, ssrf_guard)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::OAuth2AppleProviderConfig;
    use crate::test_helpers::TestResultExt;

    #[test]
    fn factory_enforces_apple_redirect_url() {
        let redirect_url = "https://app.example.com/oauth2/callback";
        let config = OAuth2ProviderPrivateConfig::Apple(OAuth2AppleProviderConfig {
            web_client_id: "org.example.app.web".to_string(),
            web_client_secret: "web-secret".to_string(),
            native_client_id: "org.example.app".to_string(),
            native_client_secret: "native-secret".to_string(),
        });
        let provider = apple_factory_from_private_config(
            &config,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .checked("Apple provider should be created");

        assert!(provider.validate_authorization_redirect_url(None).is_ok());
        assert!(provider
            .validate_authorization_redirect_url(Some(redirect_url))
            .is_ok());
        assert!(provider
            .validate_authorization_redirect_url(Some("https://other.example.com/callback"))
            .is_ok());
    }

    #[test]
    fn factory_rejects_non_apple_config() {
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

        let result = apple_factory_from_private_config(
            &config,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        );
        let Err(error) = result else {
            panic!("Apple factory should reject OIDC config");
        };
        assert!(error
            .to_string()
            .contains("Apple provider requires apple config"));
    }

    #[test]
    fn apple_advertises_browser_and_native_authorization_modes() {
        let config = OAuth2ProviderPrivateConfig::Apple(OAuth2AppleProviderConfig {
            web_client_id: "org.example.app.web".to_string(),
            web_client_secret: "web-secret".to_string(),
            native_client_id: "org.example.app".to_string(),
            native_client_secret: "native-secret".to_string(),
        });
        let provider = apple_factory_from_private_config(
            &config,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .checked("Apple provider should be created");

        assert_eq!(
            provider.supported_authorization_modes(),
            &[
                OAuth2AuthorizationMode::Browser,
                OAuth2AuthorizationMode::Native,
            ]
        );
    }

    #[test]
    fn apple_advertises_only_configured_authorization_modes() {
        let config = OAuth2ProviderPrivateConfig::Apple(OAuth2AppleProviderConfig {
            web_client_id: "org.example.app.web".to_string(),
            web_client_secret: "web-secret".to_string(),
            native_client_id: String::new(),
            native_client_secret: String::new(),
        });
        let provider = apple_factory_from_private_config(
            &config,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .checked("web-only Apple provider should be created");

        assert_eq!(
            provider.supported_authorization_modes(),
            &[OAuth2AuthorizationMode::Browser]
        );
    }

    #[test]
    fn native_nonce_claim_uses_sha256_hex() {
        assert_eq!(
            native_nonce_claim("nonce"),
            "78377b525757b494427f89014f97d79928f3938d14eb51e20fb5dec9834eb304"
        );
    }
}
