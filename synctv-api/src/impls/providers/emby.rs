//! Emby API Implementation
//!
//! Unified implementation for all Emby API operations.
//! Used by both HTTP and gRPC handlers.

use crate::proto::providers::emby::{
    GetMeRequest, GetMeResponse, ListRequest, ListResponse, LoginRequest, LoginResponse,
    LogoutRequest, LogoutResponse, MediaItem,
};
use std::sync::Arc;
use synctv_core::models::{ProviderCredential, UserProviderCredential};
use synctv_core::provider::EmbyProvider;
use synctv_core::repository::UserProviderCredentialRepository;

use super::resolve_bound_instance_name;

/// Emby API implementation
///
/// Contains all business logic for Emby operations.
/// Methods accept grpc-generated request types and return grpc-generated response types.
#[derive(Clone)]
pub struct EmbyApiImpl {
    provider: Arc<EmbyProvider>,
    credential_repo: Arc<UserProviderCredentialRepository>,
}

impl EmbyApiImpl {
    #[must_use]
    pub const fn new(
        provider: Arc<EmbyProvider>,
        credential_repo: Arc<UserProviderCredentialRepository>,
    ) -> Self {
        Self {
            provider,
            credential_repo,
        }
    }

    /// Resolve Emby credentials from DB using server_id, returning (host, api_key, emby_user_id).
    async fn resolve_credentials(
        &self,
        caller_user_id: &str,
        server_id: &str,
    ) -> Result<(String, String, String, Option<String>), synctv_core::provider::ProviderError> {
        let cred = self
            .credential_repo
            .get_by_provider_and_server(
                caller_user_id,
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
        caller_user_id: &str,
        req: LoginRequest,
        instance_name: Option<&str>,
    ) -> Result<LoginResponse, synctv_core::provider::ProviderError> {
        let host = req.host.clone();
        let api_key = req.api_key.clone();

        let user_info = self
            .provider
            .login(req.host, req.api_key, instance_name)
            .await?;

        // Extract admin status from user policy
        let is_admin = user_info
            .policy
            .as_ref()
            .is_some_and(|p| p.is_administrator);

        // Generate server_id and persist credential
        let server_id =
            UserProviderCredential::generate_server_id_for_instance(&host, instance_name);
        let credential_data = ProviderCredential::emby(host, api_key, user_info.id.clone());

        // Upsert: delete existing then create
        if let Some(existing) = self
            .credential_repo
            .get_by_provider_and_server(
                caller_user_id,
                synctv_core::provider::EmbyProvider::NAME,
                &server_id,
            )
            .await
            .ok()
            .flatten()
        {
            let _ = self.credential_repo.delete(&existing.id).await;
        }

        let credential = UserProviderCredential {
            id: nanoid::nanoid!(),
            user_id: caller_user_id.to_string(),
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
            .create(&credential)
            .await
            .map_err(|e| {
                synctv_core::provider::ProviderError::Internal(format!(
                    "Failed to persist emby credential: {e}"
                ))
            })?;

        Ok(LoginResponse {
            user_id: user_info.id,
            username: user_info.name,
            is_admin,
            server_id,
        })
    }

    /// List Emby library items using stored credential
    pub async fn list(
        &self,
        caller_user_id: &str,
        req: ListRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ListResponse, synctv_core::provider::ProviderError> {
        let (host, token, user_id, credential_instance_name) = self
            .resolve_credentials(caller_user_id, &req.server_id)
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
            .fs_list(list_req, effective_instance_name.as_deref())
            .await?;

        let items: Vec<MediaItem> = resp
            .items
            .into_iter()
            .map(|item| MediaItem {
                id: item.id,
                name: item.name,
                r#type: item.r#type,
                parent_id: item.parent_id,
                series_name: item.series_name,
                series_id: item.series_id,
                season_name: item.season_name,
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
        caller_user_id: &str,
        req: GetMeRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<GetMeResponse, synctv_core::provider::ProviderError> {
        let (host, token, _, credential_instance_name) = self
            .resolve_credentials(caller_user_id, &req.server_id)
            .await?;
        let effective_instance_name = resolve_bound_instance_name(
            requested_instance_name,
            credential_instance_name.as_deref(),
        )?;

        let me_req = synctv_media_providers::grpc::emby::MeReq {
            host,
            token,
            user_id: String::new(),
        };

        let resp = self
            .provider
            .me(me_req, effective_instance_name.as_deref())
            .await?;

        Ok(GetMeResponse {
            id: resp.id,
            name: resp.name,
        })
    }

    /// Logout and delete stored credential
    pub async fn logout(
        &self,
        caller_user_id: &str,
        req: LogoutRequest,
    ) -> Result<LogoutResponse, synctv_core::provider::ProviderError> {
        if !req.server_id.is_empty() {
            if let Some(existing) = self
                .credential_repo
                .get_by_provider_and_server(
                    caller_user_id,
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
                    .delete(&existing.id)
                    .await
                    .map_err(|e| {
                        synctv_core::provider::ProviderError::Internal(format!(
                            "Failed to delete credential: {e}"
                        ))
                    })?;
            }
        }

        Ok(LogoutResponse {
            message: "Logout successful".to_string(),
        })
    }
}
