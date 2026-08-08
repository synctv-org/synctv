//! Gitee OAuth2 provider.

use super::{
    build_provider_http_client, map_provider_http_error, require_oauth2_redirect_url,
    validate_required_oauth2_field,
};
use crate::oauth2::{OAuth2Authorization, OAuth2AuthorizationMode, OAuth2UserInfo, Provider};
use crate::service::OAuth2ProviderPrivateConfig;
use crate::{Error, InternalExt};
use async_trait::async_trait;
use oauth2::PkceCodeChallenge;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use url::Url;

const GITEE_AUTH_URL: &str = "https://gitee.com/oauth/authorize";
const GITEE_TOKEN_URL: &str = "https://gitee.com/oauth/token";
const GITEE_USERINFO_URL: &str = "https://gitee.com/api/v5/user";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GiteeConfig {
    pub client_id: String,
    pub client_secret: String,
}

pub struct GiteeProvider {
    config: GiteeConfig,
    http_client: Arc<Client>,
}

#[derive(Deserialize)]
struct GiteeTokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct GiteeUser {
    id: Value,
    login: Option<String>,
    name: Option<String>,
    avatar_url: Option<String>,
}

impl GiteeProvider {
    fn create(
        config: GiteeConfig,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Self, Error> {
        Ok(Self {
            config,
            http_client: build_provider_http_client(ssrf_guard)?,
        })
    }
}

#[async_trait]
impl Provider for GiteeProvider {
    async fn new_auth_url(
        &self,
        state: &str,
        redirect_url: Option<&str>,
        mode: OAuth2AuthorizationMode,
    ) -> Result<OAuth2Authorization, Error> {
        if mode == OAuth2AuthorizationMode::Native {
            return Err(Error::InvalidInput(
                "Gitee OAuth does not support native authorization".to_string(),
            ));
        }
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let redirect_url = require_oauth2_redirect_url(redirect_url, "Gitee OAuth2")?;
        let mut url =
            Url::parse(GITEE_AUTH_URL).internal_with_err("Invalid Gitee authorization URL")?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", redirect_url)
            .append_pair("scope", "user_info")
            .append_pair("state", state)
            .append_pair("code_challenge", challenge.as_str())
            .append_pair("code_challenge_method", "S256");
        Ok(OAuth2Authorization::new(
            url.to_string(),
            verifier.secret().clone(),
        ))
    }

    async fn get_user_info(
        &self,
        code: &str,
        redirect_url: Option<&str>,
        pkce_verifier: Option<&str>,
        _nonce: Option<&str>,
        _mode: OAuth2AuthorizationMode,
    ) -> Result<OAuth2UserInfo, Error> {
        let verifier = pkce_verifier
            .ok_or_else(|| Error::InvalidInput("Gitee OAuth requires PKCE".to_string()))?;
        let redirect_url = require_oauth2_redirect_url(redirect_url, "Gitee OAuth2")?;
        let token: GiteeTokenResponse = self
            .http_client
            .post(GITEE_TOKEN_URL)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", self.config.client_id.as_str()),
                ("client_secret", self.config.client_secret.as_str()),
                ("code", code),
                ("redirect_uri", redirect_url),
                ("code_verifier", verifier),
            ])
            .send()
            .await
            .map_err(|err| map_provider_http_error("Failed to exchange Gitee code", err))?
            .error_for_status()
            .internal_with_err("Gitee token endpoint returned an error")?
            .json()
            .await
            .internal_with_err("Failed to parse Gitee token response")?;

        let user: GiteeUser = self
            .http_client
            .get(GITEE_USERINFO_URL)
            .bearer_auth(token.access_token)
            .send()
            .await
            .map_err(|err| map_provider_http_error("Failed to fetch Gitee user info", err))?
            .error_for_status()
            .internal_with_err("Gitee user info endpoint returned an error")?
            .json()
            .await
            .internal_with_err("Failed to parse Gitee user info")?;

        let provider_user_id = match user.id {
            Value::String(value) => value,
            value => value.to_string(),
        };
        let username = user
            .login
            .or(user.name)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| provider_user_id.clone());
        Ok(OAuth2UserInfo {
            provider_user_id,
            username,
            avatar: user.avatar_url,
        })
    }
}

pub fn gitee_factory_from_private_config(
    config: &OAuth2ProviderPrivateConfig,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    let OAuth2ProviderPrivateConfig::Gitee(config) = config else {
        return Err(Error::InvalidInput(
            "Gitee provider requires gitee config".to_string(),
        ));
    };
    validate_required_oauth2_field("Gitee", "client_id", &config.client_id)?;
    validate_required_oauth2_field("Gitee", "client_secret", &config.client_secret)?;
    Ok(Box::new(GiteeProvider::create(
        GiteeConfig {
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
        },
        ssrf_guard,
    )?))
}
