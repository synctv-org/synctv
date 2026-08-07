//! Provider API implementations.

pub mod acfun;
pub mod alist;
pub mod bilibili;
pub mod cctv;
pub mod cloudreve;
pub mod common;
pub mod douyin;
pub mod douyu;
pub mod emby;
pub mod fnos;
pub mod huya;
pub mod nextcloud;
pub mod playback;
pub mod qnap;
pub mod rtmp;
pub mod seafile;
pub mod synology;
pub mod tiktok;
pub mod truenas;
pub mod twitch;
pub mod youtube;

use std::sync::Arc;
use synctv_realtime::fanout::RealtimeEventService;

pub use alist::{AlistApiImpl, ProviderApiRuntime};
pub use bilibili::BilibiliApiImpl;
pub use cloudreve::CloudreveApiImpl;
pub use common::{
    provider_instance_name_for_provider, provider_instance_name_for_response,
    resolve_bound_instance_name,
};
pub use common::{ProviderCommonApiImpl, ProviderCommonApiRuntime};
pub use douyin::DouyinApiImpl;
pub use emby::EmbyApiImpl;
pub use fnos::FnosApiImpl;
pub use nextcloud::NextcloudApiImpl;
pub use qnap::QnapApiImpl;
pub use seafile::SeafileApiImpl;
pub use synology::SynologyApiImpl;
pub use tiktok::TikTokApiImpl;
pub use truenas::TrueNasApiImpl;
pub use twitch::TwitchApiImpl;
pub use youtube::YoutubeApiImpl;

pub fn publish_provider_credential_changed(
    event_service: &Arc<dyn RealtimeEventService>,
    user_id: synctv_core::models::UserId,
    provider: &str,
    server_id: &str,
) {
    let event = synctv_realtime::sync::RealtimeEvent::ProviderCredentialChanged {
        event_id: synctv_common::snanoid!(16),
        user_id,
        provider: provider.to_string(),
        server_id: server_id.to_string(),
        timestamp: synctv_core::SystemClock.now(),
    };
    let outcome = event_service.broadcast_outcome(event);
    if !outcome.delivered_to_any() || outcome.distributed_delivery_missed() {
        tracing::warn!(
            user_id = %user_id,
            provider,
            server_id,
            local_delivered = outcome.local_delivered(),
            distributed_available = outcome.distributed_available(),
            distributed_delivered = outcome.distributed_delivered(),
            "Provider credential change notification was not fully delivered"
        );
    }
}
