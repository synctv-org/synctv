//! Playback operations: play, pause, seek, speed, set_current_media, get_playback_state

use synctv_core::models::{PermissionBits, RoomId, UserId};

use super::ClientApiImpl;
use super::convert::{media_to_proto, playback_state_to_proto, playlist_to_proto};

impl ClientApiImpl {
    pub async fn play(
        &self,
        user_id: &str,
        room_id: &str,
        _req: crate::proto::client::PlayRequest,
    ) -> Result<crate::proto::client::PlayResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership and playback control permission
        self.room_service.check_permission(&rid, &uid, PermissionBits::PLAY_CONTROL).await
            .map_err(|e| format!("Forbidden: {e}"))?;

        let state = self.room_service.playback_service().set_playing(rid, uid, true).await
            .map_err(|e| e.to_string())?;

        Ok(crate::proto::client::PlayResponse {
            playback_state: Some(playback_state_to_proto(&state)),
        })
    }

    pub async fn pause(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<crate::proto::client::PauseResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership and playback control permission
        self.room_service.check_permission(&rid, &uid, PermissionBits::PLAY_CONTROL).await
            .map_err(|e| format!("Forbidden: {e}"))?;

        let state = self.room_service.playback_service().set_playing(rid, uid, false).await
            .map_err(|e| e.to_string())?;

        Ok(crate::proto::client::PauseResponse {
            playback_state: Some(playback_state_to_proto(&state)),
        })
    }

    pub async fn seek(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::SeekRequest,
    ) -> Result<crate::proto::client::SeekResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership and playback control permission
        self.room_service.check_permission(&rid, &uid, PermissionBits::PLAY_CONTROL).await
            .map_err(|e| format!("Forbidden: {e}"))?;

        self.room_service.playback_service().seek(rid.clone(), uid, req.current_time).await
            .map_err(|e| e.to_string())?;

        let state = self.room_service.get_playback_state(&rid).await.ok();
        Ok(crate::proto::client::SeekResponse {
            playback_state: state.map(|s| playback_state_to_proto(&s)),
        })
    }

    pub async fn set_playback_speed(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::SetPlaybackSpeedRequest,
    ) -> Result<crate::proto::client::SetPlaybackSpeedResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership and playback speed permission
        self.room_service.check_permission(&rid, &uid, PermissionBits::CHANGE_PLAYBACK_RATE).await
            .map_err(|e| format!("Forbidden: {e}"))?;

        self.room_service.playback_service().change_speed(rid.clone(), uid, req.speed).await
            .map_err(|e| e.to_string())?;

        let state = self.room_service.get_playback_state(&rid).await.ok();
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
    ) -> Result<crate::proto::client::SetCurrentMediaResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership and media switching permission
        self.room_service.check_permission(&rid, &uid, PermissionBits::CHANGE_CURRENT_MOVIE).await
            .map_err(|e| format!("Forbidden: {e}"))?;

        // If media_id is provided, switch to that media
        if !req.media_id.is_empty() {
            let media_id = synctv_core::models::MediaId::from_string(req.media_id);
            self.room_service.playback_service().switch_media(rid.clone(), uid, media_id).await
                .map_err(|e| e.to_string())?;
        }

        // Get the current root playlist and its item count
        let playlist = self.room_service.playlist_service().get_root_playlist(&rid).await
            .map_err(|e| e.to_string())?;
        let item_count = self.room_service.media_service().count_playlist_media(&playlist.id).await
            .map_err(|e| e.to_string())? as i32;

        // Get the currently playing media
        let playing_media = self.room_service.get_playing_media(&rid).await
            .map_err(|e| e.to_string())?;

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
    ) -> Result<crate::proto::client::GetPlaybackStateResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership
        self.room_service.check_membership(&rid, &uid).await
            .map_err(|e| format!("Forbidden: {e}"))?;

        let state = self.room_service.get_playback_state(&rid).await
            .map_err(|e| e.to_string())?;

        Ok(crate::proto::client::GetPlaybackStateResponse {
            playback_state: Some(playback_state_to_proto(&state)),
        })
    }
}
