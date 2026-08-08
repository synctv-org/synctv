//! Discord OAuth2 provider.

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
use std::sync::Arc;
use url::Url;

const DISCORD_AUTH_URL: &str = "https://discord.com/oauth2/authorize";
const DISCORD_TOKEN_URL: &str = "https://discord.com/api/oauth2/token";
const DISCORD_USERINFO_URL: &str = "https://discord.com/api/users/@me";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordConfig {
    pub client_id: String,
    pub client_secret: String,
}

pub struct DiscordProvider {
    config: DiscordConfig,
    http_client: Arc<Client>,
}

#[derive(Deserialize)]
struct DiscordTokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct DiscordUser {
    id: String,
    username: String,
    global_name: Option<String>,
    avatar: Option<String>,
}

impl DiscordProvider {
    fn create(
        config: DiscordConfig,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Self, Error> {
        Ok(Self {
            config,
            http_client: build_provider_http_client(ssrf_guard)?,
        })
    }
}

#[async_trait]
impl Provider for DiscordProvider {
    async fn new_auth_url(
        &self,
        state: &str,
        redirect_url: Option<&str>,
        mode: OAuth2AuthorizationMode,
    ) -> Result<OAuth2Authorization, Error> {
        if mode == OAuth2AuthorizationMode::Native {
            return Err(Error::InvalidInput(
                "Discord OAuth does not support native authorization".to_string(),
            ));
        }
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let redirect_url = require_oauth2_redirect_url(redirect_url, "Discord OAuth2")?;
        let mut url =
            Url::parse(DISCORD_AUTH_URL).internal_with_err("Invalid Discord authorization URL")?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", redirect_url)
            .append_pair("scope", "identify")
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
            .ok_or_else(|| Error::InvalidInput("Discord OAuth requires PKCE".to_string()))?;
        let redirect_url = require_oauth2_redirect_url(redirect_url, "Discord OAuth2")?;
        let token: DiscordTokenResponse = self
            .http_client
            .post(DISCORD_TOKEN_URL)
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
            .map_err(|err| map_provider_http_error("Failed to exchange Discord code", err))?
            .error_for_status()
            .internal_with_err("Discord token endpoint returned an error")?
            .json()
            .await
            .internal_with_err("Failed to parse Discord token response")?;

        let user: DiscordUser = self
            .http_client
            .get(DISCORD_USERINFO_URL)
            .bearer_auth(token.access_token)
            .send()
            .await
            .map_err(|err| map_provider_http_error("Failed to fetch Discord user info", err))?
            .error_for_status()
            .internal_with_err("Discord user info endpoint returned an error")?
            .json()
            .await
            .internal_with_err("Failed to parse Discord user info")?;

        let username = user
            .global_name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(user.username);
        let avatar = user.avatar.map(|hash| {
            let extension = if hash.starts_with("a_") { "gif" } else { "png" };
            format!(
                "https://cdn.discordapp.com/avatars/{}/{hash}.{extension}",
                user.id
            )
        });
        Ok(OAuth2UserInfo {
            provider_user_id: user.id,
            username,
            avatar,
        })
    }
}

pub fn discord_factory_from_private_config(
    config: &OAuth2ProviderPrivateConfig,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<Box<dyn Provider>, Error> {
    let OAuth2ProviderPrivateConfig::Discord(config) = config else {
        return Err(Error::InvalidInput(
            "Discord provider requires discord config".to_string(),
        ));
    };
    validate_required_oauth2_field("Discord", "client_id", &config.client_id)?;
    validate_required_oauth2_field("Discord", "client_secret", &config.client_secret)?;
    Ok(Box::new(DiscordProvider::create(
        DiscordConfig {
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
        },
        ssrf_guard,
    )?))
}
