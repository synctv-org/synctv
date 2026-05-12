//! Provider API implementations.

pub mod alist;
pub mod bilibili;
pub mod common;
pub mod emby;
pub mod playback;
pub mod proxy;
pub mod rtmp;

use std::sync::Arc;

pub use alist::AlistApiImpl;
pub use bilibili::BilibiliApiImpl;
pub use common::ProviderCommonApiImpl;
pub(crate) use common::{
    extract_instance_name, get_provider_binds, get_provider_credentials,
    resolve_bound_instance_name,
};
pub use emby::EmbyApiImpl;

pub(crate) fn publish_provider_credential_changed(
    event_service: Option<&Arc<dyn crate::runtime::RealtimeEventService>>,
    user_id: synctv_core::models::UserId,
    provider: &str,
    server_id: &str,
) {
    let Some(event_service) = event_service else {
        return;
    };

    let event = synctv_realtime::sync::RealtimeEvent::ProviderCredentialChanged {
        event_id: synctv_common::snanoid!(16),
        user_id,
        provider: provider.to_string(),
        server_id: server_id.to_string(),
        timestamp: chrono::Utc::now(),
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
