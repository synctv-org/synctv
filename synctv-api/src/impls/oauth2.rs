//! OAuth2 API Implementation
//!
//! Unified implementation for OAuth2 API operations.
//! Used by both HTTP and gRPC handlers.
//!
//! ## Frontend-Driven OAuth2 Flow
//!
//! This implementation uses a frontend-driven OAuth2 flow where the frontend
//! handles the redirect from the OAuth2 provider and extracts the authorization
//! code and state before sending them to the backend.
//!
//! Flow:
//! 1. Frontend calls `get_authorization_url` to get the auth URL
//! 2. Frontend redirects user to the auth URL (OAuth2 provider)
//! 3. User authorizes on the provider
//! 4. Provider redirects to **frontend URL** with code and state
//! 5. Frontend extracts code and state from URL parameters
//! 6. Frontend calls `exchange_authorization_code` with code and state
//! 7. Backend validates state, exchanges code for user info, creates/logs in user
//! 8. Backend returns JWT token to frontend

use std::sync::Arc;
use synctv_core::models::{User, UserId, UserRole, UserStatus};
use synctv_core::service::{OAuth2Service, UserService};
use synctv_proto::client::{
    OAuth2UserInfo, OAuth2ProviderInstance, LinkedProvider,
};

use super::ApiError;

/// OAuth2 API implementation
#[derive(Clone)]
pub struct OAuth2ApiImpl {
    pub oauth2_service: Arc<OAuth2Service>,
    pub user_service: Arc<UserService>,
}

impl OAuth2ApiImpl {
    #[must_use]
    pub fn new(
        oauth2_service: Arc<OAuth2Service>,
        user_service: Arc<UserService>,
    ) -> Self {
        Self {
            oauth2_service,
            user_service,
        }
    }

    /// Get authorization URL for OAuth2 login flow
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

    /// Get authorization URL for binding OAuth2 provider to existing user
    ///
    /// Requires authentication. The user_id should come from the JWT token.
    pub async fn get_authorization_url_for_bind(
        &self,
        user_id: &UserId,
        provider: &str,
        redirect_url: Option<String>,
    ) -> Result<(String, String), ApiError> {
        // Verify user exists and is not banned/deleted
        let user = self
            .user_service
            .get_user(user_id)
            .await
            .map_err(|_| ApiError::Authentication("User not found".to_string()))?;

        if user.is_deleted() || user.status == UserStatus::Banned {
            return Err(ApiError::Authentication("User account is not active".to_string()));
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
    /// Frontend calls this after receiving code and state from OAuth2 provider redirect.
    ///
    /// For login flow (no bind_user_id in state):
    /// - If user exists: log them in
    /// - If user doesn't exist: create new user account
    ///
    /// For bind flow (bind_user_id present in state):
    /// - Binds the OAuth2 provider to the existing user account
    /// - Returns empty tokens (user is already logged in)
    pub async fn exchange_authorization_code(
        &self,
        provider: &str,
        code: &str,
        state: &str,
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
            // Bind flow: associate provider with existing user
            self.oauth2_service
                .upsert_user_provider(&bind_user_id, &provider_type, &user_info.provider_user_id, &user_info)
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
                .login_oauth2(&user_id)
                .await
                .map_err(ApiError::from)?
        } else {
            // User doesn't exist, create new account using register flow
            // Generate a random password since OAuth2 users don't use password auth
            let random_password = nanoid::nanoid!(32);

            let (new_user, access_token, refresh_token) = self
                .user_service
                .register(
                    user_info.username.clone(),
                    user_info.email.clone(),
                    random_password,
                )
                .await
                .map_err(ApiError::from)?;

            // Link OAuth2 provider to new user
            self.oauth2_service
                .upsert_user_provider(&new_user.id, &provider_type, &user_info.provider_user_id, &user_info)
                .await
                .map_err(ApiError::from)?;

            (new_user, access_token, refresh_token)
        };

        // Calculate token expiration (tokens are typically valid for 7 days)
        let expires_in = 7 * 24 * 3600; // 7 days in seconds

        Ok(ExchangeCodeResult {
            access_token: Some(access_token),
            refresh_token: Some(refresh_token),
            expires_in,
            user_info: Some(user_to_oauth2_user_info(&user)),
            redirect_url: oauth_state.redirect_url,
            is_bind: false,
        })
    }

    /// List all available OAuth2 provider instances
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

    /// Unlink OAuth2 provider from user account
    ///
    /// If provider_user_id is provided, only unlinks that specific binding.
    /// If provider_user_id is None, unlinks all bindings for the provider type.
    pub async fn unlink_provider(
        &self,
        user_id: &UserId,
        provider: &str,
        provider_user_id: Option<&str>,
    ) -> Result<UnlinkResult, ApiError> {
        use synctv_core::models::OAuth2Provider;
        let provider_type = OAuth2Provider::from_str_name(provider)
            .ok_or_else(|| ApiError::InvalidInput(format!("Unknown provider type: {provider}")))?;

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

        Ok(UnlinkResult {
            success: removed,
            removed_count: if removed { 1 } else { 0 },
        })
    }

    /// Get linked OAuth2 providers for authenticated user
    pub async fn get_linked_providers(
        &self,
        user_id: &UserId,
    ) -> Result<Vec<LinkedProviderInfo>, ApiError> {
        let providers = self
            .oauth2_service
            .get_user_providers(user_id)
            .await
            .map_err(ApiError::from)?;

        // TODO: Get detailed provider info including username and linked_at
        // For now, just return provider types
        let result = providers
            .into_iter()
            .map(|provider_type| LinkedProviderInfo {
                provider_type: provider_type.as_str().to_string(),
                provider_username: String::new(), // TODO: fetch from repository
                linked_at: String::new(),         // TODO: fetch from repository
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

/// OAuth2 provider information
pub struct ProviderInfo {
    pub name: String,
    pub provider_type: String,
}

/// Unlink provider result
pub struct UnlinkResult {
    pub success: bool,
    pub removed_count: i32,
}

/// Linked OAuth2 provider information
pub struct LinkedProviderInfo {
    pub provider_type: String,
    pub provider_username: String,
    pub linked_at: String,
}

/// Convert User model to OAuth2UserInfo proto
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
        UserStatus::Banned => ProtoUserStatus::Banned,
    };

    OAuth2UserInfo {
        user_id: user.id.to_string(),
        username: user.username.clone(),
        email: user.email.clone().unwrap_or_default(),
        avatar: String::new(), // User model doesn't have avatar field currently
        role: proto_role as i32,
        status: proto_status as i32,
        created_at: user.created_at.to_rfc3339(),
    }
}

/// Convert proto OAuth2ProviderInstance to ProviderInfo
impl From<ProviderInfo> for OAuth2ProviderInstance {
    fn from(info: ProviderInfo) -> Self {
        OAuth2ProviderInstance {
            name: info.name,
            r#type: info.provider_type,
        }
    }
}

/// Convert LinkedProviderInfo to proto LinkedProvider
impl From<LinkedProviderInfo> for LinkedProvider {
    fn from(info: LinkedProviderInfo) -> Self {
        LinkedProvider {
            provider_type: info.provider_type,
            provider_username: info.provider_username,
            linked_at: info.linked_at,
        }
    }
}
