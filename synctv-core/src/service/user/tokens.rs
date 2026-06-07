use synctv_common::ExecutionControl;

use crate::{
    models::UserId,
    repository::{UserOAuthProviderRepository, WebAuthnCredentialRepository},
    service::{
        auth::{TokenAuthContext, TokenCredentialBinding},
        user::UserService,
    },
    Error, Result,
};

use super::session_types::TWO_FACTOR_REQUIRED_MESSAGE;
use super::{nonnegative_i64_to_u64, password_binding};

impl UserService {
    /// Refresh token with rotation and replay protection.
    ///
    /// Each refresh token can only be used once:
    /// 1. The old refresh token's JTI is checked against the Redis blacklist.
    /// 2. If the JTI is blacklisted, the request is rejected (possible token theft replay).
    ///    Additionally, the refresh-token family for that login session is revoked as a
    ///    precaution (all same-session refresh tokens issued before this moment become invalid).
    /// 3. After issuing new tokens, the old JTI is added to the blacklist with a TTL
    ///    equal to the old token's remaining lifetime.
    pub async fn refresh_token(&self, refresh_token: String) -> Result<(String, String)> {
        self.refresh_token_with_control(refresh_token, None).await
    }

    pub async fn refresh_token_with_control(
        &self,
        refresh_token: String,
        control: Option<&ExecutionControl>,
    ) -> Result<(String, String)> {
        let claims = self.jwt_service.verify_refresh_token(&refresh_token)?;
        let user_id: UserId = claims.sub.parse().map_err(crate::Error::Internal)?;

        let rate_limit_key = format!("refresh:{user_id}");
        self.refresh_rate_limiter
            .check_rate_limit_with_control(
                &rate_limit_key,
                self.refresh_rate_limit_config.requests,
                self.refresh_rate_limit_config.window_secs,
                control,
            )
            .await
            .map_err(|error| {
                tracing::warn!(
                    user_id = %user_id,
                    error = %error,
                    "Refresh token rate limit exceeded"
                );
                Error::from(error)
            })?;

        let user = self
            .repository
            .get_by_id(&user_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;

        Self::validate_user_access(&user)?;
        if self
            .user_preferences_repository
            .get_or_default(&user.id)
            .await?
            .two_factor_enabled
        {
            let refresh_auth_context = claims.amr.as_deref();
            if !matches!(refresh_auth_context, Some("local_2fa" | "oauth2")) {
                return Err(Error::Authentication(
                    TWO_FACTOR_REQUIRED_MESSAGE.to_string(),
                ));
            }
        }

        let password_version = self
            .user_password_repository
            .get_state(&user.id)
            .await?
            .version;
        let credential_binding = self
            .validate_refresh_credential_binding(&claims, &user.id, password_version)
            .await?;
        let session_id = claims
            .sid
            .as_deref()
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;

        {
            let old_jti = &claims.jti;

            let family_key = self.refresh_token_family_key(&user_id, session_id);
            let family_revoked_at = self
                .token_blacklist
                .get_family_revoked_at_checked(&family_key)
                .await;
            if let Some(revoked_at) = family_revoked_at? {
                if claims.iat <= revoked_at {
                    tracing::warn!(
                        user_id = %user_id,
                        jti = %old_jti,
                        revoked_at = revoked_at,
                        token_iat = claims.iat,
                        "Refresh token rejected: token family revoked (possible token theft)"
                    );
                    return Err(Error::Authentication("Authentication failed".to_string()));
                }
            }

            if !old_jti.is_empty() {
                let blacklist_key = self.key_builder.refresh_token_blacklist(old_jti);
                let now = chrono::Utc::now().timestamp();
                let remaining_ttl = nonnegative_i64_to_u64((claims.exp - now).max(60));

                let already_existed = self
                    .token_blacklist
                    .blacklist_if_not_exists(&blacklist_key, remaining_ttl)
                    .await?;

                if already_existed {
                    tracing::warn!(
                        user_id = %user_id,
                        jti = %old_jti,
                        "Blacklisted refresh token JTI replayed - revoking token session"
                    );

                    let family_ttl = self
                        .jwt_service
                        .refresh_token_duration_seconds()
                        .saturating_add(3600);
                    self.token_blacklist
                        .set_family_revoked(&family_key, now, family_ttl)
                        .await?;

                    return Err(Error::Authentication("Authentication failed".to_string()));
                }
            }
        }

        let token_auth_context = match claims.amr.as_deref() {
            Some("local_2fa") => Some(TokenAuthContext::LocalTwoFactor),
            Some("oauth2") => Some(TokenAuthContext::OAuth2),
            _ => None,
        };
        let new_access_token = self
            .jwt_service
            .sign_access_token_with_auth_context_and_session(
                &user.id,
                password_version,
                token_auth_context,
                Some(session_id),
                &credential_binding,
            )?;
        let new_refresh_token = self.jwt_service.sign_refresh_token_with_session(
            &user.id,
            password_version,
            token_auth_context,
            session_id,
            &credential_binding,
        )?;

        Ok((new_access_token, new_refresh_token))
    }

    async fn validate_refresh_credential_binding(
        &self,
        claims: &crate::service::auth::Claims,
        user_id: &UserId,
        current_password_version: i32,
    ) -> Result<TokenCredentialBinding> {
        let credential_binding = claims.credential_binding()?;
        match credential_binding {
            TokenCredentialBinding::Password { version } => {
                if version != current_password_version {
                    return Err(Error::Authentication("Authentication failed".to_string()));
                }
                Ok(password_binding(current_password_version))
            }
            TokenCredentialBinding::OAuth2 {
                provider_instance_name,
                provider_user_id,
            } => {
                let mapping = UserOAuthProviderRepository::new(self.repository.pool().clone())
                    .find_by_provider_instance(&provider_instance_name, &provider_user_id)
                    .await?;
                if mapping
                    .as_ref()
                    .is_none_or(|mapping| mapping.user_id != *user_id)
                {
                    return Err(Error::Authentication("Authentication failed".to_string()));
                }
                Ok(TokenCredentialBinding::OAuth2 {
                    provider_instance_name,
                    provider_user_id,
                })
            }
            TokenCredentialBinding::WebAuthn { credential_id } => {
                let credential = WebAuthnCredentialRepository::new(self.repository.pool().clone())
                    .get_by_credential_id(&credential_id)
                    .await?;
                if credential
                    .as_ref()
                    .is_none_or(|credential| credential.user_id != *user_id)
                {
                    return Err(Error::Authentication("Authentication failed".to_string()));
                }
                Ok(TokenCredentialBinding::WebAuthn { credential_id })
            }
            TokenCredentialBinding::Email { email } => {
                let current_email = self.user_email_repository.get_email(user_id).await?;
                if current_email.as_deref() != Some(email.as_str()) {
                    return Err(Error::Authentication("Authentication failed".to_string()));
                }
                Ok(TokenCredentialBinding::Email { email })
            }
        }
    }

    pub async fn blacklist_access_token(&self, jti: &str, ttl_secs: u64) -> Result<()> {
        let key = self.key_builder.access_token_blacklist(jti);
        self.token_blacklist.blacklist(&key, ttl_secs).await
    }

    pub async fn revoke_refresh_token_session(
        &self,
        user_id: &UserId,
        session_id: &str,
        revoked_at: i64,
    ) -> Result<()> {
        let key = self.refresh_token_family_key(user_id, session_id);
        let family_ttl = self
            .jwt_service
            .refresh_token_duration_seconds()
            .saturating_add(3600);
        self.token_blacklist
            .set_family_revoked(&key, revoked_at, family_ttl)
            .await
    }

    fn refresh_token_family_key(&self, user_id: &UserId, session_id: &str) -> String {
        self.key_builder
            .refresh_token_session_revoked(&user_id.to_string(), session_id)
    }
}
