//! Alist API Implementation
//!
//! Unified implementation for all Alist API operations.
//! Used by both HTTP and gRPC handlers.

use crate::proto::providers::alist::{
    FileItem, GetMeRequest, GetMeResponse, ListRequest, ListResponse, LoginRequest, LoginResponse,
    LogoutRequest, LogoutResponse,
};
use std::sync::Arc;
use synctv_core::models::{ProviderCredential, UserProviderCredential};
use synctv_core::provider::AlistProvider;
use synctv_core::repository::UserProviderCredentialRepository;

use super::resolve_bound_instance_name;

/// Alist API implementation
///
/// Contains all business logic for Alist operations.
/// Methods accept grpc-generated request types and return grpc-generated response types.
#[derive(Clone)]
pub struct AlistApiImpl {
    provider: Arc<AlistProvider>,
    credential_repo: Arc<UserProviderCredentialRepository>,
}

impl AlistApiImpl {
    #[must_use]
    pub const fn new(
        provider: Arc<AlistProvider>,
        credential_repo: Arc<UserProviderCredentialRepository>,
    ) -> Self {
        Self {
            provider,
            credential_repo,
        }
    }

    /// Resolve Alist credentials from DB using server_id.
    ///
    /// Re-authenticates with stored username/password to obtain a fresh API token.
    async fn resolve_credentials(
        &self,
        caller_user_id: &str,
        server_id: &str,
    ) -> Result<(String, String, Option<String>), synctv_core::provider::ProviderError> {
        let cred = self
            .credential_repo
            .get_by_provider_and_server(
                caller_user_id,
                synctv_core::provider::AlistProvider::NAME,
                server_id,
            )
            .await
            .map_err(|e| {
                synctv_core::provider::ProviderError::Internal(format!(
                    "Failed to query alist credential: {e}"
                ))
            })?
            .ok_or(synctv_core::provider::ProviderError::CredentialNotFound(
                format!("No alist credential found for server_id '{server_id}'"),
            ))?;

        if cred.is_expired() {
            return Err(synctv_core::provider::ProviderError::CredentialExpired(
                "Alist credential has expired".to_string(),
            ));
        }

        let instance_name = cred.provider_instance_name.clone();

        match cred.get_credential() {
            Ok(ProviderCredential::Alist {
                host,
                username,
                password,
            }) => {
                // Re-login with stored credentials to get a fresh token
                let login_req = synctv_media_providers::grpc::alist::LoginReq {
                    host: host.clone(),
                    username,
                    password,
                    hashed: true,
                };

                let token = self
                    .provider
                    .login(login_req, instance_name.as_deref())
                    .await?;

                Ok((host, token, instance_name))
            }
            Ok(_) => Err(synctv_core::provider::ProviderError::InvalidCredentialType),
            Err(e) => Err(synctv_core::provider::ProviderError::Internal(format!(
                "Failed to parse alist credential: {e}"
            ))),
        }
    }

    /// Login to Alist and persist credentials
    pub async fn login(
        &self,
        caller_user_id: &str,
        req: LoginRequest,
        instance_name: Option<&str>,
    ) -> Result<LoginResponse, synctv_core::provider::ProviderError> {
        let host = req.host.clone();

        let (password, hashed) = if req.hashed_password.is_empty() {
            (req.password.clone(), false)
        } else {
            (req.hashed_password.clone(), true)
        };

        let login_req = synctv_media_providers::grpc::alist::LoginReq {
            host: req.host,
            username: req.username.clone(),
            password: password.clone(),
            hashed,
        };

        let token = self.provider.login(login_req, instance_name).await?;

        // Generate server_id and persist credential
        let server_id =
            UserProviderCredential::generate_server_id_for_instance(&host, instance_name);

        // Store with hashed password for re-authentication
        let stored_password = if hashed {
            password
        } else {
            use sha2::{Digest, Sha256};
            format!(
                "{:x}",
                Sha256::digest(format!("{password}-https://github.com/AlistGo/alist").as_bytes())
            )
        };

        let credential_data = ProviderCredential::alist(host, req.username, stored_password);

        // Upsert: delete existing then create
        if let Some(existing) = self
            .credential_repo
            .get_by_provider_and_server(
                caller_user_id,
                synctv_core::provider::AlistProvider::NAME,
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
            provider: synctv_core::provider::AlistProvider::NAME.to_string(),
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
                    "Failed to persist alist credential: {e}"
                ))
            })?;

        Ok(LoginResponse { token, server_id })
    }

    /// List Alist directory using stored credential
    pub async fn list(
        &self,
        caller_user_id: &str,
        req: ListRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ListResponse, synctv_core::provider::ProviderError> {
        let (host, token, credential_instance_name) = self
            .resolve_credentials(caller_user_id, &req.server_id)
            .await?;
        let effective_instance_name = resolve_bound_instance_name(
            requested_instance_name,
            credential_instance_name.as_deref(),
        )?;

        let list_req = synctv_media_providers::grpc::alist::FsListReq {
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
            .fs_list(list_req, effective_instance_name.as_deref())
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
                r#type: item.r#type,
            })
            .collect();

        Ok(ListResponse {
            content,
            total: resp.total,
        })
    }

    /// Get Alist user info using stored credential
    pub async fn get_me(
        &self,
        caller_user_id: &str,
        req: GetMeRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<GetMeResponse, synctv_core::provider::ProviderError> {
        let (host, token, credential_instance_name) = self
            .resolve_credentials(caller_user_id, &req.server_id)
            .await?;
        let effective_instance_name = resolve_bound_instance_name(
            requested_instance_name,
            credential_instance_name.as_deref(),
        )?;

        let me_req = synctv_media_providers::grpc::alist::MeReq { host, token };

        let resp = self
            .provider
            .me(me_req, effective_instance_name.as_deref())
            .await?;

        Ok(GetMeResponse {
            username: resp.username,
            base_path: resp.base_path,
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
                    synctv_core::provider::AlistProvider::NAME,
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
