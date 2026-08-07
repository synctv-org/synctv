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
use synctv_core::models::{OAuth2Provider, User, UserId, UserRole, UserStatus};
use synctv_core::provider::ExecutionControl;
use synctv_core::service::{OAuth2LinkResult, OAuth2Operation, OAuth2Service, UserService};
use synctv_proto::client::{
    ExchangeAuthorizationCodeRequest, ExchangeAuthorizationCodeResponse,
    GetAuthorizationUrlForBindRequest, GetAuthorizationUrlForBindResponse,
    GetAuthorizationUrlRequest, GetAuthorizationUrlResponse, GetLinkedProvidersResponse,
    LinkedProvider, ListAvailableProvidersResponse, OAuth2Operation as ProtoOAuth2Operation,
    OAuth2ProviderInstance, OAuth2ProviderType, OAuth2UserInfo, UnlinkProviderRequest,
    UnlinkProviderResponse,
};

use super::ApiError;

/// `OAuth2` API implementation
#[derive(Clone)]
pub struct OAuth2ApiImpl {
    pub oauth2_service: Arc<OAuth2Service>,
    pub user_service: Arc<UserService>,
    public_id_codec: Arc<synctv_adapter::PublicIdCodec>,
}

struct UnlinkProviderPlan {
    provider_type: OAuth2Provider,
    provider_instance_name: Option<String>,
    provider_user_id: Option<String>,
    target_oauth2_identities: usize,
}

impl OAuth2ApiImpl {
    fn optional_non_empty_trimmed(value: &str) -> Option<String> {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }

    fn oauth2_provider_to_proto(provider: &OAuth2Provider) -> i32 {
        (match provider {
            OAuth2Provider::QQ => OAuth2ProviderType::Oauth2ProviderTypeQq,
            OAuth2Provider::GitHub => OAuth2ProviderType::Oauth2ProviderTypeGithub,
            OAuth2Provider::Google => OAuth2ProviderType::Oauth2ProviderTypeGoogle,
            OAuth2Provider::Microsoft => OAuth2ProviderType::Oauth2ProviderTypeMicrosoft,
            OAuth2Provider::Discord => OAuth2ProviderType::Oauth2ProviderTypeDiscord,
            OAuth2Provider::Casdoor => OAuth2ProviderType::Oauth2ProviderTypeCasdoor,
            OAuth2Provider::Logto => OAuth2ProviderType::Oauth2ProviderTypeLogto,
            OAuth2Provider::Oidc => OAuth2ProviderType::Oauth2ProviderTypeOidc,
            OAuth2Provider::Feishu => OAuth2ProviderType::Oauth2ProviderTypeFeishu,
            OAuth2Provider::Gitee => OAuth2ProviderType::Oauth2ProviderTypeGitee,
            OAuth2Provider::Apple => OAuth2ProviderType::Oauth2ProviderTypeApple,
        }) as i32
    }

    pub fn oauth2_provider_name_to_proto(provider: &str) -> Result<i32, ApiError> {
        let provider = OAuth2Provider::from_str_name(provider)
            .ok_or_else(|| ApiError::InvalidInput("Invalid OAuth2 provider type".to_string()))?;
        Ok(Self::oauth2_provider_to_proto(&provider))
    }

    fn proto_oauth2_provider(value: i32) -> Result<OAuth2Provider, ApiError> {
        match OAuth2ProviderType::try_from(value) {
            Ok(OAuth2ProviderType::Oauth2ProviderTypeQq) => Ok(OAuth2Provider::QQ),
            Ok(OAuth2ProviderType::Oauth2ProviderTypeGithub) => Ok(OAuth2Provider::GitHub),
            Ok(OAuth2ProviderType::Oauth2ProviderTypeGoogle) => Ok(OAuth2Provider::Google),
            Ok(OAuth2ProviderType::Oauth2ProviderTypeMicrosoft) => Ok(OAuth2Provider::Microsoft),
            Ok(OAuth2ProviderType::Oauth2ProviderTypeDiscord) => Ok(OAuth2Provider::Discord),
            Ok(OAuth2ProviderType::Oauth2ProviderTypeCasdoor) => Ok(OAuth2Provider::Casdoor),
            Ok(OAuth2ProviderType::Oauth2ProviderTypeLogto) => Ok(OAuth2Provider::Logto),
            Ok(OAuth2ProviderType::Oauth2ProviderTypeOidc) => Ok(OAuth2Provider::Oidc),
            Ok(OAuth2ProviderType::Oauth2ProviderTypeFeishu) => Ok(OAuth2Provider::Feishu),
            Ok(OAuth2ProviderType::Oauth2ProviderTypeGitee) => Ok(OAuth2Provider::Gitee),
            Ok(OAuth2ProviderType::Oauth2ProviderTypeApple) => Ok(OAuth2Provider::Apple),
            Ok(OAuth2ProviderType::Oauth2ProviderTypeUnspecified) | Err(_) => Err(
                ApiError::InvalidInput("OAuth2 provider type is required".to_string()),
            ),
        }
    }

    fn oauth2_operation_to_proto(operation: OAuth2Operation) -> i32 {
        (match operation {
            OAuth2Operation::Login => ProtoOAuth2Operation::Oauth2OperationLogin,
            OAuth2Operation::Bind => ProtoOAuth2Operation::Oauth2OperationBind,
        }) as i32
    }

    fn exchange_code_result_to_proto(
        result: ExchangeCodeResult,
    ) -> Result<ExchangeAuthorizationCodeResponse, ApiError> {
        if result.operation == OAuth2Operation::Bind {
            if result.access_token.is_some()
                || result.refresh_token.is_some()
                || result.user_info.is_some()
                || result.registration_review_required
                || result.registration_review_id.is_some()
            {
                return Err(ApiError::Internal(
                    "OAuth2 bind response contains login or review payload".to_string(),
                ));
            }
            return Ok(ExchangeAuthorizationCodeResponse {
                access_token: None,
                refresh_token: None,
                expires_in: 0,
                user_info: None,
                redirect_url: result.redirect_url,
                operation: Self::oauth2_operation_to_proto(result.operation),
                registration_review_required: false,
                registration_review_id: None,
            });
        }

        if result.registration_review_required {
            let registration_review_id = result.registration_review_id.ok_or_else(|| {
                ApiError::Internal("OAuth2 review response is missing review id".to_string())
            })?;
            if result.access_token.is_some()
                || result.refresh_token.is_some()
                || result.user_info.is_some()
            {
                return Err(ApiError::Internal(
                    "OAuth2 review response contains login tokens".to_string(),
                ));
            }
            return Ok(ExchangeAuthorizationCodeResponse {
                access_token: None,
                refresh_token: None,
                expires_in: 0,
                user_info: None,
                redirect_url: result.redirect_url,
                operation: Self::oauth2_operation_to_proto(result.operation),
                registration_review_required: true,
                registration_review_id: Some(registration_review_id),
            });
        }

        let access_token = result.access_token.ok_or_else(|| {
            ApiError::Internal("OAuth2 login response is missing access token".to_string())
        })?;
        let refresh_token = result.refresh_token.ok_or_else(|| {
            ApiError::Internal("OAuth2 login response is missing refresh token".to_string())
        })?;
        let user_info = result.user_info.ok_or_else(|| {
            ApiError::Internal("OAuth2 login response is missing user info".to_string())
        })?;

        Ok(ExchangeAuthorizationCodeResponse {
            access_token: Some(access_token),
            refresh_token: Some(refresh_token),
            expires_in: result.expires_in,
            user_info: Some(user_info),
            redirect_url: result.redirect_url,
            operation: Self::oauth2_operation_to_proto(result.operation),
            registration_review_required: false,
            registration_review_id: None,
        })
    }

    fn oauth2_identity_unlink_counts(
        linked_mappings: &[synctv_core::models::oauth2_client::UserOAuthProviderMapping],
        provider_type: &OAuth2Provider,
        provider_instance_name: Option<&str>,
        provider_user_id: Option<&str>,
    ) -> (usize, usize) {
        linked_mappings
            .iter()
            .fold((0_usize, 0_usize), |counts, mapping| {
                let (mut target, mut remaining) = counts;
                let same_provider = &mapping.provider == provider_type;
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
        active_provider_keys: &HashSet<(String, OAuth2Provider)>,
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

    async fn plan_unlink_provider(
        &self,
        user_id: &UserId,
        provider: &str,
        provider_instance_name: Option<&str>,
        provider_user_id: Option<&str>,
    ) -> Result<UnlinkProviderPlan, ApiError> {
        let provider_type = OAuth2Provider::from_str_name(provider)
            .ok_or_else(|| ApiError::InvalidInput(format!("Unknown provider type: {provider}")))?;
        let provider_instance_name = provider_instance_name
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let provider_user_id = provider_user_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        if provider_user_id.is_some() && provider_instance_name.is_none() {
            return Err(ApiError::InvalidInput(
                "provider_instance_name is required when provider_user_id is set".to_string(),
            ));
        }

        let (linked_mappings, active_provider_keys) = tokio::join!(
            self.oauth2_service.get_user_provider_mappings(user_id),
            self.active_oauth2_provider_keys(),
        );
        let linked_mappings = linked_mappings.map_err(ApiError::from)?;
        let active_provider_keys = active_provider_keys?;
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
                provider_instance_name.as_deref(),
                provider_user_id.as_deref(),
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

        Ok(UnlinkProviderPlan {
            provider_type,
            provider_instance_name,
            provider_user_id,
            target_oauth2_identities,
        })
    }

    async fn execute_unlink_provider(
        &self,
        user_id: &UserId,
        plan: UnlinkProviderPlan,
    ) -> Result<UnlinkResult, ApiError> {
        let removed = if let Some(provider_user_id) = plan.provider_user_id.as_deref() {
            let provider_instance_name =
                plan.provider_instance_name.as_deref().ok_or_else(|| {
                    ApiError::InvalidInput(
                        "provider_instance_name is required when provider_user_id is set"
                            .to_string(),
                    )
                })?;
            self.oauth2_service
                .unlink_provider(user_id, provider_instance_name, provider_user_id)
                .await
                .map_err(ApiError::from)?
        } else {
            self.oauth2_service
                .unlink_provider_all(user_id, &plan.provider_type)
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
            removed_count: i32::try_from(plan.target_oauth2_identities).map_err(|_| {
                ApiError::Internal("removed OAuth2 identity count exceeds i32::MAX".to_string())
            })?,
        })
    }

    async fn active_oauth2_provider_keys(
        &self,
    ) -> Result<HashSet<(String, OAuth2Provider)>, ApiError> {
        Ok(self
            .oauth2_service
            .list_available_instances()
            .await
            .map_err(ApiError::from)?
            .into_iter()
            .map(|(name, provider, _)| (name, provider))
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
        public_id_codec: Arc<synctv_adapter::PublicIdCodec>,
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
            operation: Self::oauth2_operation_to_proto(OAuth2Operation::Login),
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
        self.user_service
            .get_user(user_id)
            .await
            .map_err(Self::map_bind_user_lookup_error)
            .and_then(|user| {
                if user.is_deleted() || user.status == UserStatus::Banned {
                    Err(ApiError::Authentication(
                        "Authentication failed".to_string(),
                    ))
                } else {
                    Ok(())
                }
            })?;
        let prepared = self
            .oauth2_service
            .prepare_authorization_url_with_control(
                &req.provider,
                redirect_url,
                OAuth2Operation::Bind,
                Some(*user_id),
                control,
            )
            .await
            .map_err(ApiError::from)?;
        self.user_service
            .consume_sensitive_operation_verification(user_id, &req.verification_id)
            .await
            .map_err(ApiError::from)?;
        self.oauth2_service
            .store_prepared_authorization_with_control(&prepared, control)
            .await
            .map_err(ApiError::from)?;

        Ok(GetAuthorizationUrlForBindResponse {
            authorization_url: prepared.auth_url,
            state: prepared.state_token,
            operation: Self::oauth2_operation_to_proto(prepared.oauth_state.operation),
        })
    }

    /// Exchange authorization code for JWT token
    ///
    /// Frontend calls this after receiving code and state from `OAuth2` provider redirect.
    ///
    /// For login flow (no `target_user_id` in state):
    /// - If user exists: log them in
    /// - If user doesn't exist: create new user account
    ///
    /// For bind flow (`target_user_id` present in state):
    /// - Binds the `OAuth2` provider to the existing user account
    /// - Returns empty tokens (user is already logged in)
    ///
    /// The `current_user_id` parameter is required for the bind flow to verify that
    /// only the intended user (the one who initiated the bind) can complete it.
    /// Pass `None` for login-only flows (no authentication needed).
    pub async fn exchange_authorization_code(
        &self,
        code: &str,
        state: &str,
        current_user_id: Option<&UserId>,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<ExchangeCodeResult, ApiError> {
        self.exchange_authorization_code_with_control(code, state, current_user_id, client_ip, None)
            .await
    }

    pub async fn exchange_authorization_code_with_control(
        &self,
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
            .map_err(|error| ApiError::OAuth2InvalidState {
                message: ApiError::from(error).message().to_string(),
            })?;
        let operation = oauth_state.operation;
        let provider_instance_name = oauth_state.instance_name.clone();

        // 2. Exchange code for user info using PKCE verifier from stored state
        let user_info = self
            .oauth2_service
            .exchange_code_for_user_info_with_state_and_control(
                &provider_instance_name,
                code,
                &oauth_state,
                control,
            )
            .await
            .map_err(|error| ApiError::OAuth2ProviderExchangeFailed {
                operation,
                message: ApiError::from(error).message().to_string(),
            })?;

        // 3. Route by the operation captured in the single-use OAuth2 state.
        if operation == OAuth2Operation::Bind {
            let target_user_id =
                oauth_state
                    .target_user_id
                    .ok_or_else(|| ApiError::OAuth2MissingTargetUser {
                        operation,
                        message: "OAuth2 bind state is missing user id".to_string(),
                    })?;
            // Bind flow: verify that the currently authenticated user matches
            // the user who initiated the bind request. This prevents a malicious
            // actor from completing another user's OAuth2 bind by replaying the
            // state token.
            if current_user_id != Some(&target_user_id) {
                return Err(ApiError::OAuth2TargetUserMismatch {
                    operation,
                    message: "Cannot bind OAuth2 to another user's account".to_string(),
                });
            }

            // Check if this provider account is already linked to a different user.
            // Silently reassigning would steal the linkage from the other user.
            if let Some(existing_user_id) = self
                .oauth2_service
                .find_user_by_provider_instance(
                    &provider_instance_name,
                    &user_info.provider_user_id,
                )
                .await
                .map_err(|error| {
                    let api_error = ApiError::from(error);
                    ApiError::OAuth2ProviderLookupFailed {
                        operation,
                        kind: api_error.classify(),
                        message: api_error.message().to_string(),
                    }
                })?
            {
                if existing_user_id != target_user_id {
                    return Err(ApiError::OAuth2ProviderAccountLinkedElsewhere {
                        operation,
                        message: "This provider account is already linked to another user"
                            .to_string(),
                    });
                }
            }

            // Bind flow: associate provider with existing user
            self.oauth2_service
                .upsert_user_provider(&target_user_id, &user_info)
                .await
                .map_err(|error| {
                    let api_error = ApiError::from(error);
                    ApiError::OAuth2ProviderLinkFailed {
                        operation,
                        kind: api_error.classify(),
                        message: api_error.message().to_string(),
                    }
                })?;

            return Ok(ExchangeCodeResult {
                access_token: None,
                refresh_token: None,
                expires_in: 0,
                user_info: None,
                redirect_url: oauth_state.redirect_url,
                operation,
                registration_review_required: false,
                registration_review_id: None,
            });
        }
        if oauth_state.target_user_id.is_some() {
            return Err(ApiError::OAuth2UnexpectedTargetUser {
                operation,
                message: "OAuth2 login state contains target user id".to_string(),
            });
        }

        // Login flow: find or create user
        let user_id = self
            .oauth2_service
            .find_user_by_provider_instance(&provider_instance_name, &user_info.provider_user_id)
            .await
            .map_err(|error| {
                let api_error = ApiError::from(error);
                ApiError::OAuth2ProviderLookupFailed {
                    operation,
                    kind: api_error.classify(),
                    message: api_error.message().to_string(),
                }
            })?;

        let login = if let Some(user_id) = user_id {
            // User exists - generate tokens using OAuth2 login method
            // (user already authenticated by OAuth2 provider)
            self.user_service
                .login_oauth2_with_control(
                    &user_id,
                    &provider_instance_name,
                    &user_info.provider_user_id,
                    client_ip,
                    control,
                )
                .await
                .map_err(|error| {
                    let api_error = ApiError::from(error);
                    ApiError::OAuth2LoginFailed {
                        operation,
                        kind: api_error.classify(),
                        message: api_error.message().to_string(),
                    }
                })?
        } else {
            match self
                .oauth2_service
                .find_or_create_and_link(&provider_instance_name, &user_info)
                .await
                .map_err(|error| {
                    let api_error = ApiError::from(error);
                    ApiError::OAuth2ProviderLinkFailed {
                        operation,
                        kind: api_error.classify(),
                        message: api_error.message().to_string(),
                    }
                })? {
                OAuth2LinkResult::Linked { user_id, .. } => self
                    .user_service
                    .login_oauth2_with_control(
                        &user_id,
                        &provider_instance_name,
                        &user_info.provider_user_id,
                        client_ip,
                        control,
                    )
                    .await
                    .map_err(|error| {
                        let api_error = ApiError::from(error);
                        ApiError::OAuth2LoginFailed {
                            operation,
                            kind: api_error.classify(),
                            message: api_error.message().to_string(),
                        }
                    })?,
                OAuth2LinkResult::PendingReview(pending) => {
                    return Ok(ExchangeCodeResult {
                        access_token: None,
                        refresh_token: None,
                        expires_in: 0,
                        user_info: None,
                        redirect_url: oauth_state.redirect_url,
                        operation,
                        registration_review_required: true,
                        registration_review_id: Some(
                            self.public_id_codec
                                .encode_user_id(pending.request_id)
                                .map_err(|message| ApiError::OAuth2ResponseBuildFailed {
                                    operation,
                                    message,
                                })?,
                        ),
                    });
                }
            }
        };

        let expires_in = self
            .user_service
            .access_token_duration_seconds()
            .map_err(|error| {
                let api_error = ApiError::from(error);
                ApiError::OAuth2LoginFailed {
                    operation,
                    kind: api_error.classify(),
                    message: api_error.message().to_string(),
                }
            })?;

        match login {
            synctv_core::service::AuthenticatedLogin::Complete {
                user,
                email: _,
                access_token,
                refresh_token,
            } => Ok(ExchangeCodeResult {
                access_token: Some(access_token),
                refresh_token: Some(refresh_token),
                expires_in,
                user_info: Some(
                    user_to_oauth2_user_info(&user, &self.public_id_codec).map_err(|error| {
                        ApiError::OAuth2ResponseBuildFailed {
                            operation,
                            message: error.message().to_string(),
                        }
                    })?,
                ),
                redirect_url: oauth_state.redirect_url,
                operation,
                registration_review_required: false,
                registration_review_id: None,
            }),
            synctv_core::service::AuthenticatedLogin::MfaRequired { .. } => {
                Err(ApiError::OAuth2LoginFailed {
                    operation,
                    kind: super::ErrorKind::Internal,
                    message: "OAuth2 must not start a two-factor authentication session"
                        .to_string(),
                })
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
                &req.code,
                &req.state,
                current_user_id,
                client_ip,
                control,
            )
            .await?;
        let operation = result.operation;

        Self::exchange_code_result_to_proto(result).map_err(|error| {
            ApiError::OAuth2ResponseBuildFailed {
                operation,
                message: error.message().to_string(),
            }
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
                provider_type,
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
        let plan = self
            .plan_unlink_provider(user_id, provider, provider_instance_name, provider_user_id)
            .await?;
        self.execute_unlink_provider(user_id, plan).await
    }

    pub async fn unlink_provider_response(
        &self,
        user_id: &UserId,
        req: UnlinkProviderRequest,
    ) -> Result<UnlinkProviderResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let provider_user_id = Self::optional_non_empty_trimmed(&req.provider_user_id);
        let provider_instance_name = Self::optional_non_empty_trimmed(&req.provider_instance_name);
        let provider = Self::proto_oauth2_provider(req.provider)?;
        let plan = self
            .plan_unlink_provider(
                user_id,
                provider.as_str(),
                provider_instance_name.as_deref(),
                provider_user_id.as_deref(),
            )
            .await?;
        self.user_service
            .consume_sensitive_operation_verification(user_id, &req.verification_id)
            .await
            .map_err(ApiError::from)?;
        let result = self.execute_unlink_provider(user_id, plan).await?;

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
        let (mappings, available) = tokio::join!(
            self.oauth2_service.get_user_provider_mappings(user_id),
            self.active_oauth2_provider_keys(),
        );
        let mappings = mappings.map_err(ApiError::from)?;
        let available = available?;

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
                provider_issuer: mapping.provider_issuer,
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
    pub operation: OAuth2Operation,
    pub registration_review_required: bool,
    pub registration_review_id: Option<String>,
}

/// `OAuth2` provider information
pub struct ProviderInfo {
    pub name: String,
    pub provider_type: OAuth2Provider,
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
    pub provider_type: OAuth2Provider,
    pub provider_instance_name: String,
    pub provider_issuer: Option<String>,
    pub provider_user_id: String,
    pub provider_username: String,
    pub linked_at: i64, // Unix timestamp (seconds)
}

/// Convert User model to `OAuth2UserInfo` proto
fn user_to_oauth2_user_info(
    user: &User,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<OAuth2UserInfo, ApiError> {
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

    Ok(OAuth2UserInfo {
        user_id: public_id_codec
            .encode_user_id(user.id)
            .map_err(ApiError::InvalidInput)?,
        username: user.username.clone(),
        avatar: None,
        role: proto_role as i32,
        status: proto_status as i32,
        created_at: user.created_at.timestamp(),
    })
}

/// Convert proto `OAuth2ProviderInstance` to `ProviderInfo`
impl From<ProviderInfo> for OAuth2ProviderInstance {
    fn from(info: ProviderInfo) -> Self {
        Self {
            name: info.name,
            r#type: OAuth2ApiImpl::oauth2_provider_to_proto(&info.provider_type),
            signup_enabled: info.signup_enabled,
            signup_need_review: info.signup_need_review,
        }
    }
}

/// Convert `LinkedProviderInfo` to proto `LinkedProvider`
impl From<LinkedProviderInfo> for LinkedProvider {
    fn from(info: LinkedProviderInfo) -> Self {
        Self {
            provider_type: OAuth2ApiImpl::oauth2_provider_to_proto(&info.provider_type),
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
        GetAuthorizationUrlRequest, OAuth2UserInfo, UnlinkProviderRequest,
    };

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn api_ok<T>(result: Result<T, ApiError>) -> TestResult<T> {
        result.map_err(|error| test_error(format!("{error:?}")))
    }

    fn oauth_user_info() -> OAuth2UserInfo {
        OAuth2UserInfo {
            user_id: "user_1".to_string(),
            username: "alice".to_string(),
            avatar: None,
            role: synctv_proto::common::UserRole::User as i32,
            status: synctv_proto::common::UserStatus::Active as i32,
            created_at: 1_700_000_000,
        }
    }

    #[test]
    fn exchange_code_result_to_proto_returns_login_payload() -> TestResult {
        let response = api_ok(super::OAuth2ApiImpl::exchange_code_result_to_proto(
            super::ExchangeCodeResult {
                access_token: Some("access".to_string()),
                refresh_token: Some("refresh".to_string()),
                expires_in: 3600,
                user_info: Some(oauth_user_info()),
                redirect_url: Some("https://app.example.test/callback".to_string()),
                operation: synctv_core::service::OAuth2Operation::Login,
                registration_review_required: false,
                registration_review_id: None,
            },
        ))?;

        assert_eq!(response.access_token.as_deref(), Some("access"));
        assert_eq!(response.refresh_token.as_deref(), Some("refresh"));
        assert!(response.user_info.is_some());
        assert_eq!(
            response.redirect_url.as_deref(),
            Some("https://app.example.test/callback")
        );
        Ok(())
    }

    #[test]
    fn exchange_code_result_to_proto_returns_bind_payload() -> TestResult {
        let response = api_ok(super::OAuth2ApiImpl::exchange_code_result_to_proto(
            super::ExchangeCodeResult {
                access_token: None,
                refresh_token: None,
                expires_in: 0,
                user_info: None,
                redirect_url: Some("https://app.example.test/bound".to_string()),
                operation: synctv_core::service::OAuth2Operation::Bind,
                registration_review_required: false,
                registration_review_id: None,
            },
        ))?;

        assert!(response.access_token.is_none());
        assert!(response.refresh_token.is_none());
        assert!(response.user_info.is_none());
        assert_eq!(
            response.operation,
            synctv_proto::client::OAuth2Operation::Oauth2OperationBind as i32
        );
        Ok(())
    }

    #[test]
    fn exchange_code_result_to_proto_returns_review_payload() -> TestResult {
        let response = api_ok(super::OAuth2ApiImpl::exchange_code_result_to_proto(
            super::ExchangeCodeResult {
                access_token: None,
                refresh_token: None,
                expires_in: 0,
                user_info: None,
                redirect_url: None,
                operation: synctv_core::service::OAuth2Operation::Login,
                registration_review_required: true,
                registration_review_id: Some("review_1".to_string()),
            },
        ))?;

        assert!(response.access_token.is_none());
        assert!(response.refresh_token.is_none());
        assert!(response.registration_review_required);
        assert_eq!(response.registration_review_id.as_deref(), Some("review_1"));
        Ok(())
    }

    #[test]
    fn exchange_code_result_to_proto_rejects_incomplete_login_payload() {
        let error =
            super::OAuth2ApiImpl::exchange_code_result_to_proto(super::ExchangeCodeResult {
                access_token: None,
                refresh_token: Some("refresh".to_string()),
                expires_in: 3600,
                user_info: Some(oauth_user_info()),
                redirect_url: None,
                operation: synctv_core::service::OAuth2Operation::Login,
                registration_review_required: false,
                registration_review_id: None,
            })
            .expect_err("login response without access token should fail");

        assert!(matches!(error, ApiError::Internal(message) if message.contains("access token")));
    }

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
            "missing target users should still be treated as authentication failure, got: {mapped:?}"
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
            verification_id: "verification-id".to_string(),
        })
        .expect_err("custom scheme redirect URL must be rejected");

        assert!(err.to_string().contains("redirect_url"));
    }

    #[test]
    fn test_oauth2_request_validation_rejects_invalid_exchange_code() {
        let err = crate::impls::validate_proto_request(&ExchangeAuthorizationCodeRequest {
            code: "code with spaces".to_string(),
            state: "AbCdEfGh1234567890aBcDeFgHiJkLm".to_string(),
        })
        .expect_err("invalid code must be rejected");

        assert!(err.to_string().contains("code"));
    }

    #[test]
    fn test_oauth2_request_validation_rejects_invalid_exchange_state() {
        let err = crate::impls::validate_proto_request(&ExchangeAuthorizationCodeRequest {
            code: "code.with.dots".to_string(),
            state: "short".to_string(),
        })
        .expect_err("invalid state must be rejected");

        assert!(err.to_string().contains("state"));
    }

    #[test]
    fn test_oauth2_request_validation_rejects_too_long_provider_user_id() {
        let err = crate::impls::validate_proto_request(&UnlinkProviderRequest {
            provider: synctv_proto::client::OAuth2ProviderType::Oauth2ProviderTypeGithub as i32,
            provider_user_id: "a".repeat(257),
            provider_instance_name: "github-main".to_string(),
            verification_id: "verification-id".to_string(),
        })
        .expect_err("overlong provider_user_id must be rejected");

        assert!(err.to_string().contains("provider_user_id"));
    }

    #[test]
    fn test_oauth2_unlink_counts_specific_identity_without_removing_same_provider_siblings() {
        use synctv_core::models::oauth2_client::{OAuth2Provider, UserOAuthProviderMapping};
        use synctv_core::models::UserId;

        let now = synctv_core::SystemClock.now();
        let mappings = vec![
            UserOAuthProviderMapping {
                id: 1,
                provider: OAuth2Provider::GitHub,
                provider_instance_name: "github-main".to_string(),
                provider_issuer: Some("https://github.com".to_string()),
                provider_user_id: "github-a".to_string(),
                user_id: UserId::expect_positive(42),
                username: "github-a".to_string(),
                avatar_url: None,
                created_at: now,
                updated_at: now,
            },
            UserOAuthProviderMapping {
                id: 2,
                provider: OAuth2Provider::GitHub,
                provider_instance_name: "github-backup".to_string(),
                provider_issuer: Some("https://github.example.com".to_string()),
                provider_user_id: "github-b".to_string(),
                user_id: UserId::expect_positive(42),
                username: "github-b".to_string(),
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

        let now = synctv_core::SystemClock.now();
        let mappings = vec![
            UserOAuthProviderMapping {
                id: 1,
                provider: OAuth2Provider::GitHub,
                provider_instance_name: "github-main".to_string(),
                provider_issuer: Some("https://github.com".to_string()),
                provider_user_id: "github-a".to_string(),
                user_id: UserId::expect_positive(42),
                username: "github-a".to_string(),
                avatar_url: None,
                created_at: now,
                updated_at: now,
            },
            UserOAuthProviderMapping {
                id: 2,
                provider: OAuth2Provider::GitHub,
                provider_instance_name: "github-backup".to_string(),
                provider_issuer: Some("https://github.example.com".to_string()),
                provider_user_id: "github-b".to_string(),
                user_id: UserId::expect_positive(42),
                username: "github-b".to_string(),
                avatar_url: None,
                created_at: now,
                updated_at: now,
            },
            UserOAuthProviderMapping {
                id: 3,
                provider: OAuth2Provider::Google,
                provider_instance_name: "google".to_string(),
                provider_issuer: Some("https://accounts.google.com".to_string()),
                provider_user_id: "google-a".to_string(),
                user_id: UserId::expect_positive(42),
                username: "google-a".to_string(),
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
        use synctv_core::models::{OAuth2Provider, UserId};

        let now = synctv_core::SystemClock.now();
        let mappings = vec![
            UserOAuthProviderMapping {
                id: 1,
                provider: OAuth2Provider::GitHub,
                provider_instance_name: "github-main".to_string(),
                provider_issuer: Some("https://github.com".to_string()),
                provider_user_id: "github-a".to_string(),
                user_id: UserId::expect_positive(42),
                username: "github-a".to_string(),
                avatar_url: None,
                created_at: now,
                updated_at: now,
            },
            UserOAuthProviderMapping {
                id: 2,
                provider: OAuth2Provider::Google,
                provider_instance_name: "removed-google".to_string(),
                provider_issuer: Some("https://accounts.google.com".to_string()),
                provider_user_id: "google-a".to_string(),
                user_id: UserId::expect_positive(42),
                username: "google-a".to_string(),
                avatar_url: None,
                created_at: now,
                updated_at: now,
            },
        ];
        let active = HashSet::from([("github-main".to_string(), OAuth2Provider::GitHub)]);

        let filtered = super::OAuth2ApiImpl::active_oauth2_mappings(&mappings, &active);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].provider_instance_name, "github-main");
    }
}
