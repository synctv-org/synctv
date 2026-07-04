use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::api_runtime::SharedApiRuntime;
use crate::grpc::map_api_error;
use crate::impls::admin::RequestContext;
use synctv_core::Config;

use synctv_proto::providers::common::provider_common_service_server::ProviderCommonService;
use synctv_proto::providers::common::{
    AddProviderInstanceRequest, AddProviderInstanceResponse, DeleteProviderInstanceRequest,
    DeleteProviderInstanceResponse, DisableProviderInstanceRequest,
    DisableProviderInstanceResponse, EnableProviderInstanceRequest, EnableProviderInstanceResponse,
    ListAvailableProviderInstancesRequest, ListProviderBackendsRequest,
    ListProviderInstancesRequest, ListProviderInstancesResponse, ProviderBackendsResponse,
    ProviderInstancesResponse, ReconnectProviderInstanceRequest, ReconnectProviderInstanceResponse,
    UpdateProviderInstanceRequest, UpdateProviderInstanceResponse,
};

#[derive(Clone)]
pub struct ProviderCommonGrpcService {
    api: Arc<crate::impls::ProviderCommonApiImpl>,
    config: Arc<Config>,
}

impl ProviderCommonGrpcService {
    #[must_use]
    pub fn new(shared_api_runtime: &Arc<SharedApiRuntime>, config: Arc<Config>) -> Self {
        Self {
            api: shared_api_runtime.provider_common_api.clone(),
            config,
        }
    }

    fn grpc_request_context<T: std::fmt::Debug>(
        request: &Request<T>,
        config: &Config,
    ) -> Result<RequestContext, Status> {
        let ip_address = crate::grpc::extract_client_ip(request, config)?.map(|ip| ip.to_string());
        let user_agent = crate::grpc::request_user_agent(request)?;
        Ok(RequestContext {
            ip_address,
            user_agent,
        })
    }
}

#[tonic::async_trait]
impl ProviderCommonService for ProviderCommonGrpcService {
    async fn list_available_provider_instances(
        &self,
        request: Request<ListAvailableProviderInstancesRequest>,
    ) -> Result<Response<ProviderInstancesResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let api = self.api.clone();
        let executor_api = api.clone();

        executor_api
            .execute_user_endpoint(
                &metadata,
                crate::impls::EndpointRateLimitCategory::Read,
                move |_| async move { api.list_available_provider_instances(req).await },
            )
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn list_provider_backends(
        &self,
        request: Request<ListProviderBackendsRequest>,
    ) -> Result<Response<ProviderBackendsResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let api = self.api.clone();
        let executor_api = api.clone();

        executor_api
            .execute_user_endpoint(
                &metadata,
                crate::impls::EndpointRateLimitCategory::Read,
                move |_| async move { api.list_provider_backends(req).await },
            )
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn list_provider_instances(
        &self,
        request: Request<ListProviderInstancesRequest>,
    ) -> Result<Response<ListProviderInstancesResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let api = self.api.clone();
        let executor_api = api.clone();

        executor_api
            .execute_admin_endpoint(&metadata, move |_| async move {
                api.list_provider_instances(req).await
            })
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn add_provider_instance(
        &self,
        request: Request<AddProviderInstanceRequest>,
    ) -> Result<Response<AddProviderInstanceResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.config)?;
        let ctx = Self::grpc_request_context(&request, &self.config)?;
        let req = request.into_inner();
        let api = self.api.clone();
        let executor_api = api.clone();

        executor_api
            .execute_admin_endpoint_with_control(&metadata, move |request_control, validated| {
                let api = api.clone();
                let ctx = ctx.clone();
                async move {
                    api.add_provider_instance(req, &validated.user_id, &ctx, Some(&request_control))
                        .await
                }
            })
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn update_provider_instance(
        &self,
        request: Request<UpdateProviderInstanceRequest>,
    ) -> Result<Response<UpdateProviderInstanceResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.config)?;
        let ctx = Self::grpc_request_context(&request, &self.config)?;
        let req = request.into_inner();
        let api = self.api.clone();
        let executor_api = api.clone();

        executor_api
            .execute_admin_endpoint_with_control(&metadata, move |request_control, validated| {
                let api = api.clone();
                let ctx = ctx.clone();
                async move {
                    api.update_provider_instance(
                        req,
                        &validated.user_id,
                        &ctx,
                        Some(&request_control),
                    )
                    .await
                }
            })
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn delete_provider_instance(
        &self,
        request: Request<DeleteProviderInstanceRequest>,
    ) -> Result<Response<DeleteProviderInstanceResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.config)?;
        let ctx = Self::grpc_request_context(&request, &self.config)?;
        let req = request.into_inner();
        let api = self.api.clone();
        let executor_api = api.clone();

        executor_api
            .execute_admin_endpoint(&metadata, move |validated| {
                let api = api.clone();
                let ctx = ctx.clone();
                async move {
                    api.delete_provider_instance(req, &validated.user_id, &ctx)
                        .await
                }
            })
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn reconnect_provider_instance(
        &self,
        request: Request<ReconnectProviderInstanceRequest>,
    ) -> Result<Response<ReconnectProviderInstanceResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.config)?;
        let ctx = Self::grpc_request_context(&request, &self.config)?;
        let req = request.into_inner();
        let api = self.api.clone();
        let executor_api = api.clone();

        executor_api
            .execute_admin_endpoint_with_control(&metadata, move |request_control, validated| {
                let api = api.clone();
                let ctx = ctx.clone();
                async move {
                    api.reconnect_provider_instance(
                        req,
                        &validated.user_id,
                        &ctx,
                        Some(&request_control),
                    )
                    .await
                }
            })
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn enable_provider_instance(
        &self,
        request: Request<EnableProviderInstanceRequest>,
    ) -> Result<Response<EnableProviderInstanceResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let api = self.api.clone();
        let executor_api = api.clone();

        executor_api
            .execute_admin_endpoint_with_control(&metadata, move |request_control, _| {
                let api = api.clone();
                async move {
                    api.enable_provider_instance(req, Some(&request_control))
                        .await
                }
            })
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn disable_provider_instance(
        &self,
        request: Request<DisableProviderInstanceRequest>,
    ) -> Result<Response<DisableProviderInstanceResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let api = self.api.clone();
        let executor_api = api.clone();

        executor_api
            .execute_admin_endpoint_with_control(&metadata, move |request_control, _| {
                let api = api.clone();
                async move {
                    api.disable_provider_instance(req, Some(&request_control))
                        .await
                }
            })
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }
}
