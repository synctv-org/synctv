use synctv_core::models::{MediaId, RoomId};

use crate::impls::client::ClientApiImpl;

impl ClientApiImpl {
    pub async fn get_live_proxy_source_config(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Option<synctv_core::models::LiveProxyMediaSourceConfig> {
        let media = match self
            .room_service
            .media_service()
            .get_room_media(room_id, media_id)
            .await
        {
            Ok(Some(media)) => media,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!(
                    room_id = %room_id,
                    media_id = %media_id,
                    error = %error,
                    "Failed to load room media for live proxy source URL"
                );
                return None;
            }
        };

        if media.source_provider != synctv_core::models::SourceProvider::LiveProxy {
            return None;
        }

        match media.source_config {
            synctv_core::models::MediaSourceConfig::LiveProxy(config) => Some(config),
            _ => None,
        }
    }
}
