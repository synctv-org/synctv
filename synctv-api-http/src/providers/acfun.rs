use axum::{extract::State, routing::post, Json, Router};
use futures::FutureExt;

use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use synctv_api_common::impls::EndpointRateLimitCategory;
use synctv_proto::providers::acfun::{ResolveRequest, ResolveResponse};

use super::common::execute_provider_user_endpoint;

pub(crate) fn acfun_routes() -> Router<AppState> {
    Router::new().route("/resolve", post(resolve))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/acfun/resolve",
        tag = "Provider",
        request_body = ResolveRequest,
        responses((status = 200, description = "Resolved AcFun video, bangumi, or live room", body = ResolveResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn resolve(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ResolveRequest>,
) -> AppResult<Json<ResolveResponse>> {
    let service = state.acfun_playback_provider_service.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |_authenticated| {
            async move {
                service
                    .resolve_resource(&req.resource)
                    .await
                    .map(synctv_api_common::providers::acfun::resolve_response)
            }
            .boxed()
        },
    )
    .await
}
