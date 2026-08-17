//! Provider gRPC Services
//!
//! Provider-specific gRPC services for authentication, discovery, and browsing.
//! Room playback resources use the provider-specific services in
//! `playback_provider`.

use std::sync::Arc;

use futures::TryStreamExt;
use tonic::{Response, Status};

pub(crate) mod acfun;
pub(crate) mod alist;
pub(crate) mod bilibili;
pub(crate) mod cctv;
pub(crate) mod cloudreve;
pub(crate) mod common;
pub(crate) mod douyin;
pub(crate) mod douyu;
pub(crate) mod emby;
pub(crate) mod fnos;
pub(crate) mod huya;
pub(crate) mod nextcloud;
pub(crate) mod playback_provider;
pub(crate) mod qnap;
pub(crate) mod seafile;
pub(crate) mod synology;
pub(crate) mod tiktok;
pub(crate) mod truenas;
pub(crate) mod twitch;
pub(crate) mod youtube;

pub(crate) type ProviderResourceResponseStream =
    playback_provider::GrpcResponseStream<synctv_proto::providers::common::ResourceResponse>;

pub(crate) fn provider_request_metadata<T>(
    request: &tonic::Request<T>,
    runtime_settings: &synctv_api_common::ApiRuntimeSettings,
) -> Result<synctv_api_common::impls::RequestMetadata, tonic::Status> {
    crate::grpc::request_metadata(
        request,
        runtime_settings,
        Some(crate::grpc::grpc_unary_request_timeout()),
    )
}

pub(crate) fn provider_stream_request_metadata<T>(
    request: &tonic::Request<T>,
    runtime_settings: &synctv_api_common::ApiRuntimeSettings,
) -> Result<synctv_api_common::impls::RequestMetadata, Status> {
    crate::grpc::request_metadata(request, runtime_settings, None)
}

pub(crate) async fn execute_provider_resource_stream(
    state: Arc<playback_provider::PlaybackProviderGrpcState>,
    metadata: synctv_api_common::impls::RequestMetadata,
    operation: impl FnOnce(
            synctv_core::provider::ExecutionControl,
            synctv_core::service::AuthenticatedToken,
        ) -> futures::future::BoxFuture<
            'static,
            Result<
                synctv_core::provider::PlaybackTransportAction,
                synctv_api_common::impls::ApiError,
            >,
        > + Send
        + 'static,
) -> Result<Response<ProviderResourceResponseStream>, Status> {
    let operation_state = state.clone();
    let stream = state
        .shared_api_runtime
        .request_executor
        .execute_user_with_control(
            &metadata,
            synctv_api_common::impls::EndpointRateLimitCategory::Streaming,
            move |control, authenticated| async move {
                let action = operation(control.clone(), authenticated).await?;
                synctv_api_common::providers::common::provider_resource_action_to_stream(
                    synctv_api_common::providers::common::ProviderResourceExecutorDeps {
                        proxy_signing_key: &operation_state.shared_api_runtime.proxy_signing_key,
                        proxy_http_client: &operation_state.proxy_http_client,
                        ssrf_guard: &operation_state.ssrf_guard,
                        proxy_slice_cache: &operation_state.proxy_slice_cache,
                        request_control: Some(&control),
                    },
                    action,
                )
                .await
            },
        )
        .await
        .map_err(crate::grpc::map_api_error)?;

    Ok(Response::new(Box::pin(
        stream.map_err(crate::grpc::map_api_error),
    )))
}

pub(crate) fn provider_instance_name(instance_name: &str) -> Result<Option<String>, tonic::Status> {
    synctv_api_common::providers::common::provider_instance_name_from_query(
        &synctv_proto::providers::common::ProviderInstanceQuery {
            instance_name: instance_name.to_string(),
        },
    )
    .map(|name| name.map(str::to_owned))
    .map_err(crate::grpc::map_api_error)
}

#[cfg(test)]
mod tests {
    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    #[test]
    fn provider_instance_name_rejects_invalid_grpc_body_field() -> TestResult {
        let Err(status) = super::provider_instance_name("bad/name") else {
            return Err(test_error(
                "gRPC body instance_name validation accepted invalid input",
            ));
        };

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        Ok(())
    }

    #[test]
    fn provider_instance_name_trims_valid_value() -> TestResult {
        let instance_name = super::provider_instance_name("  alist-main  ")?;

        assert_eq!(instance_name.as_deref(), Some("alist-main"));
        Ok(())
    }
}
