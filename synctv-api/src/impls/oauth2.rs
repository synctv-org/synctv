//! `OAuth2` API Implementation
//!
//! Unified implementation for `OAuth2` API operations.
//! Used by both HTTP and gRPC handlers.
//!
//! ## Frontend-Driven `OAuth2` Flow
//!
//! This implementation uses a frontend-driven `OAuth2` flow where the frontend
//! handles the redirect from the `OAuth2` provider and extracts the authorization
//! code and state before sending them to the backend.
//!
//! Flow:
//! 1. Frontend calls `get_authorization_url` to get the auth URL
//! 2. Frontend redirects user to the auth URL (`OAuth2` provider)
//! 3. User authorizes on the provider
//! 4. Provider redirects to **frontend URL** with code and state
//! 5. Frontend extracts code and state from URL parameters
//! 6. Frontend calls `exchange_authorization_code` with code and state
//! 7. Backend validates state, exchanges code for user info, creates/logs in user
//! 8. Backend returns JWT token to frontend

use std::sync::Arc;
use synctv_core::models::{User, UserId, UserRole, UserStatus};
use synctv_core::service::{OAuth2Service, UserService};
use synctv_proto::client::{LinkedProvider, OAuth2ProviderInstance, OAuth2UserInfo};

use super::ApiError;

/// `OAuth2` API implementation
#[derive(Clone)]
pub struct OAuth2ApiImpl {
    pub oauth2_service: Arc<OAuth2Service>,
    pub user_service: Arc<UserService>,
}

impl OAuth2ApiImpl {
    fn map_bind_user_lookup_error(err: synctv_core::Error) -> ApiError {
        match err {
            synctv_core::Error::NotFound(_) => {
                ApiError::Authentication("Authentication failed".to_string())
            }
            other => ApiError::from(other),
        }
    }

    #[must_use]
    pub const fn new(oauth2_service: Arc<OAuth2Service>, user_service: Arc<UserService>) -> Self {
        Self {
            oauth2_service,
            user_service,
        }
    }

    /// Get authorization URL for `OAuth2` login flow
    ///
    /// Returns the URL to redirect the user to for authorization.
    /// The frontend should redirect the browser to this URL.
    pub async fn get_authorization_url(
        &self,
        provider: &str,
        redirect_url: Option<String>,
    ) -> Result<(String, String), ApiError> {
        let (auth_url, state) = self
            .oauth2_service
            .get_authorization_url(provider, redirect_url)
            .await
            .map_err(ApiError::from)?;

        Ok((auth_url, state))
    }

    /// Get authorization URL for binding `OAuth2` provider to existing user
    ///
    /// Requires authentication. The `user_id` should come from the JWT token.
    ///
    /// Security: Returns a generic "Authentication failed" error for both
    /// non-existent users and disabled accounts to prevent user enumeration attacks.
    pub async fn get_authorization_url_for_bind(
        &self,
        user_id: &UserId,
        provider: &str,
        redirect_url: Option<String>,
    ) -> Result<(String, String), ApiError> {
        // Verify user exists and is not banned/deleted
        // Use generic error message to prevent user enumeration attacks
        let user = self
            .user_service
            .get_user(user_id)
            .await
            .map_err(Self::map_bind_user_lookup_error)?;

        if user.is_deleted() || user.status == UserStatus::Banned {
            // Use the same generic error to prevent distinguishing between
            // "user not found" and "user is disabled"
            return Err(ApiError::Authentication(
                "Authentication failed".to_string(),
            ));
        }

        let (auth_url, state) = self
            .oauth2_service
            .get_authorization_url_with_user(provider, redirect_url, Some(user_id.clone()))
            .await
            .map_err(ApiError::from)?;

        Ok((auth_url, state))
    }

    /// Exchange authorization code for JWT token
    ///
    /// Frontend calls this after receiving code and state from `OAuth2` provider redirect.
    ///
    /// For login flow (no `bind_user_id` in state):
    /// - If user exists: log them in
    /// - If user doesn't exist: create new user account
    ///
    /// For bind flow (`bind_user_id` present in state):
    /// - Binds the `OAuth2` provider to the existing user account
    /// - Returns empty tokens (user is already logged in)
    ///
    /// The `current_user_id` parameter is required for the bind flow to verify that
    /// only the intended user (the one who initiated the bind) can complete it.
    /// Pass `None` for login-only flows (no authentication needed).
    pub async fn exchange_authorization_code(
        &self,
        provider: &str,
        code: &str,
        state: &str,
        current_user_id: Option<&UserId>,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<ExchangeCodeResult, ApiError> {
        // 1. Verify state and retrieve stored OAuth2 state
        let oauth_state = self
            .oauth2_service
            .verify_state(state)
            .await
            .map_err(ApiError::from)?;

        // Verify provider matches
        if oauth_state.instance_name != provider {
            return Err(ApiError::InvalidInput(
                "Provider mismatch between request and stored state".to_string(),
            ));
        }

        // 2. Exchange code for user info using PKCE verifier from stored state
        let (user_info, provider_type) = self
            .oauth2_service
            .exchange_code_for_user_info(provider, code, &oauth_state.pkce_verifier)
            .await
            .map_err(ApiError::from)?;

        // 3. Handle bind flow vs login flow
        if let Some(bind_user_id) = oauth_state.bind_user_id {
            // Bind flow: verify that the currently authenticated user matches
            // the user who initiated the bind request. This prevents a malicious
            // actor from completing another user's OAuth2 bind by replaying the
            // state token.
            if current_user_id != Some(&bind_user_id) {
                return Err(ApiError::Authorization(
                    "Cannot bind OAuth2 to another user's account".to_string(),
                ));
            }

            // Check if this provider account is already linked to a different user.
            // Silently reassigning would steal the linkage from the other user.
            if let Some(existing_user_id) = self
                .oauth2_service
                .find_user_by_provider(&provider_type, &user_info.provider_user_id)
                .await
                .map_err(ApiError::from)?
            {
                if existing_user_id != bind_user_id {
                    return Err(ApiError::AlreadyExists(
                        "This provider account is already linked to another user".to_string(),
                    ));
                }
            }

            // Bind flow: associate provider with existing user
            self.oauth2_service
                .upsert_user_provider(
                    &bind_user_id,
                    &provider_type,
                    &user_info.provider_user_id,
                    &user_info,
                )
                .await
                .map_err(ApiError::from)?;

            return Ok(ExchangeCodeResult {
                access_token: None,
                refresh_token: None,
                expires_in: 0,
                user_info: None,
                redirect_url: oauth_state.redirect_url,
                is_bind: true,
            });
        }

        // Login flow: find or create user
        let user_id = self
            .oauth2_service
            .find_user_by_provider(&provider_type, &user_info.provider_user_id)
            .await
            .map_err(ApiError::from)?;

        let (user, access_token, refresh_token) = if let Some(user_id) = user_id {
            // User exists - generate tokens using OAuth2 login method
            // (user already authenticated by OAuth2 provider)
            self.user_service
                .login_oauth2(&user_id, &user_info.provider_user_id, client_ip)
                .await
                .map_err(ApiError::from)?
        } else {
            let (user_id, _is_new) = self
                .oauth2_service
                .find_or_create_and_link(&self.user_service, &provider_type, &user_info)
                .await
                .map_err(ApiError::from)?;

            self.user_service
                .login_oauth2(&user_id, &user_info.provider_user_id, client_ip)
                .await
                .map_err(ApiError::from)?
        };

        // Get the actual access token duration from the JWT service
        let expires_in = self.user_service.access_token_duration_seconds();

        Ok(ExchangeCodeResult {
            access_token: Some(access_token),
            refresh_token: Some(refresh_token),
            expires_in,
            user_info: Some(user_to_oauth2_user_info(&user)),
            redirect_url: oauth_state.redirect_url,
            is_bind: false,
        })
    }

    /// List all available `OAuth2` provider instances
    pub async fn list_available_providers(&self) -> Result<Vec<ProviderInfo>, ApiError> {
        let providers = self.oauth2_service.list_available_instances().await;

        let result = providers
            .into_iter()
            .map(|(name, provider_type)| ProviderInfo {
                name,
                provider_type: provider_type.as_str().to_string(),
            })
            .collect();

        Ok(result)
    }

    /// Unlink `OAuth2` provider from user account
    ///
    /// If `provider_user_id` is provided, only unlinks that specific binding.
    /// If `provider_user_id` is None, unlinks all bindings for the provider type.
    ///
    /// Safety: refuses to unlink if this is the user's last authentication method
    /// (no password set and no other OAuth providers linked).
    pub async fn unlink_provider(
        &self,
        user_id: &UserId,
        provider: &str,
        provider_user_id: Option<&str>,
    ) -> Result<UnlinkResult, ApiError> {
        use synctv_core::models::OAuth2Provider;
        let provider_type = OAuth2Provider::from_str_name(provider)
            .ok_or_else(|| ApiError::InvalidInput(format!("Unknown provider type: {provider}")))?;

        // Check if user has other auth methods before unlinking
        let user = self
            .user_service
            .get_user(user_id)
            .await
            .map_err(ApiError::from)?;
        let linked_providers = self
            .oauth2_service
            .get_user_providers(user_id)
            .await
            .map_err(ApiError::from)?;

        // Count how many providers would remain after unlinking
        let remaining_providers = linked_providers
            .iter()
            .filter(|p| **p != provider_type)
            .count();

        // Check if user has a usable password (can authenticate without any OAuth2 provider).
        // This checks both signup_method AND whether the user has explicitly set a password,
        // handling the case where an OAuth2 user later sets their own password.
        let has_password_auth = user.has_usable_password();

        if remaining_providers == 0 && !has_password_auth {
            return Err(ApiError::InvalidInput(
                "Cannot unlink last authentication method. Please set a password first."
                    .to_string(),
            ));
        }

        let removed = if let Some(provider_user_id) = provider_user_id {
            // Unlink specific binding
            self.oauth2_service
                .unlink_provider(user_id, &provider_type, provider_user_id)
                .await
                .map_err(ApiError::from)?
        } else {
            // Unlink all bindings for this provider
            self.oauth2_service
                .unlink_provider_all(user_id, &provider_type)
                .await
                .map_err(ApiError::from)?
        };

        if !removed {
            return Err(ApiError::NotFound(
                "No binding found for this provider".to_string(),
            ));
        }

        Ok(UnlinkResult {
            success: true,
            removed_count: 1,
        })
    }

    /// Get linked `OAuth2` providers for authenticated user
    pub async fn get_linked_providers(
        &self,
        user_id: &UserId,
    ) -> Result<Vec<LinkedProviderInfo>, ApiError> {
        // Fetch complete provider mappings with username and linked_at
        let mappings = self
            .oauth2_service
            .get_user_provider_mappings(user_id)
            .await
            .map_err(ApiError::from)?;

        let result = mappings
            .into_iter()
            .map(|mapping| LinkedProviderInfo {
                provider_type: mapping.provider,
                provider_username: mapping.username,
                linked_at: mapping.created_at.timestamp(),
            })
            .collect();

        Ok(result)
    }
}

/// Result of exchanging authorization code
pub struct ExchangeCodeResult {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: i64,
    pub user_info: Option<OAuth2UserInfo>,
    pub redirect_url: Option<String>,
    pub is_bind: bool,
}

/// `OAuth2` provider information
pub struct ProviderInfo {
    pub name: String,
    pub provider_type: String,
}

/// Unlink provider result
pub struct UnlinkResult {
    pub success: bool,
    pub removed_count: i32,
}

/// Linked `OAuth2` provider information
pub struct LinkedProviderInfo {
    pub provider_type: String,
    pub provider_username: String,
    pub linked_at: i64, // Unix timestamp (seconds)
}

/// Convert User model to `OAuth2UserInfo` proto
fn user_to_oauth2_user_info(user: &User) -> OAuth2UserInfo {
    use synctv_proto::common::{UserRole as ProtoUserRole, UserStatus as ProtoUserStatus};

    let proto_role = match user.role {
        UserRole::Root => ProtoUserRole::Root,
        UserRole::Admin => ProtoUserRole::Admin,
        UserRole::User => ProtoUserRole::User,
    };

    let proto_status = match user.status {
        UserStatus::Active => ProtoUserStatus::Active,
        UserStatus::Pending => ProtoUserStatus::Pending,
        UserStatus::Rejected => ProtoUserStatus::Rejected,
        UserStatus::Banned => ProtoUserStatus::Banned,
    };

    OAuth2UserInfo {
        user_id: user.id.to_string(),
        username: user.username.clone(),
        email: user.email.clone().unwrap_or_default(),
        avatar: String::new(), // User model doesn't have avatar field currently
        role: proto_role as i32,
        status: proto_status as i32,
        created_at: user.created_at.timestamp(),
    }
}

/// Convert proto `OAuth2ProviderInstance` to `ProviderInfo`
impl From<ProviderInfo> for OAuth2ProviderInstance {
    fn from(info: ProviderInfo) -> Self {
        Self {
            name: info.name,
            r#type: info.provider_type,
        }
    }
}

/// Convert `LinkedProviderInfo` to proto `LinkedProvider`
impl From<LinkedProviderInfo> for LinkedProvider {
    fn from(info: LinkedProviderInfo) -> Self {
        Self {
            provider_type: info.provider_type,
            provider_username: info.provider_username,
            linked_at: info.linked_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::impls::ApiError;

    #[test]
    fn test_bind_user_lookup_backend_failure_stays_service_unavailable() {
        let mapped = super::OAuth2ApiImpl::map_bind_user_lookup_error(
            synctv_core::Error::ServiceUnavailable("user backend unavailable".to_string()),
        );

        assert!(
            matches!(mapped, ApiError::ServiceUnavailable(ref msg) if msg == "user backend unavailable"),
            "bind user lookup backend failures must not be reported as authentication failures, got: {mapped:?}"
        );
    }

    #[test]
    fn test_bind_user_lookup_not_found_stays_authentication_failed() {
        let mapped = super::OAuth2ApiImpl::map_bind_user_lookup_error(
            synctv_core::Error::NotFound("missing row".to_string()),
        );

        assert!(
            matches!(mapped, ApiError::Authentication(ref msg) if msg == "Authentication failed"),
            "missing bind users should still be treated as authentication failure, got: {mapped:?}"
        );
    }

    /// Test that the unlink provider safety check uses `has_usable_password()`
    /// instead of just checking `signup_method`.
    ///
    /// Security: An OAuth2-only user with no other providers and no usable
    /// password must NOT be allowed to unlink their last `OAuth2` provider,
    /// as they would be locked out of their account.
    ///
    /// This test verifies the model-level behavior used by `unlink_provider`.
    #[test]
    fn test_oauth2_unlink_checks_actual_password_capability() {
        use synctv_core::models::{SignupMethod, User, UserId, UserRole, UserStatus};

        let now = chrono::Utc::now();

        // Case 1: OAuth2 user with pv=0 (random password, never set their own)
        let oauth2_user_no_password = User {
            id: UserId::new(),
            username: "oauth2_user".to_string(),
            email: None,
            password_hash: "$argon2id$v=19$m=16384,t=3,p=1$random$hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: SignupMethod::OAuth2,
            email_verified: false,
            created_at: now,
            updated_at: now,
            password_changed_at: now,
            password_version: 0,
            version: 0,
            deleted_at: None,
        };
        assert!(
            !oauth2_user_no_password.has_usable_password(),
            "OAuth2 user with pv=0 should NOT have usable password"
        );

        // Case 2: OAuth2 user who later set a password (pv=1)
        let oauth2_user_with_password = User {
            password_version: 1,
            ..oauth2_user_no_password.clone()
        };
        assert!(
            oauth2_user_with_password.has_usable_password(),
            "OAuth2 user with pv=1 should have usable password (they explicitly set one)"
        );

        // Case 3: Email signup user always has usable password
        let email_user = User {
            signup_method: SignupMethod::Email,
            ..oauth2_user_no_password
        };
        assert!(
            email_user.has_usable_password(),
            "Email signup user should always have usable password"
        );
    }

    #[test]
    fn test_unlink_provider_missing_binding_maps_to_not_found_api_error() {
        let err = ApiError::NotFound("No binding found for this provider".to_string());

        assert!(matches!(err.classify(), crate::impls::ErrorKind::NotFound));
        assert_eq!(err.code(), crate::impls::error_codes::NOT_FOUND);
    }
}
