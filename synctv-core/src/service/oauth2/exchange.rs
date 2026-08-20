use synctv_common::ExecutionControl;
use tracing::debug;

use crate::{
    oauth2::OAuth2AuthorizationMode,
    service::oauth2::{OAuth2Service, OAuth2State, OAuth2UserInfo},
    InternalExt, Result,
};

struct OAuth2ExchangeContext<'a> {
    redirect_url: Option<&'a str>,
    pkce_verifier: Option<&'a str>,
    nonce: Option<&'a str>,
    mode: OAuth2AuthorizationMode,
    control: Option<&'a ExecutionControl>,
}

impl OAuth2Service {
    #[cfg(test)]
    pub(crate) async fn exchange_code_for_user_info(
        &self,
        instance_name: &str,
        code: &str,
        pkce_verifier: &str,
    ) -> Result<OAuth2UserInfo> {
        self.exchange_code_for_user_info_with_control(instance_name, code, pkce_verifier, None)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn exchange_code_for_user_info_with_control(
        &self,
        instance_name: &str,
        code: &str,
        pkce_verifier: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<OAuth2UserInfo> {
        self.exchange_code_for_user_info_with_context(
            instance_name,
            code,
            OAuth2ExchangeContext {
                redirect_url: None,
                pkce_verifier: Some(pkce_verifier),
                nonce: None,
                mode: OAuth2AuthorizationMode::Browser,
                control,
            },
        )
        .await
    }

    pub async fn exchange_code_for_user_info_with_state_and_control(
        &self,
        instance_name: &str,
        code: &str,
        oauth_state: &OAuth2State,
        control: Option<&ExecutionControl>,
    ) -> Result<OAuth2UserInfo> {
        let pkce_verifier =
            (!oauth_state.pkce_verifier.is_empty()).then_some(oauth_state.pkce_verifier.as_str());
        self.exchange_code_for_user_info_with_context(
            instance_name,
            code,
            OAuth2ExchangeContext {
                redirect_url: oauth_state.redirect_url.as_deref(),
                pkce_verifier,
                nonce: oauth_state.nonce.as_deref(),
                mode: oauth_state.authorization_mode,
                control,
            },
        )
        .await
    }

    async fn exchange_code_for_user_info_with_context(
        &self,
        instance_name: &str,
        code: &str,
        context: OAuth2ExchangeContext<'_>,
    ) -> Result<OAuth2UserInfo> {
        let entry = self.provider_entry(instance_name).await?;
        let provider = entry.provider;
        let provider_type = entry.provider_type;

        debug!("Exchanging code for user info from {}", instance_name);

        let user_info = Self::run_with_control(context.control, async {
            provider
                .get_user_info(
                    code,
                    context.redirect_url,
                    context.pkce_verifier,
                    context.nonce,
                    context.mode,
                )
                .await
                .internal_with_err("Failed to get user info")
        })
        .await?;

        Ok(OAuth2UserInfo {
            provider: provider_type,
            provider_instance_name: instance_name.to_string(),
            provider_issuer: None,
            provider_user_id: user_info.provider_user_id,
            username: user_info.username,
            avatar: user_info.avatar,
        })
    }
}
