use futures::StreamExt;

use super::{ClientApiImpl, RoomActor};
use crate::impls::ApiError;

pub(crate) type BilibiliLiveDanmakuStream = std::pin::Pin<
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
        self.require_room_permission(actor, synctv_core::models::RoomPermission::VIEW_MEDIA)
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
                &user_id,
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
}
