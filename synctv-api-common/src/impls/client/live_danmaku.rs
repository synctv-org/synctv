use futures::StreamExt;

use super::{ClientApiImpl, RoomActor};
use crate::impls::ApiError;

pub type BilibiliLiveDanmakuStream = std::pin::Pin<
    Box<
        dyn futures::Stream<
                Item = Result<
                    synctv_proto::playback_provider::bilibili::BilibiliLiveDanmakuEvent,
                    ApiError,
                >,
            > + Send
            + 'static,
    >,
>;

fn live_danmaku_event_to_proto(
    event: synctv_core::provider::BilibiliLiveDanmakuEvent,
) -> synctv_proto::playback_provider::bilibili::BilibiliLiveDanmakuEvent {
    let r#type = match event.kind {
        synctv_core::provider::BilibiliLiveDanmakuEventKind::Unspecified => {
            synctv_proto::playback_provider::bilibili::BilibiliLiveDanmakuEventType::Unspecified
        }
        synctv_core::provider::BilibiliLiveDanmakuEventKind::Chat => {
            synctv_proto::playback_provider::bilibili::BilibiliLiveDanmakuEventType::Chat
        }
        synctv_core::provider::BilibiliLiveDanmakuEventKind::UserEnter => {
            synctv_proto::playback_provider::bilibili::BilibiliLiveDanmakuEventType::UserEnter
        }
        synctv_core::provider::BilibiliLiveDanmakuEventKind::Gift => {
            synctv_proto::playback_provider::bilibili::BilibiliLiveDanmakuEventType::Gift
        }
        synctv_core::provider::BilibiliLiveDanmakuEventKind::Heartbeat => {
            synctv_proto::playback_provider::bilibili::BilibiliLiveDanmakuEventType::Heartbeat
        }
        synctv_core::provider::BilibiliLiveDanmakuEventKind::Unknown => {
            synctv_proto::playback_provider::bilibili::BilibiliLiveDanmakuEventType::Unknown
        }
    };
    synctv_proto::playback_provider::bilibili::BilibiliLiveDanmakuEvent {
        format: event.format,
        event_type: event.event_type,
        user: event.user,
        message: event.message,
        timestamp: event.timestamp,
        gift_name: event.gift_name,
        gift_count: event.gift_count,
        online_count: event.online_count,
        r#type: r#type as i32,
    }
}

impl ClientApiImpl {
    pub async fn watch_bilibili_live_danmaku_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::playback_provider::bilibili::WatchBilibiliLiveDanmakuRequest,
    ) -> Result<BilibiliLiveDanmakuStream, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_room_permission(actor, synctv_core::models::RoomPermission::BROWSE_LIBRARY)
            .await?;
        let user_id = actor.require_user_id()?;
        let room_id = actor.room_id();
        let media_id = crate::impls::proto_validated_media_id(req.media_id, &self.public_id_codec)?;
        let media = self
            .room_service
            .media_service()
            .get_room_media(&room_id, &media_id)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound("Media not found".to_string()))?;
        self.room_service
            .ensure_client_usable_media(&media)
            .await
            .map_err(ApiError::from)?;
        if media.source_provider != synctv_core::models::SourceProvider::Bilibili {
            return Err(ApiError::InvalidInput(
                "Bilibili live danmaku requires Bilibili media".to_string(),
            ));
        }

        let provider = self
            .room_service
            .media_service()
            .providers_manager()
            .resolve_provider(
                synctv_core::models::SourceProvider::Bilibili,
                media.provider_instance_name.as_deref(),
            )
            .await
            .map_err(ApiError::from)?;
        let live_danmaku_provider =
            provider
                .as_bilibili_live_danmaku_provider()
                .ok_or_else(|| {
                    ApiError::ServiceUnavailable(
                        "Bilibili provider does not expose live danmaku".to_string(),
                    )
                })?;
        let ctx = self.attach_provider_store(
            self.build_provider_context(
                synctv_core::provider::ProviderActor::User(user_id),
                media.creator_id.as_ref(),
                &room_id,
                Some(media.id),
                media.provider_instance_name.as_deref(),
                None,
                None,
            )?,
            provider.as_ref(),
        );
        let stream = live_danmaku_provider
            .watch_bilibili_live_danmaku(&ctx, &media.source_config)
            .await
            .map_err(ApiError::from)?
            .map(|event| {
                event
                    .map(live_danmaku_event_to_proto)
                    .map_err(ApiError::from)
            });
        Ok(Box::pin(stream))
    }

    pub async fn watch_bilibili_dynamic_live_danmaku_for_actor(
        &self,
        actor: &RoomActor,
        playlist_id: &str,
        live_room_id: u64,
    ) -> Result<BilibiliLiveDanmakuStream, ApiError> {
        self.require_room_permission(actor, synctv_core::models::RoomPermission::BROWSE_LIBRARY)
            .await?;
        if live_room_id == 0 {
            return Err(ApiError::InvalidInput(
                "Bilibili live room_id must be non-zero".to_string(),
            ));
        }

        let user_id = actor.require_user_id()?;
        let room_id = actor.room_id();
        let playlist_id =
            crate::impls::proto_validated_playlist_id(playlist_id, &self.public_id_codec)?;
        let playlist = self
            .room_service
            .playlist_service()
            .get_room_playlist(&room_id, &playlist_id)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound("Playlist not found".to_string()))?;
        self.room_service
            .ensure_client_usable_playlist(&playlist)
            .await
            .map_err(ApiError::from)?;
        if !playlist.is_dynamic()
            || playlist.source_provider != Some(synctv_core::models::SourceProvider::Bilibili)
        {
            return Err(ApiError::InvalidInput(
                "Bilibili live danmaku requires a Bilibili dynamic playlist".to_string(),
            ));
        }

        let target = synctv_core::models::ProviderTarget::bilibili_live(live_room_id);
        let item = self
            .room_service
            .media_service()
            .resolve_dynamic_playlist_item(
                room_id,
                synctv_core::provider::ProviderActor::User(user_id),
                &playlist_id,
                &target,
            )
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound("Dynamic playlist item not found".to_string()))?;
        if !matches!(
            &item.source_config,
            synctv_core::models::MediaSourceConfig::Bilibili(
                synctv_core::models::BilibiliMediaSourceConfig::Live(_)
            )
        ) {
            return Err(ApiError::InvalidInput(
                "Dynamic playlist item is not a Bilibili live source".to_string(),
            ));
        }

        let provider = self
            .room_service
            .media_service()
            .providers_manager()
            .resolve_provider(
                synctv_core::models::SourceProvider::Bilibili,
                playlist.provider_instance_name.as_deref(),
            )
            .await
            .map_err(ApiError::from)?;
        let live_danmaku_provider =
            provider
                .as_bilibili_live_danmaku_provider()
                .ok_or_else(|| {
                    ApiError::ServiceUnavailable(
                        "Bilibili provider does not expose live danmaku".to_string(),
                    )
                })?;
        let ctx = self.attach_provider_store(
            self.build_provider_context(
                synctv_core::provider::ProviderActor::User(user_id),
                playlist.creator_id.as_ref(),
                &room_id,
                None,
                playlist.provider_instance_name.as_deref(),
                None,
                None,
            )?
            .with_playlist_id(playlist_id),
            provider.as_ref(),
        );
        let stream = live_danmaku_provider
            .watch_bilibili_live_danmaku(&ctx, &item.source_config)
            .await
            .map_err(ApiError::from)?
            .map(|event| {
                event
                    .map(live_danmaku_event_to_proto)
                    .map_err(ApiError::from)
            });
        Ok(Box::pin(stream))
    }
}
