//! Playback operations: play, pause, seek, speed, set_current_media, get_playback_state

use synctv_core::models::playback::RoomPlaybackState;
use synctv_core::models::{PermissionBits, RoomId, UserId};

use super::convert::{media_to_proto, playback_state_to_proto, playlist_to_proto};
use super::ClientApiImpl;
use crate::impls::ApiError;

impl ClientApiImpl {
    /// Broadcast a PlaybackStateChanged cluster event via Redis pub/sub.
    ///
    /// Called after every playback mutation (play/pause/seek/speed/switch_media)
    /// so that users connected to other replicas see the change in real time.
    async fn broadcast_playback_state(
        &self,
        rid: &RoomId,
        uid: &UserId,
        state: &RoomPlaybackState,
    ) {
        if let Some(ref tx) = self.redis_publish_tx {
            let username = self
                .user_service
                .get_user(uid)
                .await
                .map(|u| u.username.clone())
                .unwrap_or_default();

            if let Err(e) = tx
                .send(synctv_cluster::sync::PublishRequest {
                    event: synctv_cluster::sync::ClusterEvent::PlaybackStateChanged {
                        event_id: nanoid::nanoid!(16),
                        room_id: rid.clone(),
                        user_id: uid.clone(),
                        username,
                        state: state.clone(),
                        timestamp: chrono::Utc::now(),
                    },
                })
                .await
            {
                tracing::error!(
                    room_id = %rid.as_str(),
                    "Failed to publish PlaybackStateChanged cluster event: {e}"
                );
            }
        }
    }

    pub async fn play(
        &self,
        user_id: &str,
        room_id: &str,
        _req: crate::proto::client::PlayRequest,
    ) -> Result<crate::proto::client::PlayResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership and playback control permission
        self.room_service
            .check_permission(&rid, &uid, PermissionBits::PLAY_CONTROL)
            .await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        let state = self
            .room_service
            .playback_service()
            .set_playing(rid.clone(), uid.clone(), true)
            .await
            .map_err(ApiError::from)?;

        self.broadcast_playback_state(&rid, &uid, &state).await;

        Ok(crate::proto::client::PlayResponse {
            playback_state: Some(playback_state_to_proto(&state)),
        })
    }

    pub async fn pause(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<crate::proto::client::PauseResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership and playback control permission
        self.room_service
            .check_permission(&rid, &uid, PermissionBits::PLAY_CONTROL)
            .await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        let state = self
            .room_service
            .playback_service()
            .set_playing(rid.clone(), uid.clone(), false)
            .await
            .map_err(ApiError::from)?;

        self.broadcast_playback_state(&rid, &uid, &state).await;

        Ok(crate::proto::client::PauseResponse {
            playback_state: Some(playback_state_to_proto(&state)),
        })
    }

    pub async fn seek(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::SeekRequest,
    ) -> Result<crate::proto::client::SeekResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership and playback control permission
        self.room_service
            .check_permission(&rid, &uid, PermissionBits::PLAY_CONTROL)
            .await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        self.room_service
            .playback_service()
            .seek(rid.clone(), uid.clone(), req.current_time)
            .await
            .map_err(ApiError::from)?;

        let state = self.room_service.get_playback_state(&rid).await.ok();

        if let Some(ref s) = state {
            self.broadcast_playback_state(&rid, &uid, s).await;
        }

        Ok(crate::proto::client::SeekResponse {
            playback_state: state.map(|s| playback_state_to_proto(&s)),
        })
    }

    pub async fn set_playback_speed(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::SetPlaybackSpeedRequest,
    ) -> Result<crate::proto::client::SetPlaybackSpeedResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership and playback speed permission
        self.room_service
            .check_permission(&rid, &uid, PermissionBits::CHANGE_PLAYBACK_RATE)
            .await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        self.room_service
            .playback_service()
            .change_speed(rid.clone(), uid.clone(), req.speed)
            .await
            .map_err(ApiError::from)?;

        let state = self.room_service.get_playback_state(&rid).await.ok();

        if let Some(ref s) = state {
            self.broadcast_playback_state(&rid, &uid, s).await;
        }

        Ok(crate::proto::client::SetPlaybackSpeedResponse {
            playback_state: state.map(|s| playback_state_to_proto(&s)),
        })
    }

    // set_current_media - Set which media to play (previously set_playing)
    pub async fn set_current_media(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::SetCurrentMediaRequest,
    ) -> Result<crate::proto::client::SetCurrentMediaResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership and media switching permission
        self.room_service
            .check_permission(&rid, &uid, PermissionBits::CHANGE_CURRENT_MOVIE)
            .await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        // If media_id is provided, switch to that media
        if !req.media_id.is_empty() {
            let media_id = synctv_core::models::MediaId::from_string(req.media_id);
            self.room_service
                .playback_service()
                .switch_media(rid.clone(), uid.clone(), media_id)
                .await
                .map_err(ApiError::from)?;
        }

        // Broadcast updated playback state to other replicas
        if let Ok(state) = self.room_service.get_playback_state(&rid).await {
            self.broadcast_playback_state(&rid, &uid, &state).await;
        }

        // Get the current root playlist and its item count
        let playlist = self
            .room_service
            .playlist_service()
            .get_root_playlist(&rid)
            .await
            .map_err(ApiError::from)?;
        let item_count = self
            .room_service
            .media_service()
            .count_playlist_media(&playlist.id)
            .await
            .map_err(ApiError::from)? as i32;

        // Get the currently playing media
        let playing_media = self
            .room_service
            .get_playing_media(&rid)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::SetCurrentMediaResponse {
            playlist: Some(playlist_to_proto(&playlist, item_count)),
            playing_media: playing_media.map(|m| media_to_proto(&m)),
        })
    }

    pub async fn get_playback_state(
        &self,
        user_id: &str,
        room_id: &str,
        _req: crate::proto::client::GetPlaybackStateRequest,
    ) -> Result<crate::proto::client::GetPlaybackStateResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        let state = self
            .room_service
            .get_playback_state(&rid)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::GetPlaybackStateResponse {
            playback_state: Some(playback_state_to_proto(&state)),
        })
    }
}
