use crate::{
    models::{oauth2_client::OAuth2Provider, UserId},
    service::{oauth2::OAuth2Service, oauth2::OAuth2UserInfo, OAuth2SignupPolicy},
    Result,
};

impl OAuth2Service {
    pub async fn upsert_user_provider(
        &self,
        user_id: &UserId,
        user_info: &OAuth2UserInfo,
    ) -> Result<()> {
        let repo_user_info = user_info.to_repo_user_info();

        self.repository()?
            .upsert(
                user_id,
                &user_info.provider,
                &user_info.provider_instance_name,
                &user_info.provider_user_id,
                &repo_user_info,
            )
            .await
    }

    pub async fn upsert_user_provider_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        user_info: &OAuth2UserInfo,
        executor: E,
    ) -> Result<()>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let repo_user_info = user_info.to_repo_user_info();

        self.repository()?
            .upsert_with_executor(
                user_id,
                &user_info.provider,
                &user_info.provider_instance_name,
                &user_info.provider_user_id,
                &repo_user_info,
                executor,
            )
            .await
    }

    pub async fn find_user_by_provider_instance(
        &self,
        provider_instance_name: &str,
        provider_user_id: &str,
    ) -> Result<Option<UserId>> {
        match self
            .repository()?
            .find_by_provider_instance(provider_instance_name, provider_user_id)
            .await?
        {
            Some(mapping) => Ok(Some(mapping.user_id)),
            None => Ok(None),
        }
    }

    pub async fn get_user_providers(&self, user_id: &UserId) -> Result<Vec<OAuth2Provider>> {
        let mappings = self.repository()?.find_by_user(user_id).await?;
        Ok(mappings.into_iter().map(|m| m.provider).collect())
    }

    pub async fn get_user_provider_mappings(
        &self,
        user_id: &UserId,
    ) -> Result<Vec<crate::models::oauth2_client::UserOAuthProviderMapping>> {
        self.repository()?.find_by_user(user_id).await
    }

    pub async fn list_available_instances(
        &self,
    ) -> Result<
        Vec<(
            String,
            OAuth2Provider,
            OAuth2SignupPolicy,
            Vec<crate::oauth2::OAuth2AuthorizationMode>,
        )>,
    > {
        self.sync_runtime_providers().await?;
        let providers = self.providers.read().await;
        Ok(providers
            .iter()
            .map(|(name, entry)| {
                (
                    name.clone(),
                    entry.provider_type.clone(),
                    entry.signup_policy.clone(),
                    entry.provider.supported_authorization_modes().to_vec(),
                )
            })
            .collect())
    }

    pub async fn unlink_provider(
        &self,
        user_id: &UserId,
        provider_instance_name: &str,
        provider_user_id: &str,
    ) -> Result<bool> {
        self.repository()?
            .delete_instance(user_id, provider_instance_name, provider_user_id)
            .await
    }

    pub async fn unlink_provider_all(
        &self,
        user_id: &UserId,
        provider: &OAuth2Provider,
    ) -> Result<bool> {
        self.repository()?
            .delete_by_user_and_provider(user_id, provider)
            .await
    }

    pub async fn delete_all_for_user(&self, user_id: &UserId) -> Result<u64> {
        self.repository()?.delete_all_for_user(user_id).await
    }
}
