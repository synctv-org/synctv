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
use synctv_core::provider::{AlistProvider, ExecutionControl};
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
        request_context: Option<&ExecutionControl>,
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
                    .login_with_context(login_req, instance_name.as_deref(), request_context)
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
        self.login_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    pub async fn login_with_context(
        &self,
        caller_user_id: &str,
        req: LoginRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<LoginResponse, synctv_core::provider::ProviderError> {
        let host = req.host.clone();
        let (password, hashed) = Self::resolve_login_credential(&req)?;

        let login_req = synctv_media_providers::grpc::alist::LoginReq {
            host: req.host,
            username: req.username.clone(),
            password: password.clone(),
            hashed,
        };

        let token = self
            .provider
            .login_with_context(login_req, instance_name, request_context)
            .await?;

        // Generate server_id and persist credential
        let server_id =
            UserProviderCredential::generate_server_id_for_instance(&host, instance_name);

        // Store with hashed password for re-authentication
        let stored_password = if hashed {
            password
        } else {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(
                format!("{password}-https://github.com/AlistGo/alist").as_bytes(),
            ))
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
            id: synctv_common::snanoid!(),
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

    fn resolve_login_credential(
        req: &LoginRequest,
    ) -> Result<(String, bool), synctv_core::provider::ProviderError> {
        let password = req.password.trim();
        let hashed_password = req.hashed_password.trim();

        if hashed_password.is_empty() {
            if password.is_empty() {
                return Err(synctv_core::provider::ProviderError::InvalidConfig(
                    "Alist login requires password or hashed_password".to_string(),
                ));
            }

            return Ok((req.password.clone(), false));
        }

        Ok((req.hashed_password.clone(), true))
    }

    /// List Alist directory using stored credential
    pub async fn list(
        &self,
        caller_user_id: &str,
        req: ListRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ListResponse, synctv_core::provider::ProviderError> {
        self.list_with_context(caller_user_id, req, requested_instance_name, None)
            .await
    }

    pub async fn list_with_context(
        &self,
        caller_user_id: &str,
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
        self.get_me_with_context(caller_user_id, req, requested_instance_name, None)
            .await
    }

    pub async fn get_me_with_context(
        &self,
        caller_user_id: &str,
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

        let me_req = synctv_media_providers::grpc::alist::MeReq { host, token };

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
        caller_user_id: &str,
        req: LogoutRequest,
    ) -> Result<LogoutResponse, synctv_core::provider::ProviderError> {
        if req.server_id.trim().is_empty() {
            return Err(synctv_core::provider::ProviderError::InvalidConfig(
                "Alist logout requires an explicit server_id".to_string(),
            ));
        }

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

        Ok(LogoutResponse {
            message: "Logout successful".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::AlistApiImpl;
    use std::sync::Arc;
    use synctv_core::provider::{AlistProvider, ProviderError};
    use synctv_core::repository::{ProviderInstanceRepository, UserProviderCredentialRepository};
    use synctv_core::service::RemoteProviderManager;
    use synctv_core_testing::create_test_pool;
    use synctv_proto::providers::alist::LoginRequest;

    fn provider() -> Arc<AlistProvider> {
        let pool = sqlx::PgPool::connect_lazy("postgresql://fake").expect("lazy pool");
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        Arc::new(AlistProvider::new(Arc::new(RemoteProviderManager::new(
            repo,
        ))))
    }

    #[test]
    fn resolve_login_credential_rejects_missing_password_and_hash() {
        let err = AlistApiImpl::resolve_login_credential(&LoginRequest {
            host: "https://alist.example.com".to_string(),
            username: "alice".to_string(),
            password: String::new(),
            hashed_password: String::new(),
            instance_name: String::new(),
        })
        .expect_err("missing both credential forms must fail");

        match err {
            ProviderError::InvalidConfig(message) => {
                assert!(message.contains("password or hashed_password"));
            }
            other => panic!("expected InvalidConfig, got {other:?}"),
        }
    }

    #[test]
    fn resolve_login_credential_accepts_plaintext_password_without_hash() {
        let (credential, hashed) = AlistApiImpl::resolve_login_credential(&LoginRequest {
            host: "https://alist.example.com".to_string(),
            username: "alice".to_string(),
            password: "secret123".to_string(),
            hashed_password: String::new(),
            instance_name: String::new(),
        })
        .expect("plaintext password must remain valid");

        assert_eq!(credential, "secret123");
        assert!(!hashed);
    }

    #[test]
    fn resolve_login_credential_accepts_hashed_password_without_plaintext() {
        let (credential, hashed) = AlistApiImpl::resolve_login_credential(&LoginRequest {
            host: "https://alist.example.com".to_string(),
            username: "alice".to_string(),
            password: String::new(),
            hashed_password: "sha256:abc123".to_string(),
            instance_name: String::new(),
        })
        .expect("hashed password must remain valid");

        assert_eq!(credential, "sha256:abc123");
        assert!(hashed);
    }

    #[tokio::test]
    async fn login_rejects_missing_password_and_hash_before_provider_call() {
        let pool = sqlx::PgPool::connect_lazy("postgresql://fake").expect("lazy pool");
        let api = AlistApiImpl::new(
            provider(),
            Arc::new(UserProviderCredentialRepository::new(pool)),
        );

        let err = api
            .login(
                "user-1",
                LoginRequest {
                    host: "https://alist.example.com".to_string(),
                    username: "alice".to_string(),
                    password: String::new(),
                    hashed_password: String::new(),
                    instance_name: String::new(),
                },
                None,
            )
            .await
            .expect_err("missing both credential forms must fail before provider login");

        match err {
            ProviderError::InvalidConfig(message) => {
                assert!(message.contains("password or hashed_password"));
            }
            other => panic!("expected InvalidConfig, got {other:?}"),
        }
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn logout_rejects_empty_server_id() {
        let (_postgres, pool) = create_test_pool().await;
        let api = AlistApiImpl::new(
            provider(),
            Arc::new(UserProviderCredentialRepository::new(pool)),
        );

        let err = api
            .logout(
                "user-1",
                crate::proto::providers::alist::LogoutRequest {
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
