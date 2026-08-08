//! QQ OAuth2 provider.

use super::{
    build_provider_http_client, map_provider_http_error, require_oauth2_redirect_url,
    validate_required_oauth2_field,
};
use crate::oauth2::{OAuth2Authorization, OAuth2AuthorizationMode, OAuth2UserInfo, Provider};
use crate::service::OAuth2ProviderPrivateConfig;
use crate::{Error, InternalExt};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use url::Url;

const QQ_AUTH_URL: &str = "https://graph.qq.com/oauth2.0/authorize";
const QQ_TOKEN_URL: &str = "https://graph.qq.com/oauth2.0/token";
const QQ_OPENID_URL: &str = "https://graph.qq.com/oauth2.0/me";
const QQ_USERINFO_URL: &str = "https://graph.qq.com/user/get_user_info";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QqConfig {
    pub client_id: String,
    pub client_secret: String,
}

pub struct QqProvider {
    config: QqConfig,
    http_client: Arc<Client>,
}

#[derive(Deserialize)]
struct QqTokenJson {
    access_token: String,
}

#[derive(Deserialize)]
struct QqOpenId {
    openid: String,
}

#[derive(Deserialize)]
struct QqUserInfo {
    ret: i32,
    msg: Option<String>,
    nickname: Option<String>,
    figureurl_qq_2: Option<String>,
}

impl QqProvider {
    fn create(
        config: QqConfig,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Self, Error> {
        Ok(Self {
            config,
            http_client: build_provider_http_client(ssrf_guard)?,
        })
    }

    fn authorization_url(&self, state: &str, redirect_url: &str) -> Result<String, Error> {
        let mut url = Url::parse(QQ_AUTH_URL).internal_with_err("Invalid QQ authorization URL")?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", redirect_url)
            .append_pair("scope", "get_user_info")
            .append_pair("state", state);
        Ok(url.to_string())
    }
}

#[async_trait]
impl Provider for QqProvider {
    async fn new_auth_url(
        &self,
        state: &str,
        redirect_url: Option<&str>,
        mode: OAuth2AuthorizationMode,
    ) -> Result<OAuth2Authorization, Error> {
        if mode == OAuth2AuthorizationMode::Native {
            return Err(Error::InvalidInput(
                "QQ OAuth does not support native authorization".to_string(),
            ));
        }
        let redirect_url = require_oauth2_redirect_url(redirect_url, "QQ OAuth2")?;
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
        let redirect_url = require_oauth2_redirect_url(redirect_url, "QQ OAuth2")?;
        let response = self
            .http_client
            .get(QQ_TOKEN_URL)
            .query(&[
                ("grant_type", "authorization_code"),
                ("client_id", self.config.client_id.as_str()),
                ("client_secret", self.config.client_secret.as_str()),
                ("code", code),
                ("redirect_uri", redirect_url),
            ])
            .send()
            .await
            .map_err(|err| map_provider_http_error("Failed to exchange QQ code", err))?
            .error_for_status()
            .internal_with_err("QQ token endpoint returned an error")?;
        let body = response
            .text()
            .await
            .internal_with_err("Failed to read QQ token response")?;
        let access_token = if body.trim_start().starts_with('{') {
            serde_json::from_str::<QqTokenJson>(&body)
                .internal_with_err("Failed to parse QQ token response")?
                .access_token
        } else {
            let params: HashMap<_, _> = url::form_urlencoded::parse(body.as_bytes())
                .into_owned()
                .collect();
            params.get("access_token").cloned().ok_or_else(|| {
                Error::Authentication("QQ token response is missing access_token".to_string())
            })?
        };

        let openid_body = self
            .http_client
            .get(QQ_OPENID_URL)
            .query(&[("access_token", access_token.as_str())])
            .send()
            .await
            .map_err(|err| map_provider_http_error("Failed to fetch QQ identity", err))?
            .error_for_status()
            .internal_with_err("QQ identity endpoint returned an error")?
            .text()
            .await
            .internal_with_err("Failed to read QQ identity response")?;
        let json_start = openid_body.find('{').ok_or_else(|| {
            Error::Authentication("QQ identity response is not valid JSONP".to_string())
        })?;
        let json_end = openid_body.rfind('}').ok_or_else(|| {
            Error::Authentication("QQ identity response is not valid JSONP".to_string())
        })?;
        let openid: QqOpenId = serde_json::from_str(&openid_body[json_start..=json_end])
            .internal_with_err("Failed to parse QQ identity response")?;

        let user: QqUserInfo = self
            .http_client
            .get(QQ_USERINFO_URL)
            .query(&[
                ("access_token", access_token.as_str()),
                ("oauth_consumer_key", self.config.client_id.as_str()),
                ("openid", openid.openid.as_str()),
            ])
            .send()
            .await
            .map_err(|err| map_provider_http_error("Failed to fetch QQ user info", err))?
            .error_for_status()
            .internal_with_err("QQ user info endpoint returned an error")?
            .json()
            .await
            .internal_with_err("Failed to parse QQ user info")?;
        if user.ret != 0 {
            return Err(Error::Authentication(format!(
                "QQ user info request failed: {}",
                user.msg.unwrap_or_else(|| "unknown error".to_string())
            )));
        }
        let username = user
            .nickname
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| openid.openid.clone());
        Ok(OAuth2UserInfo {
            provider_user_id: openid.openid,
            username,
            avatar: user.figureurl_qq_2,
        })
    }
}

pub fn qq_factory_from_private_config(
    config: &OAuth2ProviderPrivateConfig,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    let OAuth2ProviderPrivateConfig::Qq(config) = config else {
        return Err(Error::InvalidInput(
            "QQ provider requires qq config".to_string(),
        ));
    };
    validate_required_oauth2_field("QQ", "client_id", &config.client_id)?;
    validate_required_oauth2_field("QQ", "client_secret", &config.client_secret)?;
    Ok(Box::new(QqProvider::create(
        QqConfig {
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
        },
        ssrf_guard,
    )?))
}
