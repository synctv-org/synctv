use futures::future::BoxFuture;
use futures::FutureExt;
use std::{future::Future, sync::Arc};
use synctv_core::provider::ExecutionControl;

use super::{middleware::RequestMetadata, AppError, AppState};
use synctv_api_common::impls::admin::{RequestContext, ValidatedAdmin};
use synctv_api_common::impls::{AdminApiImpl, ApiError, RequestMetadata as ImplRequestMetadata};
use synctv_api_common::providers::ProviderCommonApiImpl;

pub(crate) trait HttpAdminApi: Clone + Send + Sync + 'static {
    fn execute_admin_endpoint<'a, T, F, Fut>(
        &'a self,
        metadata: &'a ImplRequestMetadata,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ValidatedAdmin) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a;

    fn execute_admin_endpoint_with_control<'a, T, F, Fut>(
        &'a self,
        metadata: &'a ImplRequestMetadata,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ExecutionControl, ValidatedAdmin) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a;

    fn execute_root_endpoint<'a, T, F, Fut>(
        &'a self,
        metadata: &'a ImplRequestMetadata,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ValidatedAdmin) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a;
}

impl HttpAdminApi for AdminApiImpl {
    fn execute_admin_endpoint<'a, T, F, Fut>(
        &'a self,
        metadata: &'a ImplRequestMetadata,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ValidatedAdmin) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        Self::execute_admin_endpoint(self, metadata, operation)
    }

    fn execute_admin_endpoint_with_control<'a, T, F, Fut>(
        &'a self,
        metadata: &'a ImplRequestMetadata,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ExecutionControl, ValidatedAdmin) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        Self::execute_admin_endpoint_with_control(self, metadata, operation)
    }

    fn execute_root_endpoint<'a, T, F, Fut>(
        &'a self,
        metadata: &'a ImplRequestMetadata,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ValidatedAdmin) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        Self::execute_root_endpoint(self, metadata, operation)
    }
}

impl HttpAdminApi for ProviderCommonApiImpl {
    fn execute_admin_endpoint<'a, T, F, Fut>(
        &'a self,
        metadata: &'a ImplRequestMetadata,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ValidatedAdmin) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        Self::execute_admin_endpoint(self, metadata, operation)
    }

    fn execute_admin_endpoint_with_control<'a, T, F, Fut>(
        &'a self,
        metadata: &'a ImplRequestMetadata,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ExecutionControl, ValidatedAdmin) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        Self::execute_admin_endpoint_with_control(self, metadata, operation)
    }

    fn execute_root_endpoint<'a, T, F, Fut>(
        &'a self,
        metadata: &'a ImplRequestMetadata,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ValidatedAdmin) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        Self::execute_root_endpoint(self, metadata, operation)
    }
}

pub(crate) fn request_context(request_meta: &ImplRequestMetadata) -> RequestContext {
    RequestContext {
        ip_address: request_meta.client_ip.map(|ip| ip.to_string()),
        user_agent: request_meta.user_agent.clone(),
    }
}

pub(crate) fn execute_admin_endpoint<'a, Api, GetApi, F, Fut, T>(
    state: &'a AppState,
    request_meta: RequestMetadata,
    get_api: GetApi,
    operation: F,
) -> BoxFuture<'a, Result<T, AppError>>
where
    Api: HttpAdminApi,
    T: Send + 'a,
    GetApi: FnOnce(&'a AppState) -> Result<Arc<Api>, AppError> + Send + 'a,
    F: FnOnce(Arc<Api>, ValidatedAdmin, RequestContext) -> Fut + Send + 'a,
    Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_meta.0;
        let ctx = request_context(&request_meta);
        let api = get_api(state)?;
        let executor = api.clone();
        executor
            .execute_admin_endpoint(&request_meta, move |validated| {
                operation(api, validated, ctx)
            })
            .await
            .map_err(AppError::from)
    }
    .boxed()
}

pub(crate) fn execute_admin_endpoint_with_control<'a, Api, GetApi, F, Fut, T>(
    state: &'a AppState,
    request_meta: RequestMetadata,
    get_api: GetApi,
    operation: F,
) -> BoxFuture<'a, Result<T, AppError>>
where
    Api: HttpAdminApi,
    T: Send + 'a,
    GetApi: FnOnce(&'a AppState) -> Result<Arc<Api>, AppError> + Send + 'a,
    F: FnOnce(Arc<Api>, ExecutionControl, ValidatedAdmin, RequestContext) -> Fut + Send + 'a,
    Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_meta.0;
        let ctx = request_context(&request_meta);
        let api = get_api(state)?;
        let executor = api.clone();
        executor
            .execute_admin_endpoint_with_control(
                &request_meta,
                move |request_control, validated| operation(api, request_control, validated, ctx),
            )
            .await
            .map_err(AppError::from)
    }
    .boxed()
}

pub(crate) fn execute_root_endpoint<'a, Api, GetApi, F, Fut, T>(
    state: &'a AppState,
    request_meta: RequestMetadata,
    get_api: GetApi,
    operation: F,
) -> BoxFuture<'a, Result<T, AppError>>
where
    Api: HttpAdminApi,
    T: Send + 'a,
    GetApi: FnOnce(&'a AppState) -> Result<Arc<Api>, AppError> + Send + 'a,
    F: FnOnce(Arc<Api>, ValidatedAdmin, RequestContext) -> Fut + Send + 'a,
    Fut: Future<Output = Result<T, ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_meta.0;
        let ctx = request_context(&request_meta);
        let api = get_api(state)?;
        let executor = api.clone();
        executor
            .execute_root_endpoint(&request_meta, move |validated| {
                operation(api, validated, ctx)
            })
            .await
            .map_err(AppError::from)
    }
    .boxed()
}
