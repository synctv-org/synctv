//! Feishu OAuth2 provider.

use super::{
    build_provider_http_client, map_provider_http_error, require_oauth2_redirect_url,
    validate_provider_url, validate_required_oauth2_field,
};
use crate::oauth2::{OAuth2Authorization, OAuth2AuthorizationMode, OAuth2UserInfo, Provider};
use crate::service::OAuth2ProviderPrivateConfig;
use crate::{Error, InternalExt};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use url::Url;

const DEFAULT_FEISHU_ENDPOINT: &str = "https://open.feishu.cn";
const FEISHU_SCOPE: &str = "contact:user.base:readonly";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeishuConfig {
    pub client_id: String,
    pub client_secret: String,
    pub endpoint: String,
}

pub struct FeishuProvider {
    config: FeishuConfig,
    endpoint: String,
    http_client: Arc<Client>,
}

#[derive(Deserialize)]
struct FeishuTokenResponse {
    code: Option<i32>,
    msg: Option<String>,
    access_token: Option<String>,
}

#[derive(Deserialize)]
struct FeishuUserInfoResponse {
    code: Option<i32>,
    msg: Option<String>,
    data: Option<FeishuUserInfo>,
}

#[derive(Deserialize)]
struct FeishuUserInfo {
    open_id: Option<String>,
    union_id: Option<String>,
    user_id: Option<String>,
    name: Option<String>,
    avatar_url: Option<String>,
}

impl FeishuProvider {
    fn create(
        config: FeishuConfig,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Self, Error> {
        let endpoint = if config.endpoint.trim().is_empty() {
            DEFAULT_FEISHU_ENDPOINT.to_string()
        } else {
            config.endpoint.trim_end_matches('/').to_string()
        };
        validate_provider_url(&endpoint, "Invalid Feishu endpoint", ssrf_guard)?;
        for suffix in [
            "/open-apis/authen/v1/authorize",
            "/open-apis/authen/v1/access_token",
            "/open-apis/authen/v1/user_info",
        ] {
            validate_provider_url(
                &format!("{endpoint}{suffix}"),
                "Invalid Feishu endpoint URL",
                ssrf_guard,
            )?;
        }
        Ok(Self {
            config,
            endpoint,
            http_client: build_provider_http_client(ssrf_guard)?,
        })
    }

    fn authorization_url(&self, state: &str, redirect_url: &str) -> Result<String, Error> {
        let mut url = Url::parse(&format!("{}/open-apis/authen/v1/authorize", self.endpoint))
            .internal_with_err("Invalid Feishu authorization URL")?;
        url.query_pairs_mut()
            .append_pair("app_id", &self.config.client_id)
            .append_pair("redirect_uri", redirect_url)
            .append_pair("state", state)
            .append_pair("scope", FEISHU_SCOPE);
        Ok(url.to_string())
    }
}

#[async_trait]
impl Provider for FeishuProvider {
    async fn new_auth_url(
        &self,
        state: &str,
        redirect_url: Option<&str>,
        mode: OAuth2AuthorizationMode,
    ) -> Result<OAuth2Authorization, Error> {
        if mode == OAuth2AuthorizationMode::Native {
            return Err(Error::InvalidInput(
                "Feishu OAuth does not support native authorization".to_string(),
            ));
        }
        let redirect_url = require_oauth2_redirect_url(redirect_url, "Feishu OAuth2")?;
        Ok(OAuth2Authorization::without_pkce(
            self.authorization_url(state, redirect_url)?,
        ))
    }

    async fn get_user_info(
        &self,
        code: &str,
        redirect_url: Option<&str>,
        _pkce_verifier: Option<&str>,
        _nonce: Option<&str>,
        _mode: OAuth2AuthorizationMode,
    ) -> Result<OAuth2UserInfo, Error> {
        let _ = redirect_url;
        let response: FeishuTokenResponse = self
            .http_client
            .post(format!(
                "{}/open-apis/authen/v1/access_token",
                self.endpoint
            ))
            .json(&serde_json::json!({
                "grant_type": "authorization_code",
                "code": code,
                "app_id": self.config.client_id,
                "app_secret": self.config.client_secret,
            }))
            .send()
            .await
            .map_err(|err| map_provider_http_error("Failed to exchange Feishu code", err))?
            .error_for_status()
            .internal_with_err("Feishu token endpoint returned an error")?
            .json()
            .await
            .internal_with_err("Failed to parse Feishu token response")?;
        if response.code.unwrap_or_default() != 0 {
            return Err(Error::Authentication(format!(
                "Feishu token exchange failed: {}",
                response.msg.unwrap_or_else(|| "unknown error".to_string())
            )));
        }
        let access_token = response.access_token.ok_or_else(|| {
            Error::Authentication("Feishu token response is missing access_token".to_string())
        })?;
        let response: FeishuUserInfoResponse = self
            .http_client
            .get(format!("{}/open-apis/authen/v1/user_info", self.endpoint))
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|err| map_provider_http_error("Failed to fetch Feishu user info", err))?
            .error_for_status()
            .internal_with_err("Feishu user info endpoint returned an error")?
            .json()
            .await
            .internal_with_err("Failed to parse Feishu user info")?;
        if response.code.unwrap_or_default() != 0 {
            return Err(Error::Authentication(format!(
                "Feishu user info request failed: {}",
                response.msg.unwrap_or_else(|| "unknown error".to_string())
            )));
        }
        let user = response.data.ok_or_else(|| {
            Error::Authentication("Feishu user info response is missing data".to_string())
        })?;
        let provider_user_id =
            user.open_id
                .or(user.union_id)
                .or(user.user_id)
                .ok_or_else(|| {
                    Error::Authentication("Feishu user info is missing user identity".to_string())
                })?;
        let username = user
            .name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| provider_user_id.clone());
        Ok(OAuth2UserInfo {
            provider_user_id,
            username,
            avatar: user.avatar_url,
        })
    }
}

pub fn feishu_factory_from_private_config(
    config: &OAuth2ProviderPrivateConfig,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    let OAuth2ProviderPrivateConfig::Feishu(config) = config else {
        return Err(Error::InvalidInput(
            "Feishu provider requires feishu config".to_string(),
        ));
    };
    validate_required_oauth2_field("Feishu", "client_id", &config.client_id)?;
    validate_required_oauth2_field("Feishu", "client_secret", &config.client_secret)?;
    Ok(Box::new(FeishuProvider::create(
        FeishuConfig {
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
            endpoint: config.endpoint.clone(),
        },
        ssrf_guard,
    )?))
}
