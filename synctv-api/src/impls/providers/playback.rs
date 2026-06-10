use synctv_core::models::{MediaId, RoomId};

use crate::impls::client::ClientApiImpl;

impl ClientApiImpl {
    pub async fn get_live_proxy_source_url(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Option<String> {
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

        if media.source_provider != "live_proxy" {
            return None;
        }

        media
            .source_config
            .get("url")
            .and_then(|value| value.as_str())
            .map(ToString::to_string)
    }
}
