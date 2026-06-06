//! Emby API Implementation
//!
//! Unified implementation for all Emby API operations.
//! Used by both HTTP and gRPC handlers.

use crate::proto::providers::emby::{
    BindInfo, GetBindsResponse, GetMeRequest, GetMeResponse, ListRequest, ListResponse,
    LoginRequest, LoginResponse, LogoutRequest, LogoutResponse, MediaItem,
};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use std::sync::Arc;
use synctv_core::models::{ProviderCredential, UserId, UserProviderCredential};
use synctv_core::provider::{EmbyProvider, ExecutionControl, ProviderAccessService};
use synctv_core::repository::UserProviderCredentialRepository;

use super::ProviderApiRuntime;
use super::{get_provider_binds, publish_provider_credential_changed, resolve_bound_instance_name};

fn emby_thumbnail_url(server_id: &str, credential_owner_id: &UserId, item_id: &str) -> String {
    format!(
        "/api/providers/emby/thumbnail/{item_id}?server_id={server_id}&credential_owner_id={credential_owner_id}&max_height=300",
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
    access_service: Option<Arc<dyn ProviderAccessService>>,
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
        if let Some(access_service) = &self.access_service {
            let access = access_service
                .emby_access(*caller_user_id, server_id, None, request_context)
                .await?;
            return Ok((
                access.host,
                access.api_key,
                access.emby_user_id,
                access.provider_instance_name,
            ));
        }

        let cred = self
            .credential_repo
            .get_by_provider_and_server(
                *caller_user_id,
                synctv_core::provider::EmbyProvider::NAME,
                server_id,
            )
            .await
            .map_err(|e| {
                synctv_core::provider::ProviderError::Internal(format!(
                    "Failed to query emby credential: {e}"
                ))
            })?
            .ok_or(synctv_core::provider::ProviderError::CredentialNotFound(
                format!("No emby credential found for server_id '{server_id}'"),
            ))?;

        if cred.is_expired() {
            return Err(synctv_core::provider::ProviderError::CredentialExpired(
                "Emby credential has expired".to_string(),
            ));
        }

        match cred.get_credential() {
            Ok(ProviderCredential::Emby {
                host,
                api_key,
                emby_user_id,
            }) => Ok((host, api_key, emby_user_id, cred.provider_instance_name)),
            Ok(_) => Err(synctv_core::provider::ProviderError::InvalidCredentialType),
            Err(e) => Err(synctv_core::provider::ProviderError::Internal(format!(
                "Failed to parse emby credential: {e}"
            ))),
        }
    }

    /// Login to Emby and persist credentials
    pub async fn login(
        &self,
        caller_user_id: &UserId,
        req: LoginRequest,
        instance_name: Option<&str>,
    ) -> Result<LoginResponse, synctv_core::provider::ProviderError> {
        self.login_with_context(caller_user_id, req, instance_name, None)
            .await
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

        let login_resp = self
            .provider
            .login_with_context(
                synctv_media_providers::grpc::emby::LoginReq {
                    host: req.host,
                    username: req.username,
                    credential: Some(
                        match req.credential.ok_or_else(|| {
                            synctv_core::provider::ProviderError::InvalidConfig(
                                "Emby login requires exactly one credential".to_string(),
                            )
                        })? {
                            crate::proto::providers::emby::login_request::Credential::Password(
                                password,
                            ) => {
                                synctv_media_providers::grpc::emby::login_req::Credential::Password(
                                    password,
                                )
                            }
                            crate::proto::providers::emby::login_request::Credential::ApiKey(
                                api_key,
                            ) => synctv_media_providers::grpc::emby::login_req::Credential::ApiKey(
                                api_key,
                            ),
                        },
                    ),
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
            credential_data: serde_json::to_value(&credential_data).map_err(|e| {
                synctv_core::provider::ProviderError::Internal(format!(
                    "Failed to serialize credential: {e}"
                ))
            })?,
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

        if let Some(access_service) = &self.access_service {
            access_service
                .invalidate(
                    *caller_user_id,
                    synctv_core::provider::EmbyProvider::NAME,
                    &server_id,
                )
                .await?;
        }
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

    /// List Emby library items using stored credential
    pub async fn list(
        &self,
        caller_user_id: &UserId,
        req: ListRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ListResponse, synctv_core::provider::ProviderError> {
        self.list_with_context(caller_user_id, req, requested_instance_name, None)
            .await
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

    /// Get Emby user info using stored credential
    pub async fn get_me(
        &self,
        caller_user_id: &UserId,
        req: GetMeRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<GetMeResponse, synctv_core::provider::ProviderError> {
        self.get_me_with_context(caller_user_id, req, requested_instance_name, None)
            .await
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

        if let Some(existing) = self
            .credential_repo
            .get_by_provider_and_server(
                *caller_user_id,
                synctv_core::provider::EmbyProvider::NAME,
                &req.server_id,
            )
            .await
            .map_err(|e| {
                synctv_core::provider::ProviderError::Internal(format!(
                    "Failed to query credential: {e}"
                ))
            })?
        {
            self.credential_repo
                .delete(existing.id)
                .await
                .map_err(|e| {
                    synctv_core::provider::ProviderError::Internal(format!(
                        "Failed to delete credential: {e}"
                    ))
                })?;
            if let Some(access_service) = &self.access_service {
                access_service
                    .invalidate(
                        *caller_user_id,
                        synctv_core::provider::EmbyProvider::NAME,
                        &req.server_id,
                    )
                    .await?;
            }
            publish_provider_credential_changed(
                &self.event_service,
                *caller_user_id,
                synctv_core::provider::EmbyProvider::NAME,
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
    use synctv_core::service::RemoteProviderManager;
    use synctv_core_testing::create_test_pool;

    fn provider() -> Arc<EmbyProvider> {
        let pool = sqlx::PgPool::connect_lazy("postgresql://fake").expect("lazy pool");
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        Arc::new(
            EmbyProvider::new(Arc::new(RemoteProviderManager::new(repo)))
                .expect("provider should build"),
        )
    }

    fn test_provider_runtime() -> ProviderApiRuntime {
        ProviderApiRuntime {
            access_service: None,
            event_service: Arc::new(crate::runtime::LocalNoopRealtimeEventService::new()),
        }
    }

    fn test_api(
        provider: Arc<EmbyProvider>,
        credential_repo: Arc<UserProviderCredentialRepository>,
    ) -> EmbyApiImpl {
        EmbyApiImpl::new_with_runtime(provider, credential_repo, test_provider_runtime())
    }

    #[tokio::test]
    async fn login_rejects_missing_credential_before_provider_call() {
        let pool = sqlx::PgPool::connect_lazy("postgresql://fake").expect("lazy pool");
        let api = test_api(
            provider(),
            Arc::new(UserProviderCredentialRepository::new(pool)),
        );

        let err = api
            .login(
                &synctv_core::models::UserId::new(),
                crate::proto::providers::emby::LoginRequest {
                    host: "https://emby.example.com".to_string(),
                    username: "alice".to_string(),
                    credential: None,
                    instance_name: String::new(),
                },
                None,
            )
            .await
            .expect_err("missing credential must fail before provider login");

        match err {
            ProviderError::InvalidConfig(message) => {
                assert!(message.contains("exactly one credential"));
            }
            other => panic!("expected InvalidConfig, got {other:?}"),
        }
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn logout_rejects_empty_server_id() {
        let (_postgres, pool) = create_test_pool().await;
        let api = test_api(
            provider(),
            Arc::new(UserProviderCredentialRepository::new(pool)),
        );

        let err = api
            .logout(
                &synctv_core::models::UserId::new(),
                crate::proto::providers::emby::LogoutRequest {
                    server_id: String::new(),
                    instance_name: String::new(),
                },
            )
            .await
            .expect_err("empty server_id must fail before credential lookup");

        match err {
            ProviderError::InvalidConfig(message) => {
                assert!(message.contains("explicit server_id"));
            }
            other => panic!("expected InvalidConfig, got {other:?}"),
        }
    }
}
