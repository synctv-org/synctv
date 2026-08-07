use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError};
use synctv_management::{
    provider_runtime::{
        AlistListQuery, AlistLoginCommand, AlistLoginCredential, AlistRuntime, AlistSearchQuery,
        ProviderCredentialServerQuery,
    },
    runtime_error::RuntimeError,
};
use synctv_proto::providers::alist as alist_proto;

use super::super::map_runtime_error;

pub(crate) struct ManagementAlistRuntime {
    inner: Arc<synctv_api::providers::AlistApiImpl>,
}

impl ManagementAlistRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::providers::AlistApiImpl>) -> Self {
        Self { inner }
    }
}

fn login_command_to_proto(command: AlistLoginCommand) -> alist_proto::LoginRequest {
    alist_proto::LoginRequest {
        host: command.host,
        username: command.username,
        credential: command.credential.map(|credential| match credential {
            AlistLoginCredential::Password(password) => {
                alist_proto::login_request::Credential::Password(password)
            }
            AlistLoginCredential::HashedPassword(hashed_password) => {
                alist_proto::login_request::Credential::HashedPassword(hashed_password)
            }
        }),
        otp_code: command.otp_code,
        otp_secret: command.otp_secret,
        instance_name: String::new(),
    }
}

#[tonic::async_trait]
impl AlistRuntime for ManagementAlistRuntime {
    async fn login(
        &self,
        caller_user_id: &UserId,
        command: AlistLoginCommand,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::LoginResponse, ProviderError> {
        let req = login_command_to_proto(command);
        self.inner
            .login_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    async fn list(
        &self,
        caller_user_id: &UserId,
        query: AlistListQuery,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::ListResponse, ProviderError> {
        let req = alist_proto::ListRequest {
            server_id: query.server_id,
            path: query.path,
            password: query.password,
            page: query.page,
            per_page: query.per_page,
            refresh: query.refresh,
            instance_name: String::new(),
        };
        self.inner
            .list_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    async fn search(
        &self,
        caller_user_id: &UserId,
        query: AlistSearchQuery,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::SearchResponse, ProviderError> {
        let req = alist_proto::SearchRequest {
            server_id: query.server_id,
            parent: query.parent,
            keywords: query.keywords,
            scope: query.scope,
            page: query.page,
            per_page: query.per_page,
            password: query.password,
            instance_name: String::new(),
        };
        self.inner
            .search_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    async fn get_me(
        &self,
        caller_user_id: &UserId,
        query: ProviderCredentialServerQuery,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::GetMeResponse, ProviderError> {
        let req = alist_proto::GetMeRequest {
            server_id: query.server_id,
            instance_name: String::new(),
        };
        self.inner
            .get_me_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    async fn logout(
        &self,
        caller_user_id: &UserId,
        command: ProviderCredentialServerQuery,
    ) -> Result<alist_proto::LogoutResponse, ProviderError> {
        let req = alist_proto::LogoutRequest {
            server_id: command.server_id,
        };
        self.inner.logout(caller_user_id, req).await
    }

    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::GetBindsResponse, RuntimeError> {
        self.inner
            .get_binds(caller_user_id, instance_name)
            .await
            .map_err(|error| map_runtime_error(&error))
    }
}
