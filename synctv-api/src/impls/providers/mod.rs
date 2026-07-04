//! Provider API implementations.

pub(crate) mod alist;
pub(crate) mod bilibili;
pub(crate) mod common;
pub(crate) mod emby;
pub(crate) mod playback;
pub(crate) mod rtmp;

use std::sync::Arc;

pub use alist::{AlistApiImpl, ProviderApiRuntime};
pub use bilibili::BilibiliApiImpl;
pub(crate) use common::{
    provider_instance_name_for_provider, provider_instance_name_for_response,
    resolve_bound_instance_name,
};
pub use common::{ProviderCommonApiImpl, ProviderCommonApiRuntime};
pub use emby::EmbyApiImpl;

pub(crate) fn publish_provider_credential_changed(
    event_service: &Arc<dyn crate::runtime::RealtimeEventService>,
    user_id: synctv_core::models::UserId,
    provider: &str,
    server_id: &str,
) {
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
