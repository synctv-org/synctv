use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::models::{
    EmbyPlaybackSession, FnosPlaybackSession, ProviderPlaybackSession,
    ProviderPlaybackSessionRecord, ProviderPlaybackSessionState, ProviderPlaybackStopReason,
    RoomId, SynologyPlaybackSession, UserId,
};
use crate::{Error, Result};

const ACTIVE_LEASE_SECONDS: i64 = 45;
const PAUSED_LEASE_SECONDS: i64 = 10 * 60;
const CLEANUP_CLAIM_SECONDS: f64 = 30.0;

#[derive(Debug, Clone)]
pub struct NewProviderPlaybackSession {
    pub room_id: RoomId,
    pub playback_generation: i64,
    pub provider_instance_name: Option<String>,
    pub credential_owner_id: UserId,
    pub resource_key: String,
    pub resource_version: Option<String>,
    pub session: ProviderPlaybackSession,
    pub paused: bool,
}

#[derive(Clone)]
pub struct ProviderPlaybackSessionRepository {
    pool: PgPool,
}

impl ProviderPlaybackSessionRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    fn lease_expires_at(paused: bool) -> DateTime<Utc> {
        Utc::now()
            + chrono::Duration::seconds(if paused {
                PAUSED_LEASE_SECONDS
            } else {
                ACTIVE_LEASE_SECONDS
            })
    }

    fn validate_required(value: &str, field: &str) -> Result<()> {
        if value.trim().is_empty() {
            return Err(Error::InvalidInput(format!(
                "provider playback {field} is required"
            )));
        }
        Ok(())
    }

    fn validate_server_id(server_id: &str) -> Result<()> {
        Self::validate_required(server_id, "server_id")?;
        if server_id.len() > 64 {
            return Err(Error::InvalidInput(
                "provider playback server_id exceeds 64 bytes".to_string(),
            ));
        }
        Ok(())
    }

    fn validate_session(session: &ProviderPlaybackSession) -> Result<()> {
        match session {
            ProviderPlaybackSession::Emby(session) => {
                Self::validate_server_id(&session.server_id)?;
                Self::validate_required(&session.item_id, "Emby item_id")?;
                Self::validate_required(&session.play_session_id, "Emby play_session_id")?;
                Self::validate_required(&session.playback_cache_key, "Emby playback cache key")
            }
            ProviderPlaybackSession::Fnos(FnosPlaybackSession::MediaSession {
                server_id,
                item_guid,
                ..
            }) => {
                Self::validate_server_id(server_id)?;
                Self::validate_required(item_guid, "FNOS item_guid")
            }
            ProviderPlaybackSession::Fnos(FnosPlaybackSession::Transcode {
                server_id,
                play_link,
                media_guid,
                video_guid,
                video_encoder,
                resolution,
                bitrate,
                channels,
                ..
            }) => {
                Self::validate_server_id(server_id)?;
                Self::validate_required(play_link, "FNOS play link")?;
                Self::validate_required(media_guid, "FNOS media_guid")?;
                Self::validate_required(video_guid, "FNOS video_guid")?;
                Self::validate_required(video_encoder, "FNOS video encoder")?;
                Self::validate_required(resolution, "FNOS resolution")?;
                if *bitrate < 0 {
                    return Err(Error::InvalidInput(
                        "FNOS transcode bitrate must be non-negative".to_string(),
                    ));
                }
                if !(1..=6).contains(channels) {
                    return Err(Error::InvalidInput(
                        "FNOS transcode channels must be between 1 and 6".to_string(),
                    ));
                }
                Ok(())
            }
            ProviderPlaybackSession::Synology(SynologyPlaybackSession::WatchSession {
                server_id,
                file_id,
            }) => {
                Self::validate_server_id(server_id)?;
                if *file_id <= 0 {
                    return Err(Error::InvalidInput(
                        "Synology watch file_id must be positive".to_string(),
                    ));
                }
                Ok(())
            }
            ProviderPlaybackSession::Synology(SynologyPlaybackSession::Stream {
                server_id,
                stream_id,
                format,
                file_id,
            }) => {
                Self::validate_server_id(server_id)?;
                Self::validate_required(stream_id, "Synology stream_id")?;
                Self::validate_required(format, "Synology stream format")?;
                if *file_id <= 0 {
                    return Err(Error::InvalidInput(
                        "Synology stream file_id must be positive".to_string(),
                    ));
                }
                Ok(())
            }
        }
    }

    fn validate(input: &NewProviderPlaybackSession) -> Result<()> {
        if input.playback_generation <= 0 {
            return Err(Error::InvalidInput(
                "provider playback session generation must be positive".to_string(),
            ));
        }
        Self::validate_required(&input.resource_key, "resource key")?;
        if input
            .resource_version
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(Error::InvalidInput(
                "provider playback resource version must contain data when present".to_string(),
            ));
        }
        if input
            .provider_instance_name
            .as_deref()
            .is_some_and(|value| value.is_empty() || value.len() > 64)
        {
            return Err(Error::InvalidInput(
                "provider instance name must contain 1 to 64 bytes when present".to_string(),
            ));
        }
        Self::validate_session(&input.session)
    }

    fn merge_session(
        mut incoming: ProviderPlaybackSession,
        existing: Option<ProviderPlaybackSession>,
    ) -> ProviderPlaybackSession {
        if let (
            ProviderPlaybackSession::Emby(EmbyPlaybackSession { start_reported, .. }),
            Some(ProviderPlaybackSession::Emby(EmbyPlaybackSession {
                start_reported: true,
                ..
            })),
        ) = (&mut incoming, existing)
        {
            *start_reported = true;
        }
        incoming
    }

    pub async fn upsert(&self, input: NewProviderPlaybackSession) -> Result<i64> {
        Self::validate(&input)?;
        let mut tx = self.pool.begin().await?;
        let current_generation = sqlx::query_scalar!(
            r#"SELECT playback_generation AS "playback_generation!"
               FROM room_playback_state
               WHERE room_id = $1
               FOR SHARE"#,
            input.room_id as RoomId,
        )
        .fetch_optional(&mut *tx)
        .await?;
        if current_generation != Some(input.playback_generation) {
            return Err(Error::Conflict(
                "playback source changed while provider session was allocated".to_string(),
            ));
        }

        sqlx::query!(
            r#"INSERT INTO provider_playback_sessions (
                   room_id, playback_generation, provider_instance_name,
                   credential_owner_id, resource_key, resource_version, session,
                   state, lease_expires_at, cleanup_attempts, cleanup_fence
               )
               VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8, 0, 0)
               ON CONFLICT DO NOTHING"#,
            input.room_id as RoomId,
            input.playback_generation,
            input.provider_instance_name.as_deref(),
            input.credential_owner_id as UserId,
            input.resource_key,
            input.resource_version.as_deref(),
            input.session.clone() as ProviderPlaybackSession,
            Self::lease_expires_at(input.paused),
        )
        .execute(&mut *tx)
        .await?;

        let existing = sqlx::query!(
            r#"SELECT id AS "id!", session AS "session!: ProviderPlaybackSession"
               FROM provider_playback_sessions
               WHERE room_id = $1
                 AND playback_generation = $2
                 AND provider_instance_name IS NOT DISTINCT FROM $3
                 AND resource_key = $4
               FOR UPDATE"#,
            input.room_id as RoomId,
            input.playback_generation,
            input.provider_instance_name.as_deref(),
            input.resource_key,
        )
        .fetch_one(&mut *tx)
        .await?;
        let session = Self::merge_session(input.session, Some(existing.session));
        sqlx::query!(
            r#"UPDATE provider_playback_sessions
               SET credential_owner_id = $2,
                   resource_version = $3,
                   session = $4,
                   state = 1,
                   lease_expires_at = $5,
                   stop_position = NULL,
                   stop_reason = NULL,
                   cleanup_attempts = 0,
                   next_cleanup_at = NULL,
                   cleanup_lease_until = NULL,
                   cleanup_fence = cleanup_fence
                       + CASE WHEN state IN (2, 3) THEN 1 ELSE 0 END
               WHERE id = $1"#,
            existing.id,
            input.credential_owner_id as UserId,
            input.resource_version.as_deref(),
            session as ProviderPlaybackSession,
            Self::lease_expires_at(input.paused),
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(existing.id)
    }

    pub async fn active_for_generation(
        &self,
        room_id: RoomId,
        playback_generation: i64,
    ) -> Result<Vec<ProviderPlaybackSessionRecord>> {
        Ok(sqlx::query_as!(
            ProviderPlaybackSessionRecord,
            r#"SELECT id AS "id!", room_id AS "room_id!: RoomId",
                      playback_generation AS "playback_generation!", provider_instance_name,
                      credential_owner_id AS "credential_owner_id!: UserId",
                      resource_key AS "resource_key!", resource_version,
                      session AS "session!: ProviderPlaybackSession",
                      state AS "state!: ProviderPlaybackSessionState",
                      lease_expires_at AS "lease_expires_at!", stop_position,
                      stop_reason AS "stop_reason?: ProviderPlaybackStopReason",
                      cleanup_attempts AS "cleanup_attempts!", next_cleanup_at,
                      cleanup_lease_until, cleanup_fence AS "cleanup_fence!",
                      created_at AS "created_at!", updated_at AS "updated_at!"
               FROM provider_playback_sessions
               WHERE room_id = $1 AND playback_generation = $2 AND state = 1
               ORDER BY id"#,
            room_id as RoomId,
            playback_generation,
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn mark_emby_started(&self, id: i64) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let session = sqlx::query_scalar!(
            r#"SELECT session AS "session!: ProviderPlaybackSession"
               FROM provider_playback_sessions WHERE id = $1 FOR UPDATE"#,
            id,
        )
        .fetch_one(&mut *tx)
        .await?;
        let ProviderPlaybackSession::Emby(mut session) = session else {
            return Err(Error::InvalidInput(
                "provider playback session is not an Emby session".to_string(),
            ));
        };
        session.start_reported = true;
        let session = ProviderPlaybackSession::Emby(session);
        sqlx::query!(
            "UPDATE provider_playback_sessions SET session = $2 WHERE id = $1",
            id,
            session as ProviderPlaybackSession,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn renew_generation(
        &self,
        room_id: RoomId,
        playback_generation: i64,
        paused: bool,
    ) -> Result<()> {
        sqlx::query!(
            r#"UPDATE provider_playback_sessions SET lease_expires_at = $3
               WHERE room_id = $1 AND playback_generation = $2 AND state = 1"#,
            room_id as RoomId,
            playback_generation,
            Self::lease_expires_at(paused),
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn request_generation_stop(
        &self,
        room_id: RoomId,
        playback_generation: i64,
        position: f64,
        reason: ProviderPlaybackStopReason,
    ) -> Result<()> {
        if !position.is_finite() || position < 0.0 {
            return Err(Error::InvalidInput(
                "provider playback stop position must be finite and non-negative".to_string(),
            ));
        }
        sqlx::query!(
            r#"UPDATE provider_playback_sessions
               SET state = 2, stop_position = $3, stop_reason = $4,
                   next_cleanup_at = NOW(), cleanup_lease_until = NULL
               WHERE room_id = $1 AND playback_generation = $2 AND state = 1"#,
            room_id as RoomId,
            playback_generation,
            position,
            i16::from(reason),
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn request_all_stop(&self, reason: ProviderPlaybackStopReason) -> Result<()> {
        sqlx::query!(
            r#"UPDATE provider_playback_sessions
               SET state = 2, stop_position = COALESCE(stop_position, 0.0),
                   stop_reason = $1, next_cleanup_at = NOW(), cleanup_lease_until = NULL
               WHERE state = 1"#,
            i16::from(reason),
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn claim_cleanup(&self, limit: i64) -> Result<Vec<ProviderPlaybackSessionRecord>> {
        if limit <= 0 {
            return Err(Error::InvalidInput(
                "provider playback cleanup limit must be positive".to_string(),
            ));
        }
        Ok(sqlx::query_as!(
            ProviderPlaybackSessionRecord,
            r#"WITH candidates AS (
                   SELECT id FROM provider_playback_sessions
                   WHERE ((state = 1 AND lease_expires_at <= NOW())
                          OR (state IN (2, 3) AND COALESCE(next_cleanup_at, NOW()) <= NOW()))
                     AND (cleanup_lease_until IS NULL OR cleanup_lease_until <= NOW())
                   ORDER BY COALESCE(next_cleanup_at, lease_expires_at), id
                   FOR UPDATE SKIP LOCKED LIMIT $1
               ), claimed AS (
                   UPDATE provider_playback_sessions session
                   SET state = CASE WHEN session.state = 1 THEN 2 ELSE session.state END,
                       stop_position = COALESCE(session.stop_position, 0.0),
                       stop_reason = COALESCE(session.stop_reason, 3),
                       next_cleanup_at = COALESCE(session.next_cleanup_at, NOW()),
                       cleanup_lease_until = NOW() + make_interval(secs => $2),
                       cleanup_fence = session.cleanup_fence + 1
                   FROM candidates WHERE session.id = candidates.id RETURNING session.*
               )
               SELECT id AS "id!", room_id AS "room_id!: RoomId",
                      playback_generation AS "playback_generation!", provider_instance_name,
                      credential_owner_id AS "credential_owner_id!: UserId",
                      resource_key AS "resource_key!", resource_version,
                      session AS "session!: ProviderPlaybackSession",
                      state AS "state!: ProviderPlaybackSessionState",
                      lease_expires_at AS "lease_expires_at!", stop_position,
                      stop_reason AS "stop_reason?: ProviderPlaybackStopReason",
                      cleanup_attempts AS "cleanup_attempts!", next_cleanup_at,
                      cleanup_lease_until, cleanup_fence AS "cleanup_fence!",
                      created_at AS "created_at!", updated_at AS "updated_at!"
               FROM claimed ORDER BY id"#,
            limit,
            CLEANUP_CLAIM_SECONDS,
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn delete_claimed(&self, id: i64, cleanup_fence: i64) -> Result<bool> {
        let result = sqlx::query!(
            r#"DELETE FROM provider_playback_sessions
               WHERE id = $1 AND cleanup_fence = $2
                 AND state IN (2, 3) AND cleanup_lease_until IS NOT NULL"#,
            id,
            cleanup_fence,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn delete_active(&self, id: i64) -> Result<bool> {
        let result = sqlx::query!(
            "DELETE FROM provider_playback_sessions WHERE id = $1 AND state = 1",
            id,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn retry_claimed(&self, id: i64, cleanup_fence: i64, attempts: i32) -> Result<bool> {
        let exponent = attempts.clamp(0, 8).cast_unsigned();
        let delay_seconds = f64::from(1_i32.checked_shl(exponent).unwrap_or(300).min(300));
        let result = sqlx::query!(
            r#"UPDATE provider_playback_sessions
               SET state = 3, cleanup_attempts = cleanup_attempts + 1,
                   next_cleanup_at = NOW() + make_interval(secs => $3),
                   cleanup_lease_until = NULL
               WHERE id = $1 AND cleanup_fence = $2
                 AND state IN (2, 3) AND cleanup_lease_until IS NOT NULL"#,
            id,
            cleanup_fence,
            delay_seconds,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }
}
