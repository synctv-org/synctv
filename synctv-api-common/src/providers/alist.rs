//! Alist API Implementation
//!
//! Unified implementation for all Alist API operations.
//! Used by both HTTP and gRPC handlers.

use std::sync::Arc;
use synctv_core::models::UserId;
use synctv_core::provider::{
    AlistListRequest, AlistMeRequest, AlistProvider, AlistSearchRequest, ExecutionControl,
    ProviderAccessService,
};
use synctv_proto::providers::alist::{
    BindInfo, FileItem, GetBindsResponse, GetMeRequest, GetMeResponse, ListRequest, ListResponse,
    LoginRequest, LoginResponse, LogoutRequest, LogoutResponse, SearchItem, SearchRequest,
    SearchResponse,
};
use synctv_realtime::fanout::RealtimeEventService;

use super::{
    provider_instance_name_for_response, publish_provider_credential_changed,
    resolve_bound_instance_name,
};

/// Alist API implementation
///
/// Contains all business logic for Alist operations.
/// Methods accept grpc-generated request types and return grpc-generated response types.
#[derive(Clone)]
pub struct AlistApiImpl {
    provider: Arc<AlistProvider>,
    access_service: Arc<dyn ProviderAccessService>,
    event_service: Arc<dyn RealtimeEventService>,
}

#[derive(Clone)]
pub struct ProviderApiRuntime {
    pub access_service: Arc<dyn ProviderAccessService>,
    pub event_service: Arc<dyn RealtimeEventService>,
}

impl AlistApiImpl {
    #[must_use]
    pub fn new_with_runtime(provider: Arc<AlistProvider>, runtime: ProviderApiRuntime) -> Self {
        Self {
            provider,
            access_service: runtime.access_service,
            event_service: runtime.event_service,
        }
    }

    /// Resolve Alist credentials from DB using server_id.
    ///
    /// Re-authenticates with stored username/password to obtain a fresh API token.
    async fn resolve_credentials(
        &self,
        caller_user_id: &UserId,
        server_id: &str,
        request_context: Option<&ExecutionControl>,
    ) -> Result<(String, String, Option<String>), synctv_core::provider::ProviderError> {
        let access = self
            .access_service
            .alist_access(*caller_user_id, server_id, None, request_context)
            .await?;
        Ok((access.host, access.token, access.provider_instance_name))
    }

    pub async fn login_with_context(
        &self,
        caller_user_id: &UserId,
        req: LoginRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<LoginResponse, synctv_core::provider::ProviderError> {
        let host = req.host.clone();
        let (password_input, hashed_password_input) = match req.credential {
            Some(synctv_proto::providers::alist::login_request::Credential::Password(password)) => {
                (Some(password), None)
            }
            Some(synctv_proto::providers::alist::login_request::Credential::HashedPassword(
                hashed_password,
            )) => (None, Some(hashed_password)),
            None => (None, None),
        };
        let trimmed_otp_secret = req.otp_secret.trim();
        let login = self
            .provider
            .login_and_persist_with_context(
                synctv_core::provider::AlistLoginAndPersistRequest {
                    user_id: *caller_user_id,
                    host,
                    username: req.username,
                    password: password_input,
                    hashed_password: hashed_password_input,
                    otp_code: req.otp_code,
                    otp_secret: (!trimmed_otp_secret.is_empty())
                        .then_some(trimmed_otp_secret.to_string()),
                    provider_instance_name: instance_name.map(ToString::to_string),
                },
                request_context,
            )
            .await?;

        self.access_service
            .invalidate(
                *caller_user_id,
                synctv_core::provider::AlistProvider::NAME,
                &login.server_id,
            )
            .await?;
        publish_provider_credential_changed(
            &self.event_service,
            *caller_user_id,
            synctv_core::provider::AlistProvider::NAME,
            &login.server_id,
        );

        Ok(LoginResponse {
            token: login.token,
            server_id: login.server_id,
        })
    }

    pub async fn list_with_context(
        &self,
        caller_user_id: &UserId,
        req: ListRequest,
        requested_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<ListResponse, synctv_core::provider::ProviderError> {
        let (host, token, credential_instance_name) = self
            .resolve_credentials(caller_user_id, &req.server_id, request_context)
            .await?;
        let effective_instance_name = resolve_bound_instance_name(
            requested_instance_name,
            credential_instance_name.as_deref(),
        )?;

        let list_req = AlistListRequest {
            host,
            token,
            path: req.path,
            password: req.password,
            page: req.page,
            per_page: req.per_page,
            refresh: req.refresh,
        };

        let resp = self
            .provider
            .fs_list_with_context(
                list_req,
                effective_instance_name.as_deref(),
                request_context,
            )
            .await?;

        let content: Vec<FileItem> = resp
            .content
            .into_iter()
            .map(|item| FileItem {
                name: item.name,
                size: item.size,
                is_dir: item.is_dir,
                modified: item.modified,
                sign: item.sign,
                thumb: item.thumb,
                r#type: item.item_type,
            })
            .collect();

        Ok(ListResponse {
            content,
            total: resp.total,
        })
    }

    pub async fn search_with_context(
        &self,
        caller_user_id: &UserId,
        req: SearchRequest,
        requested_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<SearchResponse, synctv_core::provider::ProviderError> {
        if req.keywords.trim().is_empty() {
            return Err(synctv_core::provider::ProviderError::InvalidConfig(
                "Alist search keywords must not be empty".to_string(),
            ));
        }

        let (host, token, credential_instance_name) = self
            .resolve_credentials(caller_user_id, &req.server_id, request_context)
            .await?;
        let effective_instance_name = resolve_bound_instance_name(
            requested_instance_name,
            credential_instance_name.as_deref(),
        )?;

        let search_req = AlistSearchRequest {
            host,
            token,
            parent: req.parent,
            keywords: req.keywords,
            scope: req.scope,
            page: req.page.max(1),
            per_page: req.per_page.max(1),
            password: req.password,
        };

        let resp = self
            .provider
            .fs_search_with_context(
                search_req,
                effective_instance_name.as_deref(),
                request_context,
            )
            .await?;

        let content = resp
            .content
            .into_iter()
            .map(|item| SearchItem {
                parent: item.parent,
                name: item.name,
                is_dir: item.is_dir,
                size: item.size,
                r#type: item.item_type,
            })
            .collect();

        Ok(SearchResponse {
            content,
            total: resp.total,
        })
    }

    pub async fn get_me_with_context(
        &self,
        caller_user_id: &UserId,
        req: GetMeRequest,
        requested_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<GetMeResponse, synctv_core::provider::ProviderError> {
        let (host, token, credential_instance_name) = self
            .resolve_credentials(caller_user_id, &req.server_id, request_context)
            .await?;
        let effective_instance_name = resolve_bound_instance_name(
            requested_instance_name,
            credential_instance_name.as_deref(),
        )?;

        let me_req = AlistMeRequest { host, token };

        let resp = self
            .provider
            .me_with_context(me_req, effective_instance_name.as_deref(), request_context)
            .await?;

        Ok(GetMeResponse {
            username: resp.username,
            base_path: resp.base_path,
        })
    }

    /// Logout and delete stored credential
    pub async fn logout(
        &self,
        caller_user_id: &UserId,
        req: LogoutRequest,
    ) -> Result<LogoutResponse, synctv_core::provider::ProviderError> {
        if req.server_id.trim().is_empty() {
            return Err(synctv_core::provider::ProviderError::InvalidConfig(
                "Alist logout requires an explicit server_id".to_string(),
            ));
        }

        if self
            .provider
            .delete_credential(*caller_user_id, &req.server_id)
            .await?
        {
            self.access_service
                .invalidate(
                    *caller_user_id,
                    synctv_core::provider::AlistProvider::NAME,
                    &req.server_id,
                )
                .await?;
            publish_provider_credential_changed(
                &self.event_service,
                *caller_user_id,
                synctv_core::provider::AlistProvider::NAME,
                &req.server_id,
            );
        }

        Ok(LogoutResponse {
            message: "Logout successful".to_string(),
        })
    }

    pub async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<GetBindsResponse, crate::impls::ApiError> {
        let binds = self
            .provider
            .list_binds(*caller_user_id, instance_name)
            .await?
            .into_iter()
            .map(|bind| BindInfo {
                id: bind.id.to_string(),
                server_id: bind.server_id,
                host: bind.host,
                username: bind.username,
                created_at: bind.created_at,
                provider_instance_name: provider_instance_name_for_response(
                    bind.provider_instance_name,
                ),
            })
            .collect();

        Ok(GetBindsResponse { binds })
    }
}

#[cfg(test)]
mod tests {
    use super::{AlistApiImpl, ProviderApiRuntime};
    use std::sync::Arc;
    use synctv_core::provider::{AlistProvider, ProviderError};
    use synctv_core::repository::{ProviderInstanceRepository, UserProviderCredentialRepository};
    use synctv_core_testing::create_test_pool;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn provider_ok<T>(result: Result<T, ProviderError>) -> TestResult<T> {
        result.map_err(|error| test_error(error.to_string()))
    }

    fn invalid_config<T>(result: Result<T, ProviderError>) -> TestResult<String> {
        match result {
            Ok(_) => Err(test_error("expected provider invalid config")),
            Err(ProviderError::InvalidConfig(message)) => Ok(message),
            Err(other) => Err(test_error(format!("expected InvalidConfig, got {other:?}"))),
        }
    }

    fn test_api(pool: sqlx::PgPool) -> TestResult<AlistApiImpl> {
        let instance_manager = Arc::new(synctv_core::service::RemoteProviderManager::new(
            Arc::new(ProviderInstanceRepository::new(pool.clone())),
        ));
        let provider = Arc::new(AlistProvider::with_client_manager(
            instance_manager,
            Arc::new(synctv_core::provider::ProviderClientManager::new()?),
        ));
        let credential_repo = Arc::new(UserProviderCredentialRepository::new(pool));
        let runtime = ProviderApiRuntime {
            access_service: Arc::new(synctv_core::provider::CachedProviderAccessService::new(
                credential_repo.clone(),
                provider.clone(),
            )),
            event_service: Arc::new(synctv_realtime::fanout::LocalNoopRealtimeEventService::new()),
        };
        Ok(AlistApiImpl::new_with_runtime(
            Arc::new(provider.with_credential_repo(credential_repo)),
            runtime,
        ))
    }

    #[test]
    fn resolve_login_credential_rejects_missing_password_and_hash() -> TestResult {
        let message = invalid_config(AlistProvider::resolve_login_credential(None, None))?;

        assert!(message.contains("exactly one credential"));
        Ok(())
    }

    #[test]
    fn resolve_login_credential_accepts_plaintext_password_without_hash() -> TestResult {
        let (credential, hashed) = provider_ok(AlistProvider::resolve_login_credential(
            Some("secret123"),
            None,
        ))?;

        assert_eq!(credential, "secret123");
        assert!(!hashed);
        Ok(())
    }

    #[test]
    fn resolve_login_credential_accepts_hashed_password_without_plaintext() -> TestResult {
        let (credential, hashed) = provider_ok(AlistProvider::resolve_login_credential(
            None,
            Some("sha256:abc123"),
        ))?;

        assert_eq!(credential, "sha256:abc123");
        assert!(hashed);
        Ok(())
    }

    #[test]
    fn hash_password_for_storage_matches_current_alist_hash_endpoint_contract() {
        assert_eq!(
            AlistProvider::hash_password_for_storage("kaR6YeYA"),
            "6a977a872a0c445d98fc3d34634705b98716d89d7491637f0ce2f3cb6e5d4d31"
        );
    }

    #[test]
    fn resolve_alist_login_otp_prefers_explicit_code_over_secret() -> TestResult {
        let code = provider_ok(AlistProvider::resolve_login_otp_code(
            "654321",
            Some("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"),
        ))?;

        assert_eq!(code, "654321");
        Ok(())
    }

    #[test]
    fn resolve_alist_login_otp_generates_code_from_secret_when_code_missing() -> TestResult {
        let code = provider_ok(AlistProvider::resolve_login_otp_code(
            "",
            Some("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"),
        ))?;

        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|ch| ch.is_ascii_digit()));
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn logout_rejects_empty_server_id() -> TestResult {
        let (_postgres, pool) = create_test_pool().await;
        let api = test_api(pool)?;

        let message = invalid_config(
            api.logout(
                &synctv_core::models::UserId::new(),
                synctv_proto::providers::alist::LogoutRequest {
                    server_id: String::new(),
                    instance_name: String::new(),
                },
            )
            .await,
        )?;

        assert!(message.contains("explicit server_id"));
        Ok(())
    }
}
