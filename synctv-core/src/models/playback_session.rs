use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{RoomId, SourceProvider, UserId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "provider", content = "data", rename_all = "snake_case")]
pub enum ProviderPlaybackSession {
    Emby(EmbyPlaybackSession),
    Fnos(FnosPlaybackSession),
    Synology(SynologyPlaybackSession),
}

impl ProviderPlaybackSession {
    #[must_use]
    pub const fn provider(&self) -> SourceProvider {
        match self {
            Self::Emby(_) => SourceProvider::Emby,
            Self::Fnos(_) => SourceProvider::Fnos,
            Self::Synology(_) => SourceProvider::Synology,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmbyPlaybackSession {
    pub server_id: String,
    pub item_id: String,
    pub play_session_id: String,
    pub media_source_id: Option<String>,
    pub playback_cache_key: String,
    pub start_reported: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum FnosPlaybackSession {
    MediaSession {
        server_id: String,
        item_guid: String,
        media_guid: Option<String>,
    },
    Transcode {
        server_id: String,
        play_link: String,
        media_guid: String,
        video_guid: String,
        video_encoder: String,
        resolution: String,
        bitrate: i64,
        audio_guid: Option<String>,
        subtitle_guid: Option<String>,
        channels: i32,
        forced_sdr: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SynologyPlaybackSession {
    WatchSession {
        server_id: String,
        file_id: i64,
    },
    Stream {
        server_id: String,
        stream_id: String,
        format: String,
        file_id: i64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderPlaybackSessionState {
    Active,
    StopRequested,
    CleanupRetry,
}
i16_enum!(ProviderPlaybackSessionState, "invalid provider playback session state", {
    Active = 1,
    StopRequested = 2,
    CleanupRetry = 3,
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderPlaybackStopReason {
    TargetChanged,
    Stopped,
    LeaseExpired,
    Shutdown,
}
i16_enum!(ProviderPlaybackStopReason, "invalid provider playback stop reason", {
    TargetChanged = 1,
    Stopped = 2,
    LeaseExpired = 3,
    Shutdown = 4,
});

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderPlaybackSessionRecord {
    pub id: i64,
    pub room_id: RoomId,
    pub playback_generation: i64,
    pub provider_instance_name: Option<String>,
    pub credential_owner_id: UserId,
    pub resource_key: String,
    pub resource_version: Option<String>,
    pub session: ProviderPlaybackSession,
    pub state: ProviderPlaybackSessionState,
    pub lease_expires_at: DateTime<Utc>,
    pub stop_position: Option<f64>,
    pub stop_reason: Option<ProviderPlaybackStopReason>,
    pub cleanup_attempts: i32,
    pub next_cleanup_at: Option<DateTime<Utc>>,
    pub cleanup_lease_until: Option<DateTime<Utc>>,
    pub cleanup_fence: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_playback_session_round_trips_as_typed_json() -> std::result::Result<(), String> {
        let session = ProviderPlaybackSession::Fnos(FnosPlaybackSession::Transcode {
            server_id: "fnos-home".to_string(),
            play_link: "/video/transcode.m3u8".to_string(),
            media_guid: "media-1".to_string(),
            video_guid: "video-1".to_string(),
            video_encoder: "h264".to_string(),
            resolution: "1920x1080".to_string(),
            bitrate: 8_000_000,
            audio_guid: Some("audio-1".to_string()),
            subtitle_guid: None,
            channels: 2,
            forced_sdr: false,
        });

        let json = serde_json::to_value(&session).map_err(|error| error.to_string())?;
        assert_eq!(json["provider"], "fnos");
        assert_eq!(json["data"]["type"], "transcode");
        let decoded: ProviderPlaybackSession =
            serde_json::from_value(json).map_err(|error| error.to_string())?;
        assert_eq!(decoded, session);
        assert_eq!(decoded.provider(), SourceProvider::Fnos);
        Ok(())
    }
}
