use std::net::IpAddr;

use synctv_common::ExecutionControl;

use crate::{
    models::UserId,
    service::{
        user::{
            session_types::{AuthenticatedLogin, TokenIssueContext},
            UserService,
        },
        TokenAuthContext, TokenCredentialBinding,
    },
    Error, Result,
};

impl UserService {
    pub async fn login_oauth2(
        &self,
        user_id: &UserId,
        provider_instance_name: &str,
        provider_user_id: &str,
        client_ip: Option<IpAddr>,
    ) -> Result<AuthenticatedLogin> {
        self.login_oauth2_with_control(
            user_id,
            provider_instance_name,
            provider_user_id,
            client_ip,
            None,
        )
        .await
    }

    pub async fn login_oauth2_with_control(
        &self,
        user_id: &UserId,
        provider_instance_name: &str,
        provider_user_id: &str,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<AuthenticatedLogin> {
        let brute_force_key =
            Self::oauth2_login_brute_force_key(provider_instance_name, provider_user_id);
        self.brute_force
            .check_allowed_with_control(&brute_force_key, client_ip, control)
            .await?;

        let user = self
            .repository
            .get_by_id(user_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;

        if let Err(error) = Self::validate_user_access(&user) {
            if let Err(bf_err) = self
                .brute_force
                .record_failure_with_control(&brute_force_key, client_ip, control)
                .await
            {
                tracing::warn!(error = %bf_err, "Failed to record OAuth2 login failure for brute-force tracking");
                return Err(bf_err);
            }
            return Err(error);
        }

        let password_version = self
            .user_password_repository
            .get_state(&user.id)
            .await?
            .version;
        let credential_binding = TokenCredentialBinding::OAuth2 {
            provider_instance_name: provider_instance_name.to_string(),
            provider_user_id: provider_user_id.to_string(),
        };
        let (access_token, refresh_token) = self
            .issue_tokens_after_successful_authentication(
                &user,
                password_version,
                &brute_force_key,
                client_ip,
                TokenIssueContext {
                    auth_context: Some(TokenAuthContext::OAuth2),
                    credential_binding: &credential_binding,
                },
                control,
            )
            .await?;
        let email = self.user_email_repository.get_email(&user.id).await?;
        Ok(AuthenticatedLogin::Complete {
            user,
            email,
            access_token,
            refresh_token,
        })
    }

    fn oauth2_login_brute_force_key(
        provider_instance_name: &str,
        provider_user_id: &str,
    ) -> String {
        format!("oauth2:{provider_instance_name}:{provider_user_id}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth2_login_brute_force_key_scopes_provider_user_id_by_instance() {
        assert_eq!(
            UserService::oauth2_login_brute_force_key("github-main", "remote-user"),
            "oauth2:github-main:remote-user"
        );
        assert_ne!(
            UserService::oauth2_login_brute_force_key("github-main", "remote-user"),
            UserService::oauth2_login_brute_force_key("github-alt", "remote-user")
        );
    }
}
