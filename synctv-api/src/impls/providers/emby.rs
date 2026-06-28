//! Emby API Implementation
//!
//! Unified implementation for all Emby API operations.
//! Used by both HTTP and gRPC handlers.

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use std::sync::Arc;
use synctv_core::models::{ProviderCredential, UserId, UserProviderCredential};
use synctv_core::provider::{EmbyProvider, ExecutionControl, ProviderAccessService};
use synctv_core::repository::UserProviderCredentialRepository;
use synctv_proto::providers::emby::{
    BindInfo, GetBindsResponse, GetMeRequest, GetMeResponse, ListRequest, ListResponse,
    LoginRequest, LoginResponse, LogoutRequest, LogoutResponse, MediaItem,
};

use super::ProviderApiRuntime;
use super::{get_provider_binds, publish_provider_credential_changed, resolve_bound_instance_name};

fn emby_thumbnail_url(server_id: &str, credential_owner_id: &UserId, item_id: &str) -> String {
    format!(
        "/api/providers/emby/thumbnail/{item_id}?serverId={server_id}&credentialOwnerId={credential_owner_id}&maxHeight=300",
        item_id = utf8_percent_encode(item_id, NON_ALPHANUMERIC),
        server_id = utf8_percent_encode(server_id, NON_ALPHANUMERIC),
        credential_owner_id = utf8_percent_encode(&credential_owner_id.to_string(), NON_ALPHANUMERIC),
    )
}

/// Emby API implementation
///
/// Contains all business logic for Emby operations.
/// Methods accept grpc-generated request types and return grpc-generated response types.
#[derive(Clone)]
pub struct EmbyApiImpl {
    provider: Arc<EmbyProvider>,
    credential_repo: Arc<UserProviderCredentialRepository>,
    access_service: Arc<dyn ProviderAccessService>,
    event_service: Arc<dyn crate::runtime::RealtimeEventService>,
}

impl EmbyApiImpl {
    #[must_use]
    pub fn new_with_runtime(
        provider: Arc<EmbyProvider>,
        credential_repo: Arc<UserProviderCredentialRepository>,
        runtime: ProviderApiRuntime,
    ) -> Self {
        Self {
            provider,
            credential_repo,
            access_service: runtime.access_service,
            event_service: runtime.event_service,
        }
    }

    /// Resolve Emby credentials from DB using server_id, returning (host, api_key, emby_user_id).
    async fn resolve_credentials(
        &self,
        caller_user_id: &UserId,
        server_id: &str,
        request_context: Option<&ExecutionControl>,
    ) -> Result<(String, String, String, Option<String>), synctv_core::provider::ProviderError>
    {
        let access = self
            .access_service
            .emby_access(*caller_user_id, server_id, None, request_context)
            .await?;
        Ok((
            access.host,
            access.api_key,
            access.emby_user_id,
            access.provider_instance_name,
        ))
    }

    pub async fn login_with_context(
        &self,
        caller_user_id: &UserId,
        req: LoginRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<LoginResponse, synctv_core::provider::ProviderError> {
        let host = req.host.clone();
        if req.username.trim().is_empty() {
            return Err(synctv_core::provider::ProviderError::InvalidConfig(
                "Emby username must not be empty".to_string(),
            ));
        }
        let credential = Self::resolve_login_credential(req.credential)?;

        let login_resp = self
            .provider
            .login_with_context(
                synctv_media_providers::grpc::emby::LoginReq {
                    host: req.host,
                    username: req.username,
                    credential: Some(credential),
                },
                instance_name,
                request_context,
            )
            .await?;

        // Extract admin status from user policy
        let is_admin = login_resp
            .policy
            .as_ref()
            .is_some_and(|p| p.is_administrator);

        // Generate server_id and persist credential
        let server_id =
            UserProviderCredential::generate_server_id_for_instance(&host, instance_name);
        let credential_data =
            ProviderCredential::emby(host, login_resp.token, login_resp.user_id.clone());

        let credential = UserProviderCredential {
            id: 0,
            user_id: *caller_user_id,
            provider: synctv_core::provider::EmbyProvider::NAME.to_string(),
            server_id: server_id.clone(),
            provider_instance_name: instance_name.map(ToString::to_string),
            credential_data,
            expires_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        self.credential_repo
            .upsert_by_user_provider_server(&credential)
            .await
            .map_err(|e| {
                synctv_core::provider::ProviderError::Internal(format!(
                    "Failed to persist emby credential: {e}"
                ))
            })?;

        self.access_service
            .invalidate(
                *caller_user_id,
                synctv_core::provider::EmbyProvider::NAME,
                &server_id,
            )
            .await?;
        publish_provider_credential_changed(
            &self.event_service,
            *caller_user_id,
            synctv_core::provider::EmbyProvider::NAME,
            &server_id,
        );

        Ok(LoginResponse {
            user_id: login_resp.user_id,
            username: login_resp.username,
            is_admin,
            server_id,
        })
    }

    fn resolve_login_credential(
        credential: Option<synctv_proto::providers::emby::login_request::Credential>,
    ) -> Result<
        synctv_media_providers::grpc::emby::login_req::Credential,
        synctv_core::provider::ProviderError,
    > {
        match credential {
            Some(synctv_proto::providers::emby::login_request::Credential::Password(password)) => {
                Ok(synctv_media_providers::grpc::emby::login_req::Credential::Password(password))
            }
            Some(synctv_proto::providers::emby::login_request::Credential::ApiKey(api_key)) => {
                Ok(synctv_media_providers::grpc::emby::login_req::Credential::ApiKey(api_key))
            }
            None => Err(synctv_core::provider::ProviderError::InvalidConfig(
                "Emby login requires exactly one credential".to_string(),
            )),
        }
    }

    pub async fn list_with_context(
        &self,
        caller_user_id: &UserId,
        req: ListRequest,
        requested_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<ListResponse, synctv_core::provider::ProviderError> {
        let (host, token, user_id, credential_instance_name) = self
            .resolve_credentials(caller_user_id, &req.server_id, request_context)
            .await?;
        let effective_instance_name = resolve_bound_instance_name(
            requested_instance_name,
            credential_instance_name.as_deref(),
        )?;

        let list_req = synctv_media_providers::grpc::emby::FsListReq {
            host,
            token,
            path: req.path,
            start_index: req.start_index,
            limit: req.limit,
            search_term: req.search_term,
            user_id,
        };

        let resp = self
            .provider
            .fs_list_with_context(
                list_req,
                effective_instance_name.as_deref(),
                request_context,
            )
            .await?;

        let items: Vec<MediaItem> = resp
            .items
            .into_iter()
            .map(|item| {
                let thumbnail = if item.has_thumbnail {
                    emby_thumbnail_url(&req.server_id, caller_user_id, &item.id)
                } else {
                    String::new()
                };
                MediaItem {
                    thumbnail,
                    id: item.id,
                    name: item.name,
                    r#type: item.r#type,
                    parent_id: item.parent_id,
                    series_name: item.series_name,
                    series_id: item.series_id,
                    season_name: item.season_name,
                    description: item.description,
                }
            })
            .collect();

        Ok(ListResponse {
            items,
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
        let (host, token, user_id, credential_instance_name) = self
            .resolve_credentials(caller_user_id, &req.server_id, request_context)
            .await?;
        let effective_instance_name = resolve_bound_instance_name(
            requested_instance_name,
            credential_instance_name.as_deref(),
        )?;

        let me_req = synctv_media_providers::grpc::emby::MeReq {
            host,
            token,
            user_id,
        };

        let resp = self
            .provider
            .me_with_context(me_req, effective_instance_name.as_deref(), request_context)
            .await?;

        Ok(GetMeResponse {
            id: resp.id,
            name: resp.name,
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
                "Emby logout requires an explicit server_id".to_string(),
            ));
        }

        super::common::delete_credential_and_notify(
            &self.credential_repo,
            &self.access_service,
            &self.event_service,
            caller_user_id,
            synctv_core::provider::EmbyProvider::NAME,
            &req.server_id,
        )
        .await?;

        Ok(LogoutResponse {
            message: "Logout successful".to_string(),
        })
    }

    pub async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<GetBindsResponse, crate::impls::ApiError> {
        let binds = get_provider_binds(
            &self.credential_repo,
            caller_user_id,
            synctv_core::provider::EmbyProvider::NAME,
            "emby_user_id",
            instance_name,
        )
        .await?
        .into_iter()
        .map(|bind| BindInfo {
            id: bind.id,
            server_id: bind.server_id,
            host: bind.host,
            user_id: bind.label_value,
            created_at: bind.created_at,
            provider_instance_name: bind.provider_instance_name,
        })
        .collect();

        Ok(GetBindsResponse { binds })
    }
}

#[cfg(test)]
mod tests {
    use super::{EmbyApiImpl, ProviderApiRuntime};
    use std::sync::Arc;
    use synctv_core::provider::{EmbyProvider, ProviderError};
    use synctv_core::repository::{ProviderInstanceRepository, UserProviderCredentialRepository};
    use synctv_core_testing::create_test_pool;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn invalid_config<T>(result: Result<T, ProviderError>) -> TestResult<String> {
        match result {
            Ok(_) => Err(test_error("expected provider invalid config")),
            Err(ProviderError::InvalidConfig(message)) => Ok(message),
            Err(other) => Err(test_error(format!("expected InvalidConfig, got {other:?}"))),
        }
    }

    fn test_api(pool: sqlx::PgPool) -> TestResult<EmbyApiImpl> {
        let instance_manager = Arc::new(synctv_core::service::RemoteProviderManager::new(
            Arc::new(ProviderInstanceRepository::new(pool.clone())),
        ));
        let provider = Arc::new(EmbyProvider::with_client_manager(
            instance_manager,
            Arc::new(synctv_core::provider::ProviderClientManager::new()?),
        ));
        let credential_repo = Arc::new(UserProviderCredentialRepository::new(pool.clone()));
        let alist_instance_manager = Arc::new(synctv_core::service::RemoteProviderManager::new(
            Arc::new(ProviderInstanceRepository::new(pool)),
        ));
        let runtime = ProviderApiRuntime {
            access_service: Arc::new(synctv_core::provider::CachedProviderAccessService::new(
                credential_repo.clone(),
                Arc::new(synctv_core::provider::AlistProvider::with_client_manager(
                    alist_instance_manager,
                    Arc::new(synctv_core::provider::ProviderClientManager::new()?),
                )),
            )),
            event_service: Arc::new(crate::runtime::LocalNoopRealtimeEventService::new()),
        };
        Ok(EmbyApiImpl::new_with_runtime(
            provider,
            credential_repo,
            runtime,
        ))
    }

    #[test]
    fn resolve_login_credential_rejects_missing_credential() -> TestResult {
        let message = invalid_config(EmbyApiImpl::resolve_login_credential(None))?;

        assert!(message.contains("exactly one credential"));
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
                synctv_proto::providers::emby::LogoutRequest {
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
