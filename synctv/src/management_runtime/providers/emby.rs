use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError};
use synctv_management::{
    provider_runtime::{
        EmbyListQuery, EmbyLoginCommand, EmbyLoginCredential, EmbyRuntime,
        ProviderCredentialServerQuery,
    },
    runtime_error::RuntimeError,
};
use synctv_proto::providers::emby as emby_proto;

use super::super::map_runtime_error;

pub(crate) struct ManagementEmbyRuntime {
    inner: Arc<synctv_api::providers::EmbyApiImpl>,
}

impl ManagementEmbyRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::providers::EmbyApiImpl>) -> Self {
        Self { inner }
    }
}

fn login_command_to_proto(command: EmbyLoginCommand) -> emby_proto::LoginRequest {
    emby_proto::LoginRequest {
        host: command.host,
        username: command.username,
        credential: command.credential.map(|credential| match credential {
            EmbyLoginCredential::Password(password) => {
                emby_proto::login_request::Credential::Password(password)
            }
            EmbyLoginCredential::ApiKey(api_key) => {
                emby_proto::login_request::Credential::ApiKey(api_key)
            }
        }),
        instance_name: String::new(),
    }
}

#[tonic::async_trait]
impl EmbyRuntime for ManagementEmbyRuntime {
    async fn login(
        &self,
        caller_user_id: &UserId,
        command: EmbyLoginCommand,
        instance_name: Option<&str>,
    ) -> Result<emby_proto::LoginResponse, ProviderError> {
        let req = login_command_to_proto(command);
        self.inner
            .login_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    async fn list(
        &self,
        caller_user_id: &UserId,
        query: EmbyListQuery,
        instance_name: Option<&str>,
    ) -> Result<emby_proto::ListResponse, ProviderError> {
        let req = emby_proto::ListRequest {
            server_id: query.server_id,
            path: query.path,
            start_index: query.start_index,
            limit: query.limit,
            search_term: query.search_term,
            instance_name: String::new(),
        };
        self.inner
            .list_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    async fn get_me(
        &self,
        caller_user_id: &UserId,
        query: ProviderCredentialServerQuery,
        instance_name: Option<&str>,
    ) -> Result<emby_proto::GetMeResponse, ProviderError> {
        let req = emby_proto::GetMeRequest {
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
    ) -> Result<emby_proto::LogoutResponse, ProviderError> {
        let req = emby_proto::LogoutRequest {
            server_id: command.server_id,
        };
        self.inner.logout(caller_user_id, req).await
    }

    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<emby_proto::GetBindsResponse, RuntimeError> {
        self.inner
            .get_binds(caller_user_id, instance_name)
            .await
            .map_err(|error| map_runtime_error(&error))
    }
}
