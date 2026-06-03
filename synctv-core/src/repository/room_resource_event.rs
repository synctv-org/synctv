use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;

use crate::{
    models::{EventCursor, RoomId},
    Result,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i16)]
pub enum RoomResourceEventScope {
    Room = 1,
    User = 2,
    Global = 3,
}

impl RoomResourceEventScope {
    #[must_use]
    pub const fn as_i16(self) -> i16 {
        self as i16
    }
}

impl TryFrom<i16> for RoomResourceEventScope {
    type Error = String;

    fn try_from(value: i16) -> std::result::Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Room),
            2 => Ok(Self::User),
            3 => Ok(Self::Global),
            other => Err(format!("Unknown resource event scope: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewRoomResourceEvent {
    pub event_id: String,
    pub scope_type: RoomResourceEventScope,
    pub room_id: Option<i64>,
    pub user_id: Option<i64>,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub resource_type: String,
    pub resource_id: String,
    pub event_type: String,
    pub event_version: i64,
    pub aggregate_version: Option<i64>,
    pub actor_user_id: Option<i64>,
    pub payload: Option<Value>,
    pub summary: Value,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct RoomResourceEventLog {
    pub event_id: String,
    pub sequence: i64,
    pub resource_type: String,
    pub event_type: String,
    pub payload: Option<Value>,
}

#[derive(Clone)]
pub struct RoomResourceEventRepository {
    pool: PgPool,
}

impl RoomResourceEventRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn insert(&self, event: &NewRoomResourceEvent) -> Result<()> {
        insert_room_resource_event_with_executor(event, &self.pool).await
    }

    pub async fn latest_room_event_cursor_for_resource_types(
        &self,
        room_id: &RoomId,
        resource_types: &[&str],
    ) -> Result<EventCursor> {
        if resource_types.is_empty() {
            return Ok(EventCursor {
                event_id: None,
                sequence: 0,
            });
        }
        let resource_types: Vec<String> = resource_types
            .iter()
            .map(|resource_type| (*resource_type).to_string())
            .collect();

        let row = sqlx::query!(
            r"
            SELECT event_id, sequence
            FROM room_resource_events
            WHERE room_id = $1
              AND resource_type = ANY($2::TEXT[])
            ORDER BY sequence DESC
            LIMIT 1
            ",
            room_id.as_i64(),
            &resource_types
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map_or(
            EventCursor {
                event_id: None,
                sequence: 0,
            },
            |row| EventCursor {
                event_id: Some(row.event_id),
                sequence: row.sequence,
            },
        ))
    }

    pub async fn list_room_events_after_sequence_for_resource_types(
        &self,
        room_id: &RoomId,
        resource_types: &[&str],
        after_sequence: i64,
        limit: i32,
    ) -> Result<Vec<RoomResourceEventLog>> {
        if resource_types.is_empty() {
            return Ok(Vec::new());
        }

        let limit = limit.clamp(1, 500);
        let after_sequence = after_sequence.max(0);
        let resource_types: Vec<String> = resource_types
            .iter()
            .map(|resource_type| (*resource_type).to_string())
            .collect();

        let rows = sqlx::query_as_unchecked!(
            RoomResourceEventLog,
            r"
            SELECT event_id, sequence, resource_type, event_type, payload
            FROM room_resource_events
            WHERE room_id = $1
              AND sequence > $2
              AND resource_type = ANY($3::TEXT[])
            ORDER BY sequence ASC
            LIMIT $4
            ",
            room_id.as_i64(),
            after_sequence,
            &resource_types,
            limit
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    pub async fn retained_room_event_sequence_bounds_for_resource_types(
        &self,
        room_id: &RoomId,
        resource_types: &[&str],
    ) -> Result<Option<(i64, i64)>> {
        if resource_types.is_empty() {
            return Ok(None);
        }

        let resource_types: Vec<String> = resource_types
            .iter()
            .map(|resource_type| (*resource_type).to_string())
            .collect();

        let row = sqlx::query!(
            r"
            SELECT MIN(sequence) AS min_sequence, MAX(sequence) AS max_sequence
            FROM room_resource_events
            WHERE room_id = $1
              AND resource_type = ANY($2::TEXT[])
            ",
            room_id.as_i64(),
            &resource_types
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(row
            .min_sequence
            .zip(row.max_sequence)
            .map(|(min_sequence, max_sequence)| (min_sequence, max_sequence)))
    }

    pub async fn is_room_event_sequence_retained_for_resource_types(
        &self,
        room_id: &RoomId,
        resource_types: &[&str],
        after_sequence: i64,
    ) -> Result<bool> {
        let after_sequence = after_sequence.max(0);

        let Some((min_sequence, _max_sequence)) = self
            .retained_room_event_sequence_bounds_for_resource_types(room_id, resource_types)
            .await?
        else {
            return Ok(after_sequence == 0);
        };

        Ok(min_sequence <= after_sequence.saturating_add(1))
    }

    pub async fn room_event_cursor_by_event_id(
        &self,
        room_id: &RoomId,
        event_id: &str,
    ) -> Result<Option<EventCursor>> {
        let row = sqlx::query!(
            r"
            SELECT event_id, sequence
            FROM room_resource_events
            WHERE room_id = $1
              AND event_id = $2
            LIMIT 1
            ",
            room_id.as_i64(),
            event_id
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|row| EventCursor {
            event_id: Some(row.event_id),
            sequence: row.sequence,
        }))
    }

    pub async fn delete_older_than(&self, retention_seconds: i64) -> Result<u64> {
        let result = sqlx::query!(
            r"
            DELETE FROM room_resource_events
            WHERE created_at < NOW() - ($1::bigint::text || ' seconds')::interval
            ",
            retention_seconds.max(1)
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

pub async fn insert_room_resource_event_with_executor<'e, E>(
    event: &NewRoomResourceEvent,
    executor: E,
) -> Result<()>
where
    E: sqlx::PgExecutor<'e>,
{
    sqlx::query!(
        r"
        INSERT INTO room_resource_events (
            event_id, scope_type, room_id, user_id, aggregate_type, aggregate_id,
            resource_type, resource_id, event_type, event_version, aggregate_version,
            actor_user_id, payload, summary, occurred_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
        ON CONFLICT (event_id) DO NOTHING
        ",
        &event.event_id,
        event.scope_type.as_i16(),
        event.room_id,
        event.user_id,
        &event.aggregate_type,
        &event.aggregate_id,
        &event.resource_type,
        &event.resource_id,
        &event.event_type,
        event.event_version,
        event.aggregate_version,
        event.actor_user_id,
        event.payload.as_ref(),
        &event.summary,
        event.occurred_at
    )
    .execute(executor)
    .await?;
    Ok(())
}
