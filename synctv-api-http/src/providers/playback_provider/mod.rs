pub(crate) mod acfun;
pub(crate) mod alist;
pub(crate) mod bilibili;
pub(crate) mod cctv;
pub(crate) mod cloudreve;
pub(crate) mod direct_url;
pub(crate) mod douyin;
pub(crate) mod douyu;
pub(crate) mod emby;
pub(crate) mod fnos;
pub(crate) mod huya;
pub(crate) mod live_proxy;
pub(crate) mod nextcloud;
pub(crate) mod qnap;
pub(crate) mod rtmp;
pub(crate) mod seafile;
pub(crate) mod synology;
pub(crate) mod tiktok;
pub(crate) mod transport;
pub(crate) mod truenas;
pub(crate) mod twitch;
pub(crate) mod youtube;

pub(crate) fn playback_provider_api_runtime(
    state: &crate::http::AppState,
) -> synctv_api_common::playback_provider::common::PlaybackProviderApiRuntime<'_> {
    synctv_api_common::playback_provider::common::PlaybackProviderApiRuntime {
        proxy_signing_key: &state.shared_api_runtime.proxy_signing_key,
        public_id_codec: &state.shared_api_runtime.public_id_codec,
        provider_stores: state.shared_api_runtime.provider_stores.as_ref(),
        user_service: &state.shared_api_runtime.client_api.user_service,
        playback_transport_services: &state.shared_api_runtime.playback_transport_services,
        proxy_http_client: &state.proxy_http_client,
        ssrf_guard: &state.ssrf_guard,
        proxy_slice_cache: &state.proxy_slice_cache,
    }
}

pub(crate) fn live_playback_api_runtime(
    state: &crate::http::AppState,
) -> synctv_api_common::playback_provider::common::LivePlaybackApiRuntime<'_> {
    synctv_api_common::playback_provider::common::LivePlaybackApiRuntime {
        proxy_signing_key: &state.shared_api_runtime.proxy_signing_key,
        live_streaming_infrastructure: state.live_streaming_infrastructure.as_ref(),
        connection_runtime: state.connection_manager.as_ref(),
        livestream_config: &state.runtime_settings.livestream,
        runtime_settings_store: state.runtime_settings_store.as_deref(),
    }
}
