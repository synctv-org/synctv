use axum::{extract::State, routing::post, Json, Router};
use futures::FutureExt;

use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::impls::EndpointRateLimitCategory;
use synctv_proto::providers::douyu::{ResolveRequest, ResolveResponse};

use super::common::execute_provider_user_endpoint;

pub(crate) fn douyu_routes() -> Router<AppState> {
    Router::new().route("/resolve", post(resolve))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/douyu/resolve",
        tag = "Provider",
        request_body = ResolveRequest,
        responses((status = 200, description = "Resolved Douyu live room", body = ResolveResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn resolve(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ResolveRequest>,
) -> AppResult<Json<ResolveResponse>> {
    let service = state.douyu_playback_provider_service.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |_authenticated| {
            async move {
                service
                    .resolve_resource(&req.resource)
                    .await
                    .map(crate::impls::providers::douyu::resolve_response)
            }
            .boxed()
        },
    )
    .await
}
