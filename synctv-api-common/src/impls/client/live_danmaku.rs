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

fn guest_is_current_bilibili_live_media(
    state: &synctv_core::models::RoomPlaybackState,
    media_id: &synctv_core::models::MediaId,
) -> bool {
    state.playing_media_id.as_ref() == Some(media_id)
}

fn guest_is_current_bilibili_dynamic_live(
    state: &synctv_core::models::RoomPlaybackState,
    playlist_id: &synctv_core::models::PlaylistId,
    target: &synctv_core::models::ProviderTarget,
) -> bool {
    state.playing_playlist_id.as_ref() == Some(playlist_id) && state.target.as_ref() == Some(target)
}

impl ClientApiImpl {
    async fn authorize_bilibili_live_danmaku_access(
        &self,
        actor: &RoomActor,
        guest_target_is_current: impl FnOnce(&synctv_core::models::RoomPlaybackState) -> bool,
    ) -> Result<(), ApiError> {
        match actor {
            RoomActor::User { .. } => {
                self.require_room_permission(
                    actor,
                    synctv_core::models::RoomPermission::BROWSE_LIBRARY,
                )
                .await
            }
            RoomActor::Guest(_) => {
                let room_id = actor.room_id();
                let state = self
                    .room_service
                    .get_playback_state(&room_id)
                    .await
                    .map_err(ApiError::from)?;
                if guest_target_is_current(&state) {
                    Ok(())
                } else {
                    Err(ApiError::Authorization(
                        "Guests can watch Bilibili live danmaku only for the current playback"
                            .to_string(),
                    ))
                }
            }
        }
    }

    pub async fn watch_bilibili_live_danmaku_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::playback_provider::bilibili::WatchBilibiliLiveDanmakuRequest,
        request_control: Option<&synctv_core::provider::ExecutionControl>,
    ) -> Result<BilibiliLiveDanmakuStream, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        match req.target.ok_or_else(|| {
            ApiError::InvalidInput("Bilibili live danmaku target is required".to_string())
        })? {
            synctv_proto::playback_provider::bilibili::watch_bilibili_live_danmaku_request::Target::MediaId(
                media_id,
            ) => {
                self.watch_bilibili_media_live_danmaku_for_actor(actor, media_id, request_control)
                    .await
            }
            synctv_proto::playback_provider::bilibili::watch_bilibili_live_danmaku_request::Target::Dynamic(dynamic) => {
                self.watch_bilibili_dynamic_live_danmaku_for_actor(actor, dynamic, request_control)
                    .await
            }
        }
    }

    async fn watch_bilibili_media_live_danmaku_for_actor(
        &self,
        actor: &RoomActor,
        media_id: String,
        request_control: Option<&synctv_core::provider::ExecutionControl>,
    ) -> Result<BilibiliLiveDanmakuStream, ApiError> {
        let room_id = actor.room_id();
        let media_id = crate::impls::proto_validated_media_id(media_id, &self.public_id_codec)?;
        self.authorize_bilibili_live_danmaku_access(actor, |state| {
            guest_is_current_bilibili_live_media(state, &media_id)
        })
        .await?;
        let provider_actor = super::provider_actor_for_viewer(actor.user_id());
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
                provider_actor,
                media.creator_id.as_ref(),
                &room_id,
                Some(media.id),
                media.provider_instance_name.as_deref(),
                None,
                request_control,
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

    async fn watch_bilibili_dynamic_live_danmaku_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::playback_provider::bilibili::BilibiliDynamicLiveDanmakuTarget,
        request_control: Option<&synctv_core::provider::ExecutionControl>,
    ) -> Result<BilibiliLiveDanmakuStream, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let room_id = actor.room_id();
        let playlist_id =
            crate::impls::proto_validated_playlist_id(&req.playlist_id, &self.public_id_codec)?;
        let target = synctv_core::models::ProviderTarget::bilibili_live(req.live_room_id);
        self.authorize_bilibili_live_danmaku_access(actor, |state| {
            guest_is_current_bilibili_dynamic_live(state, &playlist_id, &target)
        })
        .await?;
        let provider_actor = super::provider_actor_for_viewer(actor.user_id());
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

        let item = self
            .room_service
            .media_service()
            .resolve_dynamic_playlist_item(room_id, provider_actor, &playlist_id, &target)
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
                provider_actor,
                playlist.creator_id.as_ref(),
                &room_id,
                None,
                playlist.provider_instance_name.as_deref(),
                None,
                request_control,
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

#[cfg(test)]
mod tests {
    use super::{guest_is_current_bilibili_dynamic_live, guest_is_current_bilibili_live_media};

    #[test]
    fn guest_live_danmaku_access_is_limited_to_the_current_playback() {
        let room_id = synctv_core::models::RoomId::expect_positive(1);
        let media_id = synctv_core::models::MediaId::expect_positive(2);
        let playlist_id = synctv_core::models::PlaylistId::expect_positive(3);
        let target = synctv_core::models::ProviderTarget::bilibili_live(21_292_831);
        let mut state = synctv_core::models::RoomPlaybackState::new(room_id);

        state.playing_media_id = Some(media_id);
        assert!(guest_is_current_bilibili_live_media(&state, &media_id));
        assert!(!guest_is_current_bilibili_live_media(
            &state,
            &synctv_core::models::MediaId::expect_positive(4),
        ));

        state.playing_media_id = None;
        state.playing_playlist_id = Some(playlist_id);
        state.target = Some(target.clone());
        assert!(guest_is_current_bilibili_dynamic_live(
            &state,
            &playlist_id,
            &target,
        ));
        assert!(!guest_is_current_bilibili_dynamic_live(
            &state,
            &playlist_id,
            &synctv_core::models::ProviderTarget::bilibili_live(2),
        ));
    }

    #[test]
    fn dynamic_live_danmaku_request_requires_a_live_room_id() {
        let request = synctv_proto::playback_provider::bilibili::BilibiliDynamicLiveDanmakuTarget {
            playlist_id: "pl_abc123".to_string(),
            live_room_id: 0,
        };

        assert!(crate::impls::validate_proto_request(&request).is_err());
    }
}
