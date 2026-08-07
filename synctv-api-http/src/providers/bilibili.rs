//! Bilibili Provider HTTP Routes
//!
//! Provider API endpoints for Bilibili login, parse, etc.
//! Video, subtitle, manifest, and danmaku playback routes live under the
//! Bilibili playback-provider transport.

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;

use super::common::{
    execute_provider_user_endpoint, execute_provider_user_endpoint_with_control,
    provider_instance_name, provider_instance_name_from_request_field,
};
use crate::http::{middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState};
use synctv_api_common::impls::EndpointRateLimitCategory;
use synctv_proto::providers::bilibili::{
    CheckQrRequest, GetBindsResponse, ListFavoriteFoldersRequest, ListFollowedPgcRequest,
    ListHistoryRequest, ListLiveAreasRequest, ListPgcSeasonsRequest, ListPgcTimelineRequest,
    LoginQrRequest, LoginSmsRequest, LogoutRequest, ParseRequest, SendSmsRequest,
    StartSmsLoginRequest, UserInfoRequest,
};
use synctv_proto::providers::common::ProviderInstanceQuery;

/// Bilibili endpoints that authenticate, issue challenges, or mutate stored credentials.
pub(crate) fn bilibili_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login/qr/generate", post(login_qr))
        .route("/login/qr/check", post(qr_check))
        .route("/login/sms/start", post(sms_start))
        .route("/login/sms/send", post(sms_send))
        .route("/login/sms/login", post(sms_login))
        .route("/logout", post(logout))
}

/// Bilibili read/query endpoints.
pub(crate) fn bilibili_read_routes() -> Router<AppState> {
    Router::new()
        .route("/parse", post(parse))
        .route("/live/areas", post(list_live_areas))
        .route("/favorites", post(list_favorite_folders))
        .route("/pgc/followed", post(list_followed_pgc))
        .route("/history", post(list_history))
        .route("/pgc/timeline", post(list_pgc_timeline))
        .route("/pgc/seasons", post(list_pgc_seasons))
        .route("/me", post(user_info))
        .route("/binds", get(binds))
}

// Provider API handlers

/// Parse Bilibili URL (uses stored cookies)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/parse",
        tag = "Provider",
        request_body = ParseRequest,
        responses(
            (status = 200, description = "Bilibili media parsed", body = synctv_proto::providers::bilibili::ParseResponse),
            (status = 400, description = "Invalid parse request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Provider resource not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Provider request conflict", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn parse(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ParseRequest>,
) -> AppResult<Json<synctv_proto::providers::bilibili::ParseResponse>> {
    tracing::info!("Bilibili parse request");

    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.bilibili_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |control, authenticated| {
            async move {
                api.parse_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            }
            .boxed()
        },
    )
    .await
    .map_err(|e| {
        tracing::error!("Bilibili parse failed: {}", e);
        e
    })
}

/// List Bilibili live categories for dynamic playlist creation.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/live/areas",
        tag = "Provider",
        request_body = ListLiveAreasRequest,
        responses(
            (status = 200, description = "Bilibili live areas", body = synctv_proto::providers::bilibili::ListLiveAreasResponse),
            (status = 400, description = "Invalid provider instance", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_live_areas(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ListLiveAreasRequest>,
) -> AppResult<Json<synctv_proto::providers::bilibili::ListLiveAreasResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.bilibili_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |control, _| {
            async move {
                api.list_live_areas_with_context(req, instance_name.as_deref(), Some(&control))
                    .await
            }
            .boxed()
        },
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/favorites",
        tag = "Provider",
        request_body = ListFavoriteFoldersRequest,
        responses(
            (status = 200, description = "Bilibili favorite folders", body = synctv_proto::providers::bilibili::ListFavoriteFoldersResponse),
            (status = 400, description = "Invalid provider instance", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Bilibili credential required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_favorite_folders(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ListFavoriteFoldersRequest>,
) -> AppResult<Json<synctv_proto::providers::bilibili::ListFavoriteFoldersResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.bilibili_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |control, authenticated| {
            async move {
                api.list_favorite_folders_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            }
            .boxed()
        },
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/pgc/followed",
        tag = "Provider",
        request_body = ListFollowedPgcRequest,
        responses(
            (status = 200, description = "Followed Bilibili PGC seasons", body = synctv_proto::providers::bilibili::ListFollowedPgcResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Bilibili credential required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_followed_pgc(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ListFollowedPgcRequest>,
) -> AppResult<Json<synctv_proto::providers::bilibili::ListFollowedPgcResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.bilibili_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |control, authenticated| {
            async move {
                api.list_followed_pgc_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            }
            .boxed()
        },
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/history",
        tag = "Provider",
        request_body = ListHistoryRequest,
        responses(
            (status = 200, description = "Bilibili playback history cursor page", body = synctv_proto::providers::bilibili::ListHistoryResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Bilibili credential required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_history(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ListHistoryRequest>,
) -> AppResult<Json<synctv_proto::providers::bilibili::ListHistoryResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.bilibili_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |control, authenticated| {
            async move {
                api.list_history_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            }
            .boxed()
        },
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/pgc/timeline",
        tag = "Provider",
        request_body = ListPgcTimelineRequest,
        responses((status = 200, description = "Bilibili PGC timeline", body = synctv_proto::providers::bilibili::ListPgcTimelineResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_pgc_timeline(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ListPgcTimelineRequest>,
) -> AppResult<Json<synctv_proto::providers::bilibili::ListPgcTimelineResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.bilibili_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |control, authenticated| {
            async move {
                api.list_pgc_timeline_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            }
            .boxed()
        },
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/pgc/seasons",
        tag = "Provider",
        request_body = ListPgcSeasonsRequest,
        responses((status = 200, description = "Bilibili PGC season index", body = synctv_proto::providers::bilibili::ListPgcSeasonsResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_pgc_seasons(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ListPgcSeasonsRequest>,
) -> AppResult<Json<synctv_proto::providers::bilibili::ListPgcSeasonsResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.bilibili_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |control, authenticated| {
            async move {
                api.list_pgc_seasons_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            }
            .boxed()
        },
    )
    .await
}

/// Generate Bilibili QR code for login
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/login/qr/generate",
        tag = "Provider",
        request_body = LoginQrRequest,
        responses(
            (status = 200, description = "Bilibili login QR code generated", body = synctv_proto::providers::bilibili::QrCodeResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 400, description = "Invalid QR login request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Provider resource not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Provider request conflict", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn login_qr(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<LoginQrRequest>,
) -> AppResult<Json<synctv_proto::providers::bilibili::QrCodeResponse>> {
    tracing::info!("Bilibili login QR request");

    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.bilibili_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |control, _| {
            async move {
                api.login_qr_with_context(req, instance_name.as_deref(), Some(&control))
                    .await
            }
            .boxed()
        },
    )
    .await
    .map_err(|e| {
        tracing::error!("Failed to generate QR code: {}", e);
        e
    })
}

/// Check Bilibili QR code login status (persists cookies on success)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/login/qr/check",
        tag = "Provider",
        request_body = CheckQrRequest,
        responses(
            (status = 200, description = "Bilibili QR login status", body = synctv_proto::providers::bilibili::QrStatusResponse),
            (status = 400, description = "Invalid QR check request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Provider resource not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Provider request conflict", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn qr_check(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<CheckQrRequest>,
) -> AppResult<Json<synctv_proto::providers::bilibili::QrStatusResponse>> {
    tracing::info!("Bilibili QR check");

    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.bilibili_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |control, authenticated| {
            async move {
                api.check_qr_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            }
            .boxed()
        },
    )
    .await
    .map_err(|e| {
        tracing::error!("Failed to check QR status: {}", e);
        e
    })
}

/// Start SMS login
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/login/sms/start",
        tag = "Provider",
        request_body = StartSmsLoginRequest,
        responses(
            (status = 200, description = "Bilibili SMS login session started", body = synctv_proto::providers::bilibili::StartSmsLoginResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 400, description = "Invalid SMS login start request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Provider resource not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Provider request conflict", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn sms_start(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<StartSmsLoginRequest>,
) -> AppResult<Json<synctv_proto::providers::bilibili::StartSmsLoginResponse>> {
    tracing::info!("Bilibili SMS login start request");

    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.bilibili_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |control, _| {
            async move {
                api.start_sms_login_with_context(req, instance_name.as_deref(), Some(&control))
                    .await
            }
            .boxed()
        },
    )
    .await
    .map_err(|e| {
        tracing::error!("Failed to start SMS login: {}", e);
        e
    })
}

/// Send SMS verification code
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/login/sms/send",
        tag = "Provider",
        request_body = SendSmsRequest,
        responses(
            (status = 200, description = "Bilibili SMS sent", body = synctv_proto::providers::bilibili::SendSmsResponse),
            (status = 400, description = "Invalid SMS request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Provider resource not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Provider request conflict", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn sms_send(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<SendSmsRequest>,
) -> AppResult<Json<synctv_proto::providers::bilibili::SendSmsResponse>> {
    tracing::info!("Bilibili SMS send request");

    let api = state.shared_api_runtime.bilibili_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |control, _| {
            async move { api.send_sms_with_context(req, None, Some(&control)).await }.boxed()
        },
    )
    .await
    .map_err(|e| {
        tracing::error!("Failed to send SMS: {}", e);
        e
    })
}

/// Login with SMS code (persists cookies on success)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/login/sms/login",
        tag = "Provider",
        request_body = LoginSmsRequest,
        responses(
            (status = 200, description = "Bilibili SMS login succeeded", body = synctv_proto::providers::bilibili::LoginSmsResponse),
            (status = 400, description = "Invalid SMS login request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Provider resource not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Provider request conflict", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn sms_login(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<LoginSmsRequest>,
) -> AppResult<Json<synctv_proto::providers::bilibili::LoginSmsResponse>> {
    tracing::info!("Bilibili SMS login request");

    let api = state.shared_api_runtime.bilibili_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |control, authenticated| {
            async move {
                api.login_sms_with_context(&authenticated.user_id, req, None, Some(&control))
                    .await
            }
            .boxed()
        },
    )
    .await
    .map_err(|e| {
        tracing::error!("Failed to login with SMS: {}", e);
        e
    })
}

/// Get Bilibili user info (uses stored cookies)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/me",
        tag = "Provider",
        request_body = UserInfoRequest,
        responses(
            (status = 200, description = "Bilibili account info", body = synctv_proto::providers::bilibili::UserInfoResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Provider resource not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Provider request conflict", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn user_info(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<UserInfoRequest>,
) -> AppResult<Json<synctv_proto::providers::bilibili::UserInfoResponse>> {
    tracing::info!("Bilibili user info request");

    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.bilibili_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |control, authenticated| {
            async move {
                api.get_user_info_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            }
            .boxed()
        },
    )
    .await
    .map_err(|e| {
        tracing::error!("Failed to get user info: {}", e);
        e
    })
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/bilibili/binds",
        tag = "Provider",
        params(ProviderInstanceQuery),
        responses(
            (status = 200, description = "Saved Bilibili credentials", body = GetBindsResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 400, description = "Invalid provider instance query", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider bind request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider bind information unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn binds(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
) -> AppResult<Json<GetBindsResponse>> {
    let instance_name = provider_instance_name(&query)?.map(str::to_owned);
    let api = state.shared_api_runtime.bilibili_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |authenticated| {
            async move {
                tracing::info!("Bilibili binds request for user: {}", authenticated.user_id);
                api.get_binds(&authenticated.user_id, instance_name.as_deref())
                    .await
            }
            .boxed()
        },
    )
    .await
}

/// Logout (delete stored credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/logout",
        tag = "Provider",
        request_body = LogoutRequest,
        responses(
            (status = 200, description = "Bilibili credential removed", body = synctv_proto::providers::bilibili::LogoutResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Provider resource not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Provider request conflict", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn logout(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<LogoutRequest>,
) -> AppResult<Json<synctv_proto::providers::bilibili::LogoutResponse>> {
    tracing::info!("Bilibili logout request");

    let api = state.shared_api_runtime.bilibili_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |authenticated| async move { api.logout(&authenticated.user_id, req).await }.boxed(),
    )
    .await
    .map_err(|e| {
        tracing::error!("Bilibili logout failed: {}", e);
        e
    })
}

#[cfg(test)]
mod tests {
    use synctv_core::provider::BilibiliProvider;

    #[test]
    fn bilibili_server_id_is_global_for_all_instances() {
        assert_eq!(
            BilibiliProvider::credential_server_id(),
            BilibiliProvider::credential_server_id()
        );
    }
}
