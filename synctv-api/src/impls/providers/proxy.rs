use axum::http::HeaderMap;
use synctv_core::models::{RoomId, UserId};
use synctv_core::provider::proxy::{
    ProxyAction, ProxyProviderRegistry, ProxyRequestContext, ProxyServices,
};
use synctv_core::provider::store::ProviderStoreResolver;
use synctv_core::provider::ExecutionControl;
use synctv_core::proxy_signature::ProxySigningKey;
use synctv_core::service::UserService;
use synctv_proto::providers::common::ProviderProxyPathRequest;

use crate::impls::ApiError;

pub(crate) struct ProviderProxyResolution<'a> {
    pub(crate) path: ProviderProxyPathRequest,
    pub(crate) query_string: &'a str,
    pub(crate) request_headers: &'a HeaderMap,
    pub(crate) public_id_codec: &'a synctv_core::PublicIdCodec,
    pub(crate) proxy_signing_key: &'a ProxySigningKey,
    pub(crate) proxy_provider_registry: &'a ProxyProviderRegistry,
    pub(crate) provider_stores: &'a dyn ProviderStoreResolver,
    pub(crate) proxy_services: &'a ProxyServices,
    pub(crate) user_service: &'a UserService,
    pub(crate) request_control: &'a ExecutionControl,
}

fn parse_proxy_user_id(
    codec: &synctv_core::PublicIdCodec,
    value: &str,
) -> Result<UserId, ApiError> {
    crate::impls::parse_user_id_param(value, "user_id", codec)
}

fn parse_proxy_room_id(
    codec: &synctv_core::PublicIdCodec,
    value: &str,
) -> Result<RoomId, ApiError> {
    crate::impls::parse_room_id_param(value, "room_id", codec)
}

fn proxy_version_from_sub_path(sub_path: &str) -> Result<&str, ApiError> {
    let version = sub_path
        .split_once('/')
        .map_or(sub_path, |(version, _)| version);
    if version.trim().is_empty() {
        return Err(ApiError::InvalidInput(
            "provider proxy path must include a version".to_string(),
        ));
    }
    Ok(version)
}

pub(crate) fn map_proxy_membership_probe_error(err: synctv_core::Error) -> ApiError {
    match err {
        synctv_core::Error::Authorization(message)
            if message == synctv_core::repository::room_member::KICK_COOLDOWN_DENIED_MESSAGE =>
        {
            ApiError::Authorization(message)
        }
        synctv_core::Error::Authorization(_) => {
            ApiError::Authorization(synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string())
        }
        other => ApiError::from(other),
    }
}

pub(crate) async fn validate_fresh_provider_proxy_access(
    user_service: &UserService,
    proxy_services: &ProxyServices,
    room_id: &RoomId,
    user_id: &UserId,
) -> Result<(), ApiError> {
    let user = user_service
        .get_user(user_id)
        .await
        .map_err(ApiError::from)?;
    if user.status != synctv_core::models::UserStatus::Active || user.deleted_at.is_some() {
        return Err(ApiError::Authorization(
            synctv_common::messages::STALE_PROXY_ACCESS.to_string(),
        ));
    }

    let room = proxy_services
        .room_service
        .get_room(room_id)
        .await
        .map_err(ApiError::from)?;
    if room.is_banned || !room.status.is_active() {
        return Err(ApiError::Authorization(
            "Proxy URL is no longer valid for this room".to_string(),
        ));
    }

    proxy_services
        .room_service
        .check_membership(room_id, user_id)
        .await
        .map_err(map_proxy_membership_probe_error)
}

pub(crate) async fn resolve_provider_proxy_action(
    resolution: ProviderProxyResolution<'_>,
) -> Result<ProxyAction, ApiError> {
    crate::impls::validate_proto_request(&resolution.path)?;
    let ProviderProxyPathRequest {
        provider_name,
        sub_path,
    } = resolution.path;

    let version = proxy_version_from_sub_path(&sub_path)?;
    let claims = resolution
        .proxy_signing_key
        .parse_and_verify_query(resolution.query_string, &provider_name, version)
        .map_err(|error| {
            tracing::warn!(
                error = %error,
                message = synctv_common::messages::INVALID_PROXY_SIGNATURE,
                "Proxy signature validation failed"
            );
            ApiError::Authentication(synctv_common::messages::INVALID_PROXY_SIGNATURE.to_string())
        })?;

    let uid = parse_proxy_user_id(resolution.public_id_codec, &claims.user_id)?;
    let rid = parse_proxy_room_id(resolution.public_id_codec, &claims.room_id)?;
    validate_fresh_provider_proxy_access(
        resolution.user_service,
        resolution.proxy_services,
        &rid,
        &uid,
    )
    .await?;

    let proxy = resolution
        .proxy_provider_registry
        .get(&provider_name)
        .ok_or_else(|| ApiError::NotFound(synctv_common::messages::UNKNOWN_PROVIDER.to_string()))?;

    let store = resolution.provider_stores.load(&provider_name);
    let proxy_base = format!("/api/providers/proxy/{provider_name}");
    let ctx = ProxyRequestContext {
        sub_path: &sub_path,
        query_string: Some(resolution.query_string),
        store: Some(&store),
        proxy_base: &proxy_base,
        services: Some(resolution.proxy_services),
        public_id_codec: Some(resolution.public_id_codec),
        verified_claims: Some(&claims),
        request_context: Some(resolution.request_control),
        request_headers: resolution.request_headers,
    };

    proxy.resolve_proxy(&ctx).await.map_err(ApiError::from)
}

#[cfg(test)]
mod tests {
    use super::proxy_version_from_sub_path;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    #[test]
    fn proxy_version_from_sub_path_requires_version() -> TestResult {
        for sub_path in ["", "/stream", " /stream"] {
            let error =
                proxy_version_from_sub_path(sub_path).expect_err("sub_path should require version");
            assert!(
                error.to_string().contains("version"),
                "error should mention version, got: {error:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn proxy_version_from_sub_path_returns_first_segment() -> TestResult {
        let version = proxy_version_from_sub_path("v1/stream/master.m3u8")
            .map_err(|error| test_error(format!("{error:?}")))?;
        assert_eq!(version, "v1");
        Ok(())
    }

    #[test]
    fn proxy_version_from_sub_path_accepts_version_only_signed_rewrite_path() {
        let version = proxy_version_from_sub_path("version-only")
            .expect("version-only proxy rewrite path should expose version");
        assert_eq!(version, "version-only");
    }
}
