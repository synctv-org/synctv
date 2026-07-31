use std::sync::Arc;

use synctv_core::models::{
    ProviderInstanceListQuery, ProviderInstanceListSortBy, SortDirection, UserId,
};
use synctv_management::{
    provider_runtime::{
        AddProviderInstanceCommand, ListAvailableProviderInstancesQuery, ListProviderBackendsQuery,
        ProviderCommonRuntime, ProviderInstanceNameCommand, UpdateProviderInstanceCommand,
    },
    request_context::RequestContext,
    runtime_error::RuntimeError,
};
use synctv_proto::{
    providers::common as provider_common_proto, source_config as source_config_proto,
};

use super::super::map_runtime_error;

pub(crate) struct ManagementProviderCommonRuntime {
    inner: Arc<synctv_api::providers::ProviderCommonApiImpl>,
}

impl ManagementProviderCommonRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::providers::ProviderCommonApiImpl>) -> Self {
        Self { inner }
    }
}

fn api_request_context(ctx: &RequestContext) -> synctv_api::AdminRequestContext {
    synctv_api::AdminRequestContext {
        ip_address: ctx.ip_address.clone(),
        user_agent: ctx.user_agent.clone(),
    }
}

fn source_provider_to_proto(provider: Option<synctv_core::models::SourceProvider>) -> i32 {
    match provider {
        Some(synctv_core::models::SourceProvider::DirectUrl) => {
            source_config_proto::SourceProvider::DirectUrl as i32
        }
        Some(synctv_core::models::SourceProvider::Bilibili) => {
            source_config_proto::SourceProvider::Bilibili as i32
        }
        Some(synctv_core::models::SourceProvider::Alist) => {
            source_config_proto::SourceProvider::Alist as i32
        }
        Some(synctv_core::models::SourceProvider::Emby) => {
            source_config_proto::SourceProvider::Emby as i32
        }
        Some(synctv_core::models::SourceProvider::Rtmp) => {
            source_config_proto::SourceProvider::Rtmp as i32
        }
        Some(synctv_core::models::SourceProvider::LiveProxy) => {
            source_config_proto::SourceProvider::LiveProxy as i32
        }
        Some(synctv_core::models::SourceProvider::Cloudreve) => {
            source_config_proto::SourceProvider::Cloudreve as i32
        }
        Some(synctv_core::models::SourceProvider::Twitch) => {
            source_config_proto::SourceProvider::Twitch as i32
        }
        Some(synctv_core::models::SourceProvider::Huya) => {
            source_config_proto::SourceProvider::Huya as i32
        }
        Some(synctv_core::models::SourceProvider::Douyu) => {
            source_config_proto::SourceProvider::Douyu as i32
        }
        Some(synctv_core::models::SourceProvider::Douyin) => {
            source_config_proto::SourceProvider::Douyin as i32
        }
        Some(synctv_core::models::SourceProvider::TikTok) => {
            source_config_proto::SourceProvider::Tiktok as i32
        }
        Some(synctv_core::models::SourceProvider::AcFun) => {
            source_config_proto::SourceProvider::Acfun as i32
        }
        Some(synctv_core::models::SourceProvider::Cctv) => {
            source_config_proto::SourceProvider::Cctv as i32
        }
        Some(synctv_core::models::SourceProvider::Fnos) => {
            source_config_proto::SourceProvider::Fnos as i32
        }
        Some(synctv_core::models::SourceProvider::Qnap) => {
            source_config_proto::SourceProvider::Qnap as i32
        }
        Some(synctv_core::models::SourceProvider::Synology) => {
            source_config_proto::SourceProvider::Synology as i32
        }
        Some(synctv_core::models::SourceProvider::Nextcloud) => {
            source_config_proto::SourceProvider::Nextcloud as i32
        }
        Some(synctv_core::models::SourceProvider::Seafile) => {
            source_config_proto::SourceProvider::Seafile as i32
        }
        Some(synctv_core::models::SourceProvider::TrueNas) => {
            source_config_proto::SourceProvider::Truenas as i32
        }
        Some(synctv_core::models::SourceProvider::Youtube) => {
            source_config_proto::SourceProvider::Youtube as i32
        }
        None => source_config_proto::SourceProvider::Unspecified as i32,
    }
}

fn required_source_provider_to_proto(provider: synctv_core::models::SourceProvider) -> i32 {
    source_provider_to_proto(Some(provider))
}

fn provider_instance_sort_by_to_proto(sort_by: ProviderInstanceListSortBy) -> i32 {
    match sort_by {
        ProviderInstanceListSortBy::Name => {
            provider_common_proto::ProviderInstanceListSortBy::Name as i32
        }
        ProviderInstanceListSortBy::Endpoint => {
            provider_common_proto::ProviderInstanceListSortBy::Endpoint as i32
        }
        ProviderInstanceListSortBy::UpdatedAt => {
            provider_common_proto::ProviderInstanceListSortBy::UpdatedAt as i32
        }
        ProviderInstanceListSortBy::CreatedAt => {
            provider_common_proto::ProviderInstanceListSortBy::CreatedAt as i32
        }
    }
}

fn provider_sort_direction_to_proto(sort_direction: SortDirection) -> i32 {
    match sort_direction {
        SortDirection::Asc => provider_common_proto::SortDirection::Asc as i32,
        SortDirection::Desc => provider_common_proto::SortDirection::Desc as i32,
    }
}

#[tonic::async_trait]
impl ProviderCommonRuntime for ManagementProviderCommonRuntime {
    async fn list_available_provider_instances(
        &self,
        query: ListAvailableProviderInstancesQuery,
    ) -> Result<provider_common_proto::ProviderInstancesResponse, RuntimeError> {
        let req = provider_common_proto::ListAvailableProviderInstancesRequest {
            provider_type: source_provider_to_proto(query.provider_type),
        };
        self.inner
            .list_available_provider_instances(req)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn list_provider_backends(
        &self,
        query: ListProviderBackendsQuery,
    ) -> Result<provider_common_proto::ProviderBackendsResponse, RuntimeError> {
        let req = provider_common_proto::ListProviderBackendsRequest {
            provider_type: required_source_provider_to_proto(query.provider_type),
        };
        self.inner
            .list_provider_backends(req)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn list_provider_instances(
        &self,
        query: ProviderInstanceListQuery,
    ) -> Result<provider_common_proto::ListProviderInstancesResponse, RuntimeError> {
        let req = provider_common_proto::ListProviderInstancesRequest {
            page: i32::try_from(query.pagination.page).unwrap_or(i32::MAX),
            page_size: i32::try_from(query.pagination.page_size).unwrap_or(i32::MAX),
            provider_type: source_provider_to_proto(query.provider_type),
            search: query.search.unwrap_or_default(),
            enabled: query.enabled,
            tls: query.tls,
            sort_by: provider_instance_sort_by_to_proto(query.sort_by),
            sort_direction: provider_sort_direction_to_proto(query.sort_direction),
        };
        self.inner
            .list_provider_instances(req)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn add_provider_instance(
        &self,
        command: AddProviderInstanceCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<provider_common_proto::AddProviderInstanceResponse, RuntimeError> {
        let req = provider_common_proto::AddProviderInstanceRequest {
            name: command.name,
            endpoint: command.endpoint,
            comment: command.comment,
            timeout_seconds: command.timeout_seconds,
            tls: command.tls,
            insecure_tls: command.insecure_tls,
            providers: command.providers,
            jwt_secret: command.jwt_secret,
            custom_ca: command.custom_ca,
        };
        self.inner
            .add_provider_instance(req, admin_user_id, &api_request_context(ctx), None)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn update_provider_instance(
        &self,
        command: UpdateProviderInstanceCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<provider_common_proto::UpdateProviderInstanceResponse, RuntimeError> {
        let req = provider_common_proto::UpdateProviderInstanceRequest {
            name: command.name,
            endpoint: command.endpoint,
            comment: command.comment,
            timeout_seconds: command.timeout_seconds,
            tls: command.tls,
            insecure_tls: command.insecure_tls,
            providers: command.providers,
            jwt_secret: command.jwt_secret,
            custom_ca: command.custom_ca,
            clear_comment: command.clear_comment,
            clear_jwt_secret: command.clear_jwt_secret,
            clear_custom_ca: command.clear_custom_ca,
        };
        self.inner
            .update_provider_instance(req, admin_user_id, &api_request_context(ctx), None)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn delete_provider_instance(
        &self,
        command: ProviderInstanceNameCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<provider_common_proto::DeleteProviderInstanceResponse, RuntimeError> {
        let req = provider_common_proto::DeleteProviderInstanceRequest { name: command.name };
        self.inner
            .delete_provider_instance(req, admin_user_id, &api_request_context(ctx))
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn reconnect_provider_instance(
        &self,
        command: ProviderInstanceNameCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<provider_common_proto::ReconnectProviderInstanceResponse, RuntimeError> {
        let req = provider_common_proto::ReconnectProviderInstanceRequest { name: command.name };
        self.inner
            .reconnect_provider_instance(req, admin_user_id, &api_request_context(ctx), None)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn enable_provider_instance(
        &self,
        command: ProviderInstanceNameCommand,
    ) -> Result<provider_common_proto::EnableProviderInstanceResponse, RuntimeError> {
        let req = provider_common_proto::EnableProviderInstanceRequest { name: command.name };
        self.inner
            .enable_provider_instance(req, None)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn disable_provider_instance(
        &self,
        command: ProviderInstanceNameCommand,
    ) -> Result<provider_common_proto::DisableProviderInstanceResponse, RuntimeError> {
        let req = provider_common_proto::DisableProviderInstanceRequest { name: command.name };
        self.inner
            .disable_provider_instance(req, None)
            .await
            .map_err(|error| map_runtime_error(&error))
    }
}
