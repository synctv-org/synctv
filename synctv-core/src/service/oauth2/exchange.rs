use synctv_common::ExecutionControl;
use tracing::debug;

use crate::{
    service::oauth2::{OAuth2Service, OAuth2State, OAuth2UserInfo},
    InternalExt, Result,
};

impl OAuth2Service {
    pub async fn exchange_code_for_user_info(
        &self,
        instance_name: &str,
        code: &str,
        pkce_verifier: &str,
    ) -> Result<OAuth2UserInfo> {
        self.exchange_code_for_user_info_with_control(instance_name, code, pkce_verifier, None)
            .await
    }

    pub async fn exchange_code_for_user_info_with_control(
        &self,
        instance_name: &str,
        code: &str,
        pkce_verifier: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<OAuth2UserInfo> {
        self.exchange_code_for_user_info_with_nonce_and_control(
            instance_name,
            code,
            None,
            pkce_verifier,
            None,
            control,
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
        self.exchange_code_for_user_info_with_nonce_and_control(
            instance_name,
            code,
            oauth_state.redirect_url.as_deref(),
            &oauth_state.pkce_verifier,
            oauth_state.nonce.as_deref(),
            control,
        )
        .await
    }

    async fn exchange_code_for_user_info_with_nonce_and_control(
        &self,
        instance_name: &str,
        code: &str,
        redirect_url: Option<&str>,
        pkce_verifier: &str,
        nonce: Option<&str>,
        control: Option<&ExecutionControl>,
    ) -> Result<OAuth2UserInfo> {
        let entry = self.provider_entry(instance_name).await?;
        let provider = entry.provider;
        let provider_type = entry.provider_type;

        debug!("Exchanging code for user info from {}", instance_name);

        let user_info = Self::run_with_control(control, async {
            provider
                .get_user_info(code, redirect_url, pkce_verifier, nonce)
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
