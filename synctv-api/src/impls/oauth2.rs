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

use std::{collections::HashSet, sync::Arc};
use synctv_core::models::{User, UserId, UserRole, UserStatus};
use synctv_core::provider::ExecutionControl;
use synctv_core::service::{OAuth2LinkResult, OAuth2Service, UserService};
use synctv_proto::client::{
    ExchangeAuthorizationCodeRequest, ExchangeAuthorizationCodeResponse,
    GetAuthorizationUrlForBindRequest, GetAuthorizationUrlForBindResponse,
    GetAuthorizationUrlRequest, GetAuthorizationUrlResponse, GetLinkedProvidersResponse,
    LinkedProvider, ListAvailableProvidersResponse, OAuth2ProviderInstance, OAuth2UserInfo,
    UnlinkProviderRequest, UnlinkProviderResponse,
};

use super::ApiError;

/// `OAuth2` API implementation
#[derive(Clone)]
pub struct OAuth2ApiImpl {
    pub oauth2_service: Arc<OAuth2Service>,
    pub user_service: Arc<UserService>,
    public_id_codec: Arc<crate::PublicIdCodec>,
}

impl OAuth2ApiImpl {
    fn optional_non_empty_trimmed(value: &str) -> Option<String> {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }

    fn oauth2_identity_unlink_counts(
        linked_mappings: &[synctv_core::models::oauth2_client::UserOAuthProviderMapping],
        provider_type: &synctv_core::models::OAuth2Provider,
        provider_instance_name: Option<&str>,
        provider_user_id: Option<&str>,
    ) -> (usize, usize) {
        linked_mappings
            .iter()
            .fold((0_usize, 0_usize), |counts, mapping| {
                let (mut target, mut remaining) = counts;
                let same_provider = mapping.provider == provider_type.as_str();
                let will_unlink = same_provider
                    && match provider_user_id {
                        Some(target_provider_user_id) => {
                            provider_instance_name.is_some_and(|target_instance_name| {
                                mapping.provider_instance_name == target_instance_name
                                    && mapping.provider_user_id == target_provider_user_id
                            })
                        }
                        None => true,
                    };
                if will_unlink {
                    target += 1;
                } else {
                    remaining += 1;
                }
                (target, remaining)
            })
    }

    fn active_oauth2_mappings<'a>(
        linked_mappings: &'a [synctv_core::models::oauth2_client::UserOAuthProviderMapping],
        active_provider_keys: &HashSet<(String, String)>,
    ) -> Vec<&'a synctv_core::models::oauth2_client::UserOAuthProviderMapping> {
        linked_mappings
            .iter()
            .filter(|mapping| {
                active_provider_keys.contains(&(
                    mapping.provider_instance_name.clone(),
                    mapping.provider.clone(),
                ))
            })
            .collect()
    }

    async fn active_oauth2_provider_keys(&self) -> Result<HashSet<(String, String)>, ApiError> {
        Ok(self
            .oauth2_service
            .list_available_instances()
            .await
            .map_err(ApiError::from)?
            .into_iter()
            .map(|(name, provider, _)| (name, provider.as_str().to_string()))
            .collect())
    }

    fn map_bind_user_lookup_error(err: synctv_core::Error) -> ApiError {
        match err {
            synctv_core::Error::NotFound(_) => {
                ApiError::Authentication("Authentication failed".to_string())
            }
            other => ApiError::from(other),
        }
    }

    #[must_use]
    pub fn new(
        oauth2_service: Arc<OAuth2Service>,
        user_service: Arc<UserService>,
        public_id_codec: Arc<crate::PublicIdCodec>,
    ) -> Self {
        Self {
            oauth2_service,
            user_service,
            public_id_codec,
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
        self.get_authorization_url_with_control(provider, redirect_url, None)
            .await
    }

    pub async fn get_authorization_url_with_control(
        &self,
        provider: &str,
        redirect_url: Option<String>,
        control: Option<&ExecutionControl>,
    ) -> Result<(String, String), ApiError> {
        let (auth_url, state) = self
            .oauth2_service
            .get_authorization_url_with_control(provider, redirect_url, control)
            .await
            .map_err(ApiError::from)?;

        Ok((auth_url, state))
    }

    pub async fn get_authorization_url_response_with_control(
        &self,
        req: GetAuthorizationUrlRequest,
        control: Option<&ExecutionControl>,
    ) -> Result<GetAuthorizationUrlResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let redirect_url = Self::optional_non_empty_trimmed(&req.redirect_url);
        let (authorization_url, state) = self
            .get_authorization_url_with_control(&req.provider, redirect_url, control)
            .await?;

        Ok(GetAuthorizationUrlResponse {
            authorization_url,
            state,
        })
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
        self.get_authorization_url_for_bind_with_control(user_id, provider, redirect_url, None)
            .await
    }

    pub async fn get_authorization_url_for_bind_with_control(
        &self,
        user_id: &UserId,
        provider: &str,
        redirect_url: Option<String>,
        control: Option<&ExecutionControl>,
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
            .get_authorization_url_with_user_with_control(
                provider,
                redirect_url,
                Some(*user_id),
                control,
            )
            .await
            .map_err(ApiError::from)?;

        Ok((auth_url, state))
    }

    pub async fn get_authorization_url_for_bind_response_with_control(
        &self,
        user_id: &UserId,
        req: GetAuthorizationUrlForBindRequest,
        control: Option<&ExecutionControl>,
    ) -> Result<GetAuthorizationUrlForBindResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let redirect_url = Self::optional_non_empty_trimmed(&req.redirect_url);
        let (authorization_url, state) = self
            .get_authorization_url_for_bind_with_control(
                user_id,
                &req.provider,
                redirect_url,
                control,
            )
            .await?;

        Ok(GetAuthorizationUrlForBindResponse {
            authorization_url,
            state,
        })
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
        self.exchange_authorization_code_with_control(
            provider,
            code,
            state,
            current_user_id,
            client_ip,
            None,
        )
        .await
    }

    pub async fn exchange_authorization_code_with_control(
        &self,
        provider: &str,
        code: &str,
        state: &str,
        current_user_id: Option<&UserId>,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<ExchangeCodeResult, ApiError> {
        // 1. Verify state and retrieve stored OAuth2 state
        let oauth_state = self
            .oauth2_service
            .verify_state_with_control(state, control)
            .await
            .map_err(ApiError::from)?;

        // Verify provider matches
        if oauth_state.instance_name != provider {
            return Err(ApiError::InvalidInput(
                "Provider mismatch between request and stored state".to_string(),
            ));
        }

        // 2. Exchange code for user info using PKCE verifier from stored state
        let user_info = self
            .oauth2_service
            .exchange_code_for_user_info_with_state_and_control(
                provider,
                code,
                &oauth_state,
                control,
            )
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
                .find_user_by_provider_instance(provider, &user_info.provider_user_id)
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
                .upsert_user_provider(&bind_user_id, &user_info)
                .await
                .map_err(ApiError::from)?;

            return Ok(ExchangeCodeResult {
                access_token: None,
                refresh_token: None,
                expires_in: 0,
                user_info: None,
                redirect_url: oauth_state.redirect_url,
                is_bind: true,
                registration_review_required: false,
                registration_review_id: None,
            });
        }

        // Login flow: find or create user
        let user_id = self
            .oauth2_service
            .find_user_by_provider_instance(provider, &user_info.provider_user_id)
            .await
            .map_err(ApiError::from)?;

        let login = if let Some(user_id) = user_id {
            // User exists - generate tokens using OAuth2 login method
            // (user already authenticated by OAuth2 provider)
            self.user_service
                .login_oauth2_with_control(
                    &user_id,
                    &user_info.provider_user_id,
                    client_ip,
                    control,
                )
                .await
                .map_err(ApiError::from)?
        } else {
            match self
                .oauth2_service
                .find_or_create_and_link(&self.user_service, provider, &user_info)
                .await
                .map_err(ApiError::from)?
            {
                OAuth2LinkResult::Linked { user_id, .. } => self
                    .user_service
                    .login_oauth2_with_control(
                        &user_id,
                        &user_info.provider_user_id,
                        client_ip,
                        control,
                    )
                    .await
                    .map_err(ApiError::from)?,
                OAuth2LinkResult::PendingReview(pending) => {
                    return Ok(ExchangeCodeResult {
                        access_token: None,
                        refresh_token: None,
                        expires_in: 0,
                        user_info: None,
                        redirect_url: oauth_state.redirect_url,
                        is_bind: false,
                        registration_review_required: true,
                        registration_review_id: Some(
                            self.public_id_codec
                                .encode_user_id(pending.request_id)
                                .map_err(ApiError::InvalidInput)?,
                        ),
                    });
                }
            }
        };

        // Get the actual access token duration from the JWT service
        let expires_in = self.user_service.access_token_duration_seconds();

        match login {
            synctv_core::service::AuthenticatedLogin::Complete {
                user,
                email,
                access_token,
                refresh_token,
            } => Ok(ExchangeCodeResult {
                access_token: Some(access_token),
                refresh_token: Some(refresh_token),
                expires_in,
                user_info: Some(user_to_oauth2_user_info(
                    &user,
                    email.as_deref(),
                    &self.public_id_codec,
                )),
                redirect_url: oauth_state.redirect_url,
                is_bind: false,
                registration_review_required: false,
                registration_review_id: None,
            }),
            synctv_core::service::AuthenticatedLogin::MfaRequired { .. } => {
                Err(ApiError::Internal(
                    "OAuth2 must not start a two-factor authentication session".to_string(),
                ))
            }
        }
    }

    pub async fn exchange_authorization_code_response_with_control(
        &self,
        req: ExchangeAuthorizationCodeRequest,
        current_user_id: Option<&UserId>,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<ExchangeAuthorizationCodeResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let result = self
            .exchange_authorization_code_with_control(
                &req.provider,
                &req.code,
                &req.state,
                current_user_id,
                client_ip,
                control,
            )
            .await?;

        Ok(ExchangeAuthorizationCodeResponse {
            access_token: result.access_token.unwrap_or_default(),
            refresh_token: result.refresh_token.unwrap_or_default(),
            expires_in: result.expires_in,
            user_info: result.user_info,
            redirect_url: result.redirect_url.unwrap_or_default(),
            is_bind: result.is_bind,
            registration_review_required: result.registration_review_required,
            registration_review_id: result.registration_review_id.unwrap_or_default(),
        })
    }

    /// List all available `OAuth2` provider instances
    pub async fn list_available_providers(&self) -> Result<Vec<ProviderInfo>, ApiError> {
        let providers = self
            .oauth2_service
            .list_available_instances()
            .await
            .map_err(ApiError::from)?;

        let result = providers
            .into_iter()
            .map(|(name, provider_type, signup_policy)| ProviderInfo {
                name,
                provider_type: provider_type.as_str().to_string(),
                signup_enabled: signup_policy.enable_signup,
                signup_need_review: signup_policy.signup_need_review,
            })
            .collect();

        Ok(result)
    }

    pub async fn list_available_providers_response(
        &self,
    ) -> Result<ListAvailableProvidersResponse, ApiError> {
        let providers = self
            .list_available_providers()
            .await?
            .into_iter()
            .map(std::convert::Into::into)
            .collect();

        Ok(ListAvailableProvidersResponse { providers })
    }

    /// Unlink `OAuth2` provider from user account
    ///
    /// If `provider_user_id` is provided, only unlinks that specific binding.
    /// If `provider_user_id` is None, unlinks all bindings for the provider type.
    ///
    /// Safety: users registered through `OAuth2` must keep at least one
    /// `OAuth2` identity linked. Other signup methods may unlink `OAuth2`
    /// identities freely because `OAuth2` is not their registration resource.
    pub async fn unlink_provider(
        &self,
        user_id: &UserId,
        provider: &str,
        provider_instance_name: Option<&str>,
        provider_user_id: Option<&str>,
    ) -> Result<UnlinkResult, ApiError> {
        use synctv_core::models::OAuth2Provider;
        let provider_type = OAuth2Provider::from_str_name(provider)
            .ok_or_else(|| ApiError::InvalidInput(format!("Unknown provider type: {provider}")))?;
        let provider_instance_name = provider_instance_name
            .map(str::trim)
            .filter(|value| !value.is_empty());

        if provider_user_id.is_some() && provider_instance_name.is_none() {
            return Err(ApiError::InvalidInput(
                "provider_instance_name is required when provider_user_id is set".to_string(),
            ));
        }

        let linked_mappings = self
            .oauth2_service
            .get_user_provider_mappings(user_id)
            .await
            .map_err(ApiError::from)?;
        let active_provider_keys = self.active_oauth2_provider_keys().await?;
        let active_linked_mappings =
            Self::active_oauth2_mappings(&linked_mappings, &active_provider_keys);
        let active_linked_mappings = active_linked_mappings
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();

        let (target_oauth2_identities, remaining_oauth2_identities) =
            Self::oauth2_identity_unlink_counts(
                &active_linked_mappings,
                &provider_type,
                provider_instance_name,
                provider_user_id,
            );

        if target_oauth2_identities == 0 {
            return Err(ApiError::NotFound(
                "No binding found for this provider".to_string(),
            ));
        }

        let (_preferences, auth_factors) = self
            .user_service
            .get_user_preferences(user_id)
            .await
            .map_err(ApiError::from)?;
        let remaining_sign_in_method_count =
            UserService::sign_in_method_count(&auth_factors, remaining_oauth2_identities);
        if remaining_sign_in_method_count == 0 {
            return Err(ApiError::InvalidInput(
                "Cannot unlink the last sign-in method".to_string(),
            ));
        }

        let removed = if let Some(provider_user_id) = provider_user_id {
            // Unlink specific binding
            self.oauth2_service
                .unlink_provider(
                    user_id,
                    provider_instance_name.expect("validated above"),
                    provider_user_id,
                )
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

    pub async fn unlink_provider_response(
        &self,
        user_id: &UserId,
        req: UnlinkProviderRequest,
    ) -> Result<UnlinkProviderResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let provider_user_id = Self::optional_non_empty_trimmed(&req.provider_user_id);
        let provider_instance_name = Self::optional_non_empty_trimmed(&req.provider_instance_name);
        let result = self
            .unlink_provider(
                user_id,
                &req.provider,
                provider_instance_name.as_deref(),
                provider_user_id.as_deref(),
            )
            .await?;

        Ok(UnlinkProviderResponse {
            success: result.success,
            removed_count: result.removed_count,
        })
    }

    /// Get linked `OAuth2` providers for authenticated user
    pub async fn get_linked_providers(
        &self,
        user_id: &UserId,
    ) -> Result<Vec<LinkedProviderInfo>, ApiError> {
        let mappings = self
            .oauth2_service
            .get_user_provider_mappings(user_id)
            .await
            .map_err(ApiError::from)?;
        let available = self.active_oauth2_provider_keys().await?;

        let result = mappings
            .into_iter()
            .filter(|mapping| {
                available.contains(&(
                    mapping.provider_instance_name.clone(),
                    mapping.provider.clone(),
                ))
            })
            .map(|mapping| LinkedProviderInfo {
                provider_type: mapping.provider,
                provider_instance_name: mapping.provider_instance_name,
                provider_issuer: mapping.provider_issuer.unwrap_or_default(),
                provider_user_id: mapping.provider_user_id,
                provider_username: mapping.username,
                linked_at: mapping.created_at.timestamp(),
            })
            .collect();

        Ok(result)
    }

    pub async fn get_linked_providers_response(
        &self,
        user_id: &UserId,
    ) -> Result<GetLinkedProvidersResponse, ApiError> {
        let providers = self
            .get_linked_providers(user_id)
            .await?
            .into_iter()
            .map(std::convert::Into::into)
            .collect();

        Ok(GetLinkedProvidersResponse { providers })
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
    pub registration_review_required: bool,
    pub registration_review_id: Option<String>,
}

/// `OAuth2` provider information
pub struct ProviderInfo {
    pub name: String,
    pub provider_type: String,
    pub signup_enabled: bool,
    pub signup_need_review: bool,
}

/// Unlink provider result
pub struct UnlinkResult {
    pub success: bool,
    pub removed_count: i32,
}

/// Linked `OAuth2` provider information
pub struct LinkedProviderInfo {
    pub provider_type: String,
    pub provider_instance_name: String,
    pub provider_issuer: String,
    pub provider_user_id: String,
    pub provider_username: String,
    pub linked_at: i64, // Unix timestamp (seconds)
}

/// Convert User model to `OAuth2UserInfo` proto
fn user_to_oauth2_user_info(
    user: &User,
    email: Option<&str>,
    public_id_codec: &crate::PublicIdCodec,
) -> OAuth2UserInfo {
    use synctv_proto::common::{UserRole as ProtoUserRole, UserStatus as ProtoUserStatus};

    let proto_role = match user.role {
        UserRole::Root => ProtoUserRole::Root,
        UserRole::Admin => ProtoUserRole::Admin,
        UserRole::User => ProtoUserRole::User,
    };

    let proto_status = match user.status {
        UserStatus::Active => ProtoUserStatus::Active,
        UserStatus::Banned => ProtoUserStatus::Banned,
    };

    OAuth2UserInfo {
        user_id: public_id_codec
            .encode_user_id(user.id)
            .expect("positive user ID must encode"),
        username: user.username.clone(),
        email: email.unwrap_or_default().to_string(),
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
            signup_enabled: info.signup_enabled,
            signup_need_review: info.signup_need_review,
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
            provider_instance_name: info.provider_instance_name,
            provider_issuer: info.provider_issuer,
            provider_user_id: info.provider_user_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::impls::ApiError;
    use synctv_proto::client::{
        ExchangeAuthorizationCodeRequest, GetAuthorizationUrlForBindRequest,
        GetAuthorizationUrlRequest, UnlinkProviderRequest,
    };

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

    #[test]
    fn test_unlink_provider_missing_binding_maps_to_not_found_api_error() {
        let err = ApiError::NotFound("No binding found for this provider".to_string());

        assert!(matches!(err.classify(), crate::impls::ErrorKind::NotFound));
        assert_eq!(err.code(), crate::impls::error_codes::NOT_FOUND);
    }

    #[test]
    fn test_oauth2_request_validation_rejects_invalid_redirect_url() {
        let err = crate::impls::validate_proto_request(&GetAuthorizationUrlRequest {
            provider: "github".to_string(),
            redirect_url: "javascript:alert(1)".to_string(),
        })
        .expect_err("invalid redirect URL must be rejected");

        assert!(err.to_string().contains("redirect_url"));
    }

    #[test]
    fn test_oauth2_request_validation_rejects_custom_scheme_redirect_url() {
        let err = crate::impls::validate_proto_request(&GetAuthorizationUrlForBindRequest {
            provider: "logto1".to_string(),
            redirect_url: "native-app://oauth2/callback".to_string(),
        })
        .expect_err("custom scheme redirect URL must be rejected");

        assert!(err.to_string().contains("redirect_url"));
    }

    #[test]
    fn test_oauth2_request_validation_rejects_invalid_exchange_code() {
        let err = crate::impls::validate_proto_request(&ExchangeAuthorizationCodeRequest {
            provider: "github".to_string(),
            code: "code with spaces".to_string(),
            state: "AbCdEfGh1234567890aBcDeFgHiJkLm".to_string(),
        })
        .expect_err("invalid code must be rejected");

        assert!(err.to_string().contains("code"));
    }

    #[test]
    fn test_oauth2_request_validation_rejects_invalid_exchange_state() {
        let err = crate::impls::validate_proto_request(&ExchangeAuthorizationCodeRequest {
            provider: "github".to_string(),
            code: "code.with.dots".to_string(),
            state: "short".to_string(),
        })
        .expect_err("invalid state must be rejected");

        assert!(err.to_string().contains("state"));
    }

    #[test]
    fn test_oauth2_request_validation_rejects_too_long_provider_user_id() {
        let err = crate::impls::validate_proto_request(&UnlinkProviderRequest {
            provider: "github".to_string(),
            provider_user_id: "a".repeat(257),
            provider_instance_name: "github-main".to_string(),
        })
        .expect_err("overlong provider_user_id must be rejected");

        assert!(err.to_string().contains("provider_user_id"));
    }

    #[test]
    fn test_oauth2_unlink_counts_specific_identity_without_removing_same_provider_siblings() {
        use synctv_core::models::oauth2_client::{OAuth2Provider, UserOAuthProviderMapping};
        use synctv_core::models::UserId;

        let now = chrono::Utc::now();
        let mappings = vec![
            UserOAuthProviderMapping {
                id: 1,
                provider: "github".to_string(),
                provider_instance_name: "github-main".to_string(),
                provider_issuer: Some("https://github.com".to_string()),
                provider_user_id: "github-a".to_string(),
                user_id: UserId::expect_positive(42),
                username: "github-a".to_string(),
                email: None,
                avatar_url: None,
                created_at: now,
                updated_at: now,
            },
            UserOAuthProviderMapping {
                id: 2,
                provider: "github".to_string(),
                provider_instance_name: "github-backup".to_string(),
                provider_issuer: Some("https://github.example.com".to_string()),
                provider_user_id: "github-b".to_string(),
                user_id: UserId::expect_positive(42),
                username: "github-b".to_string(),
                email: None,
                avatar_url: None,
                created_at: now,
                updated_at: now,
            },
        ];

        let (target, remaining) = super::OAuth2ApiImpl::oauth2_identity_unlink_counts(
            &mappings,
            &OAuth2Provider::GitHub,
            Some("github-main"),
            Some("github-a"),
        );

        assert_eq!(target, 1);
        assert_eq!(remaining, 1);
    }

    #[test]
    fn test_oauth2_unlink_counts_provider_wide_unlink_removes_all_provider_identities() {
        use synctv_core::models::oauth2_client::{OAuth2Provider, UserOAuthProviderMapping};
        use synctv_core::models::UserId;

        let now = chrono::Utc::now();
        let mappings = vec![
            UserOAuthProviderMapping {
                id: 1,
                provider: "github".to_string(),
                provider_instance_name: "github-main".to_string(),
                provider_issuer: Some("https://github.com".to_string()),
                provider_user_id: "github-a".to_string(),
                user_id: UserId::expect_positive(42),
                username: "github-a".to_string(),
                email: None,
                avatar_url: None,
                created_at: now,
                updated_at: now,
            },
            UserOAuthProviderMapping {
                id: 2,
                provider: "github".to_string(),
                provider_instance_name: "github-backup".to_string(),
                provider_issuer: Some("https://github.example.com".to_string()),
                provider_user_id: "github-b".to_string(),
                user_id: UserId::expect_positive(42),
                username: "github-b".to_string(),
                email: None,
                avatar_url: None,
                created_at: now,
                updated_at: now,
            },
            UserOAuthProviderMapping {
                id: 3,
                provider: "google".to_string(),
                provider_instance_name: "google".to_string(),
                provider_issuer: Some("https://accounts.google.com".to_string()),
                provider_user_id: "google-a".to_string(),
                user_id: UserId::expect_positive(42),
                username: "google-a".to_string(),
                email: None,
                avatar_url: None,
                created_at: now,
                updated_at: now,
            },
        ];

        let (target, remaining) = super::OAuth2ApiImpl::oauth2_identity_unlink_counts(
            &mappings,
            &OAuth2Provider::GitHub,
            None,
            None,
        );

        assert_eq!(target, 2);
        assert_eq!(remaining, 1);
    }

    #[test]
    fn test_active_oauth2_mappings_filters_missing_provider_instances() {
        use synctv_core::models::oauth2_client::UserOAuthProviderMapping;
        use synctv_core::models::UserId;

        let now = chrono::Utc::now();
        let mappings = vec![
            UserOAuthProviderMapping {
                id: 1,
                provider: "github".to_string(),
                provider_instance_name: "github-main".to_string(),
                provider_issuer: Some("https://github.com".to_string()),
                provider_user_id: "github-a".to_string(),
                user_id: UserId::expect_positive(42),
                username: "github-a".to_string(),
                email: None,
                avatar_url: None,
                created_at: now,
                updated_at: now,
            },
            UserOAuthProviderMapping {
                id: 2,
                provider: "google".to_string(),
                provider_instance_name: "removed-google".to_string(),
                provider_issuer: Some("https://accounts.google.com".to_string()),
                provider_user_id: "google-a".to_string(),
                user_id: UserId::expect_positive(42),
                username: "google-a".to_string(),
                email: None,
                avatar_url: None,
                created_at: now,
                updated_at: now,
            },
        ];
        let active = HashSet::from([("github-main".to_string(), "github".to_string())]);

        let filtered = super::OAuth2ApiImpl::active_oauth2_mappings(&mappings, &active);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].provider_instance_name, "github-main");
    }

    #[test]
    fn test_linked_provider_proto_includes_provider_user_id() {
        let proto: synctv_proto::client::LinkedProvider = super::LinkedProviderInfo {
            provider_type: "github".to_string(),
            provider_instance_name: "github-main".to_string(),
            provider_issuer: "https://github.com".to_string(),
            provider_user_id: "gh_123".to_string(),
            provider_username: "alice".to_string(),
            linked_at: 1_700_000_000,
        }
        .into();

        assert_eq!(proto.provider_type, "github");
        assert_eq!(proto.provider_instance_name, "github-main");
        assert_eq!(proto.provider_user_id, "gh_123");
        assert_eq!(proto.provider_username, "alice");
    }
}
