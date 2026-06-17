use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::{
    repository::room_resource_event::{NewRoomResourceEvent, RoomResourceEventScope},
    Error, Result,
};

pub const REALTIME_OUTBOX_CHANNEL: &str = "realtime_outbox_new";
const DEFAULT_MAX_ATTEMPTS: i32 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeOutboxStatus {
    Pending,
    Processing,
    Sent,
    Dead,
}

impl RealtimeOutboxStatus {
    #[must_use]
    pub const fn as_i16(self) -> i16 {
        match self {
            Self::Pending => 1,
            Self::Processing => 2,
            Self::Sent => 3,
            Self::Dead => 4,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::Sent => "sent",
            Self::Dead => "dead",
        }
    }
}

impl TryFrom<i16> for RealtimeOutboxStatus {
    type Error = String;

    fn try_from(value: i16) -> std::result::Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Pending),
            2 => Ok(Self::Processing),
            3 => Ok(Self::Sent),
            4 => Ok(Self::Dead),
            other => Err(format!("Unknown realtime outbox status: {other}")),
        }
    }
}

impl From<RealtimeOutboxStatus> for i16 {
    fn from(value: RealtimeOutboxStatus) -> Self {
        value.as_i16()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewRealtimeOutboxEvent {
    pub id: String,
    pub enqueue_outbox: bool,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub event_type: String,
    pub event_version: i64,
    pub aggregate_version: Option<i64>,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealtimeOutboxEvent {
    pub id: String,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub event_type: String,
    pub event_version: i64,
    pub aggregate_version: Option<i64>,
    pub payload: Value,
    pub status: RealtimeOutboxStatus,
    pub attempts: i32,
    pub next_retry_at: DateTime<Utc>,
    pub locked_by: Option<String>,
    pub locked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub dispatched_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct RealtimeOutboxEventRow {
    id: String,
    aggregate_type: String,
    aggregate_id: String,
    event_type: String,
    event_version: i64,
    aggregate_version: Option<i64>,
    payload: Value,
    status: i16,
    attempts: i32,
    next_retry_at: DateTime<Utc>,
    locked_by: Option<String>,
    locked_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    dispatched_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

impl TryFrom<RealtimeOutboxEventRow> for RealtimeOutboxEvent {
    type Error = Error;

    fn try_from(row: RealtimeOutboxEventRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            aggregate_type: row.aggregate_type,
            aggregate_id: row.aggregate_id,
            event_type: row.event_type,
            event_version: row.event_version,
            aggregate_version: row.aggregate_version,
            payload: row.payload,
            status: RealtimeOutboxStatus::try_from(row.status).map_err(Error::Internal)?,
            attempts: row.attempts,
            next_retry_at: row.next_retry_at,
            locked_by: row.locked_by,
            locked_at: row.locked_at,
            created_at: row.created_at,
            dispatched_at: row.dispatched_at,
            last_error: row.last_error,
        })
    }
}

#[derive(Clone)]
pub struct RealtimeOutboxRepository {
    pool: PgPool,
}

impl RealtimeOutboxRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn insert(&self, event: &NewRealtimeOutboxEvent) -> Result<()> {
        self.insert_with_executor(event, &self.pool).await
    }

    pub async fn insert_with_executor<'e, E>(
        &self,
        event: &NewRealtimeOutboxEvent,
        executor: E,
    ) -> Result<()>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let resource_event = room_resource_event_from_outbox_event(event)?;
        let resource_event = resource_event.as_ref();
        sqlx::query!(
            r"
            WITH resource_insert AS (
                INSERT INTO room_resource_events (
                    event_id,
                    scope_type,
                    room_id,
                    user_id,
                    aggregate_type,
                    aggregate_id,
                    resource_type,
                    resource_id,
                    event_type,
                    event_version,
                    aggregate_version,
                    actor_user_id,
                    payload,
                    summary,
                    occurred_at
                )
                SELECT
                    $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24
                WHERE $10::text IS NOT NULL
                ON CONFLICT (event_id) DO NOTHING
            ),
            inserted AS (
                INSERT INTO realtime_outbox (
                    id,
                    aggregate_type,
                    aggregate_id,
                    event_type,
                    event_version,
                    aggregate_version,
                    payload,
                    status
                )
                SELECT $1, $2, $3, $4, $5, $6, $7, $8
                WHERE $25
                RETURNING id
            )
            SELECT pg_notify($9, id) FROM inserted
            ",
            &event.id,
            &event.aggregate_type,
            &event.aggregate_id,
            &event.event_type,
            event.event_version,
            event.aggregate_version,
            &event.payload,
            RealtimeOutboxStatus::Pending.as_i16(),
            REALTIME_OUTBOX_CHANNEL,
            resource_event.as_ref().map(|event| event.event_id.as_str()),
            resource_event
                .as_ref()
                .map(|event| event.scope_type.as_i16()),
            resource_event.as_ref().and_then(|event| event.room_id),
            resource_event.as_ref().and_then(|event| event.user_id),
            resource_event
                .as_ref()
                .map(|event| event.aggregate_type.as_str()),
            resource_event
                .as_ref()
                .map(|event| event.aggregate_id.as_str()),
            resource_event
                .as_ref()
                .map(|event| event.resource_type.as_str()),
            resource_event
                .as_ref()
                .map(|event| event.resource_id.as_str()),
            resource_event
                .as_ref()
                .map(|event| event.event_type.as_str()),
            resource_event.as_ref().map(|event| event.event_version),
            resource_event
                .as_ref()
                .and_then(|event| event.aggregate_version),
            resource_event
                .as_ref()
                .and_then(|event| event.actor_user_id),
            resource_event
                .as_ref()
                .and_then(|event| event.payload.as_ref()),
            resource_event.as_ref().map(|event| &event.summary),
            resource_event.as_ref().map(|event| event.occurred_at),
            event.enqueue_outbox,
        )
        .execute(executor)
        .await?;
        Ok(())
    }

    pub async fn claim_batch(
        &self,
        worker_id: &str,
        limit: i64,
    ) -> Result<Vec<RealtimeOutboxEvent>> {
        let rows = sqlx::query_as!(
            RealtimeOutboxEventRow,
            r#"
            WITH picked AS (
                SELECT id
                FROM realtime_outbox
                WHERE status = $2
                  AND next_retry_at <= NOW()
                ORDER BY created_at, id
                LIMIT $1
                FOR UPDATE SKIP LOCKED
            )
            UPDATE realtime_outbox o
            SET status = $3,
                locked_by = $4,
                locked_at = NOW()
            FROM picked
            WHERE o.id = picked.id
            RETURNING
                o.id,
                o.aggregate_type,
                o.aggregate_id,
                o.event_type,
                o.event_version,
                o.aggregate_version,
                o.payload AS "payload!: serde_json::Value",
                o.status,
                o.attempts,
                o.next_retry_at,
                o.locked_by,
                o.locked_at,
                o.created_at,
                o.dispatched_at,
                o.last_error
            "#,
            limit,
            RealtimeOutboxStatus::Pending.as_i16(),
            RealtimeOutboxStatus::Processing.as_i16(),
            worker_id,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn mark_sent(&self, id: &str) -> Result<()> {
        let result = sqlx::query!(
            r"
            UPDATE realtime_outbox
            SET status = $2,
                dispatched_at = NOW(),
                locked_by = NULL,
                locked_at = NULL,
                last_error = NULL
            WHERE id = $1
            ",
            id,
            RealtimeOutboxStatus::Sent.as_i16()
        )
        .execute(&self.pool)
        .await?;
        ensure_outbox_row_updated(result.rows_affected(), id, "mark_sent")?;
        Ok(())
    }

    pub async fn mark_failed(&self, id: &str, attempts: i32, error: &str) -> Result<()> {
        let next_attempt = attempts.saturating_add(1);
        let delay_seconds = retry_delay_seconds(next_attempt);
        let status = if next_attempt >= DEFAULT_MAX_ATTEMPTS {
            RealtimeOutboxStatus::Dead
        } else {
            RealtimeOutboxStatus::Pending
        };

        let result = sqlx::query!(
            r"
            UPDATE realtime_outbox
            SET status = $2,
                attempts = $3,
                next_retry_at = NOW() + ($4::BIGINT::TEXT || ' seconds')::INTERVAL,
                locked_by = NULL,
                locked_at = NULL,
                last_error = $5
            WHERE id = $1
            ",
            id,
            status.as_i16(),
            next_attempt,
            delay_seconds,
            error
        )
        .execute(&self.pool)
        .await?;
        ensure_outbox_row_updated(result.rows_affected(), id, "mark_failed")?;
        Ok(())
    }

    pub async fn requeue_stale_processing(&self, stale_after_seconds: i64) -> Result<u64> {
        let result = sqlx::query!(
            r"
            UPDATE realtime_outbox
            SET status = $2,
                locked_by = NULL,
                locked_at = NULL,
                next_retry_at = NOW()
            WHERE status = $3
              AND locked_at < NOW() - ($1::BIGINT::TEXT || ' seconds')::INTERVAL
            ",
            stale_after_seconds,
            RealtimeOutboxStatus::Pending.as_i16(),
            RealtimeOutboxStatus::Processing.as_i16()
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn notify_dispatchers(&self) -> Result<()> {
        sqlx::query!("SELECT pg_notify($1, '')", REALTIME_OUTBOX_CHANNEL)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    #[cfg(test)]
    pub async fn notify_dispatchers_with_executor<'e, E>(&self, executor: E) -> Result<()>
    where
        E: sqlx::PgExecutor<'e>,
    {
        sqlx::query!("SELECT pg_notify($1, '')", REALTIME_OUTBOX_CHANNEL)
            .execute(executor)
            .await?;
        Ok(())
    }
}

fn ensure_outbox_row_updated(rows_affected: u64, id: &str, operation: &str) -> Result<()> {
    if rows_affected == 1 {
        return Ok(());
    }

    Err(Error::Internal(format!(
        "Realtime outbox {operation} updated {rows_affected} rows for id {id}"
    )))
}

fn room_resource_event_from_outbox_event(
    event: &NewRealtimeOutboxEvent,
) -> Result<Option<NewRoomResourceEvent>> {
    let Some(room_id) = json_i64(&event.payload, "room_id") else {
        return Ok(None);
    };
    let Some(timestamp) = json_datetime(&event.payload, "timestamp")? else {
        return Ok(None);
    };

    let actor_user_id = actor_user_id(event);
    let resource = match event.event_type.as_str() {
        "playback_state_changed" => {
            let state = event.payload.get("state").unwrap_or(&Value::Null);
            (
                "playback_state".to_string(),
                room_id.to_string(),
                json_object(vec![
                    ("user_id", opt(json_i64(&event.payload, "user_id"))),
                    ("username", opt(json_string(&event.payload, "username"))),
                    ("playback_version", opt(json_i64(state, "version"))),
                    ("is_playing", opt(json_bool(state, "is_playing"))),
                    ("position", opt(json_f64(state, "position"))),
                    ("speed", opt(json_f64(state, "speed"))),
                    ("media_id", opt(json_i64(state, "playing_media_id"))),
                    ("playlist_id", opt(json_i64(state, "playing_playlist_id"))),
                    ("target_hash", opt(json_target_hash(state, "target"))),
                ]),
            )
        }
        "room_settings_changed" => (
            "room_settings".to_string(),
            room_id.to_string(),
            json_object(vec![
                ("user_id", opt(json_i64(&event.payload, "user_id"))),
                ("username", opt(json_string(&event.payload, "username"))),
                ("settings_version", opt(event.aggregate_version)),
            ]),
        ),
        "media_added" | "media_updated" => {
            let media_id = required_json_i64(&event.payload, "media_id", &event.event_type)?;
            (
                "media".to_string(),
                media_id.to_string(),
                json_object(vec![
                    ("user_id", opt(json_i64(&event.payload, "user_id"))),
                    ("username", opt(json_string(&event.payload, "username"))),
                    ("media_id", opt(Some(media_id))),
                    (
                        "media_title",
                        opt(json_string(&event.payload, "media_title")),
                    ),
                ]),
            )
        }
        "media_removed" => {
            let media_id = required_json_i64(&event.payload, "media_id", &event.event_type)?;
            (
                "media".to_string(),
                media_id.to_string(),
                json_object(vec![
                    ("user_id", opt(json_i64(&event.payload, "user_id"))),
                    ("username", opt(json_string(&event.payload, "username"))),
                    ("media_id", opt(Some(media_id))),
                ]),
            )
        }
        "media_removed_batch" | "playlist_reordered" => (
            "playlist_items".to_string(),
            room_id.to_string(),
            json_object(vec![
                ("user_id", opt(json_i64(&event.payload, "user_id"))),
                ("username", opt(json_string(&event.payload, "username"))),
                (
                    "media_ids",
                    event
                        .payload
                        .get("media_ids")
                        .cloned()
                        .unwrap_or(Value::Null),
                ),
            ]),
        ),
        "playlist_created" | "playlist_updated" => {
            let playlist = event.payload.get("playlist").unwrap_or(&Value::Null);
            let playlist_id = required_json_i64(playlist, "id", &event.event_type)?;
            (
                "playlist".to_string(),
                playlist_id.to_string(),
                json_object(vec![
                    ("user_id", opt(json_i64(&event.payload, "user_id"))),
                    ("username", opt(json_string(&event.payload, "username"))),
                    ("playlist_id", opt(Some(playlist_id))),
                    ("playlist_name", opt(json_string(playlist, "name"))),
                    ("parent_id", opt(json_i64(playlist, "parent_id"))),
                ]),
            )
        }
        "playlist_deleted" => {
            let playlist_id = required_json_i64(&event.payload, "playlist_id", &event.event_type)?;
            (
                "playlist".to_string(),
                playlist_id.to_string(),
                json_object(vec![
                    ("user_id", opt(json_i64(&event.payload, "user_id"))),
                    ("username", opt(json_string(&event.payload, "username"))),
                    ("playlist_id", opt(Some(playlist_id))),
                ]),
            )
        }
        "user_joined" => {
            let user_id = required_json_i64(&event.payload, "user_id", &event.event_type)?;
            (
                "room_member_events".to_string(),
                user_id.to_string(),
                json_object(vec![
                    ("member_kind", opt(Some("user".to_string()))),
                    ("user_id", opt(Some(user_id))),
                    ("username", opt(json_string(&event.payload, "username"))),
                    ("role", opt(json_i64(&event.payload, "role"))),
                    (
                        "permissions",
                        opt(permission_bits(&event.payload, "permissions")),
                    ),
                    (
                        "added_permissions",
                        opt(permission_bits(&event.payload, "added_permissions")),
                    ),
                    (
                        "removed_permissions",
                        opt(permission_bits(&event.payload, "removed_permissions")),
                    ),
                    (
                        "admin_added_permissions",
                        opt(permission_bits(&event.payload, "admin_added_permissions")),
                    ),
                    (
                        "admin_removed_permissions",
                        opt(permission_bits(&event.payload, "admin_removed_permissions")),
                    ),
                    (
                        "joined_at",
                        event
                            .payload
                            .get("joined_at")
                            .cloned()
                            .unwrap_or(Value::Null),
                    ),
                ]),
            )
        }
        "guest_joined" => {
            let guest_id = required_json_string(&event.payload, "guest_id", &event.event_type)?;
            (
                "room_member_events".to_string(),
                guest_id.clone(),
                json_object(vec![
                    ("member_kind", opt(Some("guest".to_string()))),
                    ("guest_id", opt(Some(guest_id))),
                    ("username", opt(json_string(&event.payload, "username"))),
                    ("role", opt(json_i64(&event.payload, "role"))),
                    (
                        "permissions",
                        opt(permission_bits(&event.payload, "permissions")),
                    ),
                    (
                        "joined_at",
                        event
                            .payload
                            .get("joined_at")
                            .cloned()
                            .unwrap_or(Value::Null),
                    ),
                ]),
            )
        }
        "user_left" => {
            let user_id = required_json_i64(&event.payload, "user_id", &event.event_type)?;
            (
                "room_member_events".to_string(),
                user_id.to_string(),
                json_object(vec![
                    ("member_kind", opt(Some("user".to_string()))),
                    ("user_id", opt(Some(user_id))),
                    ("username", opt(json_string(&event.payload, "username"))),
                ]),
            )
        }
        "guest_left" => {
            let guest_id = required_json_string(&event.payload, "guest_id", &event.event_type)?;
            (
                "room_member_events".to_string(),
                guest_id.clone(),
                json_object(vec![
                    ("member_kind", opt(Some("guest".to_string()))),
                    ("guest_id", opt(Some(guest_id))),
                    ("username", opt(json_string(&event.payload, "username"))),
                ]),
            )
        }
        "permission_changed" => {
            let target_user_id =
                required_json_i64(&event.payload, "target_user_id", &event.event_type)?;
            (
                "room_member_events".to_string(),
                target_user_id.to_string(),
                json_object(vec![
                    ("target_user_id", opt(Some(target_user_id))),
                    (
                        "target_username",
                        opt(json_string(&event.payload, "target_username")),
                    ),
                    ("changed_by", opt(json_i64(&event.payload, "changed_by"))),
                    (
                        "role_changed",
                        opt(json_bool(&event.payload, "role_changed")),
                    ),
                    (
                        "changed_by_username",
                        opt(json_string(&event.payload, "changed_by_username")),
                    ),
                    ("role", opt(json_i64(&event.payload, "role"))),
                    (
                        "new_permissions",
                        opt(permission_bits(&event.payload, "new_permissions")),
                    ),
                    (
                        "added_permissions",
                        opt(permission_bits(&event.payload, "added_permissions")),
                    ),
                    (
                        "removed_permissions",
                        opt(permission_bits(&event.payload, "removed_permissions")),
                    ),
                    (
                        "admin_added_permissions",
                        opt(permission_bits(&event.payload, "admin_added_permissions")),
                    ),
                    (
                        "admin_removed_permissions",
                        opt(permission_bits(&event.payload, "admin_removed_permissions")),
                    ),
                ]),
            )
        }
        "kick_publisher" => {
            let media_id = required_json_i64(&event.payload, "media_id", &event.event_type)?;
            (
                "playback_stream".to_string(),
                media_id.to_string(),
                json_object(vec![
                    ("media_id", opt(Some(media_id))),
                    ("reason", opt(json_string(&event.payload, "reason"))),
                ]),
            )
        }
        "kick_user_from_room" => {
            let user_id = required_json_i64(&event.payload, "user_id", &event.event_type)?;
            (
                "room_member_events".to_string(),
                user_id.to_string(),
                json_object(vec![
                    ("user_id", opt(Some(user_id))),
                    ("reason", opt(json_string(&event.payload, "reason"))),
                ]),
            )
        }
        "room_created" => (
            "room".to_string(),
            room_id.to_string(),
            json_object(vec![
                ("room_name", opt(json_string(&event.payload, "room_name"))),
                ("creator_id", opt(json_i64(&event.payload, "creator_id"))),
            ]),
        ),
        "room_deleted" => (
            "room".to_string(),
            room_id.to_string(),
            json_object(vec![(
                "deleted_by",
                opt(json_i64(&event.payload, "deleted_by")),
            )]),
        ),
        "room_banned" => (
            "room".to_string(),
            room_id.to_string(),
            json_object(vec![(
                "banned_by",
                opt(json_i64(&event.payload, "banned_by")),
            )]),
        ),
        "room_owner_inactive" => (
            "room".to_string(),
            room_id.to_string(),
            json_object(vec![
                ("owner_id", opt(json_i64(&event.payload, "owner_id"))),
                (
                    "triggered_by",
                    opt(json_i64(&event.payload, "triggered_by")),
                ),
            ]),
        ),
        _ => return Ok(None),
    };

    let (resource_type, resource_id, details) = resource;
    Ok(Some(NewRoomResourceEvent {
        event_id: event.id.clone(),
        scope_type: RoomResourceEventScope::Room,
        room_id: Some(room_id),
        user_id: None,
        aggregate_type: event.aggregate_type.clone(),
        aggregate_id: event.aggregate_id.clone(),
        resource_type: resource_type.clone(),
        resource_id,
        event_type: event.event_type.clone(),
        event_version: event.event_version,
        aggregate_version: event.aggregate_version,
        actor_user_id,
        payload: Some(event.payload.clone()),
        summary: serde_json::json!({
            "event_type": event.event_type,
            "room_id": room_id,
            "actor_user_id": actor_user_id,
            "resource_type": resource_type,
            "details": details,
        }),
        occurred_at: timestamp,
    }))
}

fn actor_user_id(event: &NewRealtimeOutboxEvent) -> Option<i64> {
    match event.event_type.as_str() {
        "permission_changed" => json_i64(&event.payload, "changed_by"),
        "room_created" => json_i64(&event.payload, "creator_id"),
        "room_deleted" => json_i64(&event.payload, "deleted_by"),
        "room_banned" => json_i64(&event.payload, "banned_by"),
        "room_owner_inactive" => json_i64(&event.payload, "triggered_by"),
        "kick_publisher" => None,
        _ => json_i64(&event.payload, "user_id"),
    }
}

fn json_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(value_i64)
}

fn value_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
}

fn json_f64(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn json_bool(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn json_target_hash(value: &Value, key: &str) -> Option<String> {
    let target = value.get(key)?.as_array()?;
    let mut bytes = Vec::with_capacity(target.len());
    for item in target {
        let byte = item.as_u64().and_then(|value| u8::try_from(value).ok())?;
        bytes.push(byte);
    }
    Some(hex::encode(Sha256::digest(&bytes)))
}

fn json_datetime(value: &Value, key: &str) -> Result<Option<DateTime<Utc>>> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|timestamp| {
            DateTime::parse_from_rfc3339(timestamp)
                .map(|timestamp| timestamp.with_timezone(&Utc))
                .map_err(|error| {
                    Error::Internal(format!("Invalid realtime event timestamp: {error}"))
                })
        })
        .transpose()
}

fn required_json_i64(value: &Value, key: &str, event_type: &str) -> Result<i64> {
    json_i64(value, key).ok_or_else(|| {
        Error::Internal(format!(
            "Realtime event {event_type} is missing numeric payload field {key}"
        ))
    })
}

fn required_json_string(value: &Value, key: &str, event_type: &str) -> Result<String> {
    json_string(value, key).ok_or_else(|| {
        Error::Internal(format!(
            "Realtime event {event_type} is missing string payload field {key}"
        ))
    })
}

fn permission_bits(value: &Value, key: &str) -> Option<i64> {
    value
        .get(key)
        .and_then(|value| json_i64(value, "bits").or_else(|| value_i64(value)))
}

fn json_object(entries: Vec<(&'static str, Value)>) -> Value {
    let mut object = serde_json::Map::with_capacity(entries.len());
    for (key, value) in entries {
        object.insert(key.to_string(), value);
    }
    Value::Object(object)
}

fn opt<T: Into<Value>>(value: Option<T>) -> Value {
    value.map_or(Value::Null, Into::into)
}

fn retry_delay_seconds(attempts: i32) -> i64 {
    let capped = attempts.clamp(1, 8);
    i64::from(2_i32.pow(capped.cast_unsigned())).min(300)
}
