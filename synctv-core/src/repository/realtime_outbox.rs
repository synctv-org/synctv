use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};

use crate::{
    models::{RealtimeEvent, RoomPermissionSet},
    repository::{
        room_resource_event::{
            NewRoomResourceEvent, RoomMemberResourceSummary, RoomResourceEventPayload,
            RoomResourceEventScope, RoomResourceEventSummary, RoomResourceEventSummaryDetails,
            RoomResourceKind,
        },
        JsonbArray, OptionalJsonbArray,
    },
    Error, Result,
};

pub const REALTIME_OUTBOX_CHANNEL: &str = "realtime_outbox_new";
const DEFAULT_MAX_ATTEMPTS: i32 = 12;
const INSERT_MANY_CHUNK_SIZE: usize = 1000;

struct RoomResourceEventBatch {
    event_ids: Vec<String>,
    scope_types: Vec<i16>,
    room_ids: Vec<Option<i64>>,
    user_ids: Vec<Option<i64>>,
    aggregate_types: Vec<String>,
    aggregate_ids: Vec<String>,
    resource_types: Vec<String>,
    resource_ids: Vec<String>,
    event_types: Vec<String>,
    event_versions: Vec<i64>,
    aggregate_versions: Vec<Option<i64>>,
    actor_user_ids: Vec<Option<i64>>,
    payloads: OptionalJsonbArray<RoomResourceEventPayload>,
    summaries: JsonbArray<RoomResourceEventSummary>,
    occurred_ats: Vec<DateTime<Utc>>,
}

impl RoomResourceEventBatch {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            event_ids: Vec::with_capacity(capacity),
            scope_types: Vec::with_capacity(capacity),
            room_ids: Vec::with_capacity(capacity),
            user_ids: Vec::with_capacity(capacity),
            aggregate_types: Vec::with_capacity(capacity),
            aggregate_ids: Vec::with_capacity(capacity),
            resource_types: Vec::with_capacity(capacity),
            resource_ids: Vec::with_capacity(capacity),
            event_types: Vec::with_capacity(capacity),
            event_versions: Vec::with_capacity(capacity),
            aggregate_versions: Vec::with_capacity(capacity),
            actor_user_ids: Vec::with_capacity(capacity),
            payloads: OptionalJsonbArray::with_capacity(capacity),
            summaries: JsonbArray::with_capacity(capacity),
            occurred_ats: Vec::with_capacity(capacity),
        }
    }

    fn push(&mut self, event: &NewRoomResourceEvent) -> Result<()> {
        self.event_ids.push(event.event_id.clone());
        self.scope_types.push(event.scope_type.as_i16());
        self.room_ids.push(event.room_id);
        self.user_ids.push(event.user_id);
        self.aggregate_types.push(event.aggregate_type.clone());
        self.aggregate_ids.push(event.aggregate_id.clone());
        self.resource_types
            .push(event.resource_type.as_db_str().to_string());
        self.resource_ids.push(event.resource_id.clone());
        self.event_types.push(event.event_type.clone());
        self.event_versions.push(event.event_version);
        self.aggregate_versions.push(event.aggregate_version);
        self.actor_user_ids.push(event.actor_user_id);
        self.payloads.push(event.payload.as_ref())?;
        self.summaries.push(&event.summary)?;
        self.occurred_ats.push(event.occurred_at);
        Ok(())
    }
}

struct RealtimeOutboxEventBatch {
    ids: Vec<String>,
    aggregate_types: Vec<String>,
    aggregate_ids: Vec<String>,
    event_types: Vec<String>,
    event_versions: Vec<i64>,
    aggregate_versions: Vec<Option<i64>>,
    payloads: JsonbArray<RealtimeEvent>,
    statuses: Vec<i16>,
}

impl RealtimeOutboxEventBatch {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            ids: Vec::with_capacity(capacity),
            aggregate_types: Vec::with_capacity(capacity),
            aggregate_ids: Vec::with_capacity(capacity),
            event_types: Vec::with_capacity(capacity),
            event_versions: Vec::with_capacity(capacity),
            aggregate_versions: Vec::with_capacity(capacity),
            payloads: JsonbArray::with_capacity(capacity),
            statuses: Vec::with_capacity(capacity),
        }
    }

    fn push(&mut self, event: &NewRealtimeOutboxEvent) -> Result<()> {
        self.ids.push(event.id.clone());
        self.aggregate_types.push(event.aggregate_type.clone());
        self.aggregate_ids.push(event.aggregate_id.clone());
        self.event_types.push(event.event_type.clone());
        self.event_versions.push(event.event_version);
        self.aggregate_versions.push(event.aggregate_version);
        self.payloads.push(&event.payload)?;
        self.statuses.push(RealtimeOutboxStatus::Pending.as_i16());
        Ok(())
    }
}

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
    pub payload: RealtimeEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealtimeOutboxEvent {
    pub id: String,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub event_type: String,
    pub event_version: i64,
    pub aggregate_version: Option<i64>,
    pub payload: RealtimeEvent,
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
    payload: sqlx::types::Json<RealtimeEvent>,
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
            payload: row.payload.0,
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
        let payload = sqlx::types::Json(&event.payload);
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
            payload as _,
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
                .map(|event| event.resource_type.as_db_str()),
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
                .and_then(|event| event.payload.as_ref().map(sqlx::types::Json)) as _,
            resource_event
                .as_ref()
                .map(|event| sqlx::types::Json(&event.summary)) as _,
            resource_event.as_ref().map(|event| event.occurred_at),
            event.enqueue_outbox,
        )
        .execute(executor)
        .await?;
        Ok(())
    }

    pub async fn insert_many(&self, events: &[NewRealtimeOutboxEvent]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        self.insert_many_with_executor(events, &mut tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn insert_many_with_executor(
        &self,
        events: &[NewRealtimeOutboxEvent],
        executor: &mut PgConnection,
    ) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }

        let resource_events = events
            .iter()
            .map(room_resource_event_from_outbox_event)
            .collect::<Result<Vec<_>>>()?;

        insert_resource_events_many(&resource_events, executor).await?;
        insert_realtime_outbox_events_many(events, executor).await?;
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
                WHERE status = 1
                  AND next_retry_at <= NOW()
                ORDER BY next_retry_at, created_at, id
                LIMIT $1
                FOR UPDATE SKIP LOCKED
            )
            UPDATE realtime_outbox o
            SET status = $2,
                locked_by = $3,
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
                o.payload AS "payload!: sqlx::types::Json<RealtimeEvent>",
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
            RealtimeOutboxStatus::Processing.as_i16(),
            worker_id,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn mark_sent(&self, id: &str, worker_id: &str) -> Result<()> {
        let result = sqlx::query!(
            r"
            UPDATE realtime_outbox
            SET status = $2,
                dispatched_at = NOW(),
                locked_by = NULL,
                locked_at = NULL,
                last_error = NULL
            WHERE id = $1 AND status = $3 AND locked_by = $4
            ",
            id,
            RealtimeOutboxStatus::Sent.as_i16(),
            RealtimeOutboxStatus::Processing.as_i16(),
            worker_id,
        )
        .execute(&self.pool)
        .await?;
        ensure_outbox_row_updated(result.rows_affected(), id, "mark_sent")?;
        Ok(())
    }

    pub async fn mark_failed(
        &self,
        id: &str,
        worker_id: &str,
        attempts: i32,
        error: &str,
    ) -> Result<()> {
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
            WHERE id = $1 AND status = $6 AND locked_by = $7
            ",
            id,
            status.as_i16(),
            next_attempt,
            delay_seconds,
            error,
            RealtimeOutboxStatus::Processing.as_i16(),
            worker_id,
        )
        .execute(&self.pool)
        .await?;
        ensure_outbox_row_updated(result.rows_affected(), id, "mark_failed")?;
        Ok(())
    }

    pub async fn release_claims(&self, worker_id: &str, ids: &[String]) -> Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let result = sqlx::query!(
            r"
            UPDATE realtime_outbox
            SET status = $1,
                locked_by = NULL,
                locked_at = NULL,
                next_retry_at = NOW()
            WHERE status = $2 AND locked_by = $3 AND id = ANY($4::text[])
            ",
            RealtimeOutboxStatus::Pending.as_i16(),
            RealtimeOutboxStatus::Processing.as_i16(),
            worker_id,
            ids,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn requeue_stale_processing(&self, stale_after_seconds: i64) -> Result<u64> {
        let result = sqlx::query!(
            r"
            UPDATE realtime_outbox
            SET status = $2,
                locked_by = NULL,
                locked_at = NULL,
                next_retry_at = NOW()
            WHERE status = 2
              AND locked_at < NOW() - ($1::BIGINT::TEXT || ' seconds')::INTERVAL
            ",
            stale_after_seconds,
            RealtimeOutboxStatus::Pending.as_i16(),
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
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

async fn insert_resource_events_many(
    resource_events: &[Option<NewRoomResourceEvent>],
    executor: &mut PgConnection,
) -> Result<()> {
    let events = resource_events
        .iter()
        .filter_map(Option::as_ref)
        .collect::<Vec<_>>();
    if events.is_empty() {
        return Ok(());
    }

    for chunk in events.chunks(INSERT_MANY_CHUNK_SIZE) {
        insert_resource_event_chunk(chunk, executor).await?;
    }

    Ok(())
}

async fn insert_resource_event_chunk(
    events: &[&NewRoomResourceEvent],
    executor: &mut PgConnection,
) -> Result<()> {
    let mut batch = RoomResourceEventBatch::with_capacity(events.len());
    for event in events {
        batch.push(event)?;
    }

    sqlx::query!(
        r#"
        INSERT INTO room_resource_events (
            event_id, scope_type, room_id, user_id, aggregate_type, aggregate_id,
            resource_type, resource_id, event_type, event_version, aggregate_version,
            actor_user_id, payload, summary, occurred_at
        )
        SELECT
            event_id::text,
            scope_type::smallint,
            room_id::bigint,
            user_id::bigint,
            aggregate_type::text,
            aggregate_id::text,
            resource_type::text,
            resource_id::text,
            event_type::text,
            event_version::bigint,
            aggregate_version::bigint,
            actor_user_id::bigint,
            payload::jsonb,
            summary::jsonb,
            occurred_at::timestamptz
        FROM UNNEST(
            $1::text[],
            $2::smallint[],
            $3::bigint[],
            $4::bigint[],
            $5::text[],
            $6::text[],
            $7::text[],
            $8::text[],
            $9::text[],
            $10::bigint[],
            $11::bigint[],
            $12::bigint[],
            $13::jsonb[],
            $14::jsonb[],
            $15::timestamptz[]
        ) AS t(
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
        ON CONFLICT (event_id) DO NOTHING
        "#,
        &batch.event_ids,
        &batch.scope_types,
        &batch.room_ids as _,
        &batch.user_ids as _,
        &batch.aggregate_types,
        &batch.aggregate_ids,
        &batch.resource_types,
        &batch.resource_ids,
        &batch.event_types,
        &batch.event_versions,
        &batch.aggregate_versions as _,
        &batch.actor_user_ids as _,
        batch.payloads.as_slice() as _,
        batch.summaries.as_slice(),
        &batch.occurred_ats,
    )
    .execute(executor)
    .await?;
    Ok(())
}

async fn insert_realtime_outbox_events_many(
    events: &[NewRealtimeOutboxEvent],
    executor: &mut PgConnection,
) -> Result<()> {
    let enqueue_events = events
        .iter()
        .filter(|event| event.enqueue_outbox)
        .collect::<Vec<_>>();
    if enqueue_events.is_empty() {
        return Ok(());
    }

    for chunk in enqueue_events.chunks(INSERT_MANY_CHUNK_SIZE) {
        insert_realtime_outbox_event_chunk(chunk, executor).await?;
    }

    Ok(())
}

async fn insert_realtime_outbox_event_chunk(
    events: &[&NewRealtimeOutboxEvent],
    executor: &mut PgConnection,
) -> Result<()> {
    let mut batch = RealtimeOutboxEventBatch::with_capacity(events.len());
    for event in events {
        batch.push(event)?;
    }

    sqlx::query!(
        r#"
        WITH inserted AS (
            INSERT INTO realtime_outbox (
                id, aggregate_type, aggregate_id, event_type, event_version,
                aggregate_version, payload, status
            )
            SELECT
                id::text,
                aggregate_type::text,
                aggregate_id::text,
                event_type::text,
                event_version::bigint,
                aggregate_version::bigint,
                payload::jsonb,
                status::smallint
            FROM UNNEST(
                $1::text[],
                $2::text[],
                $3::text[],
                $4::text[],
                $5::bigint[],
                $6::bigint[],
                $7::jsonb[],
                $8::smallint[]
            ) AS t(
                id,
                aggregate_type,
                aggregate_id,
                event_type,
                event_version,
                aggregate_version,
                payload,
                status
            )
            RETURNING id
        )
        SELECT pg_notify($9, id) FROM inserted
        "#,
        &batch.ids,
        &batch.aggregate_types,
        &batch.aggregate_ids,
        &batch.event_types,
        &batch.event_versions,
        &batch.aggregate_versions as _,
        batch.payloads.as_slice(),
        &batch.statuses,
        REALTIME_OUTBOX_CHANNEL,
    )
    .execute(executor)
    .await?;
    Ok(())
}

fn room_resource_event_from_outbox_event(
    event: &NewRealtimeOutboxEvent,
) -> Result<Option<NewRoomResourceEvent>> {
    let Some(room_id) = event.payload.room_id().copied() else {
        return Ok(None);
    };
    let actor_user_id = event
        .payload
        .user_id()
        .map(super::super::models::id::UserId::as_i64);
    let Some((resource_type, resource_id, details)) = resource_summary_details(event)? else {
        return Ok(None);
    };

    Ok(Some(NewRoomResourceEvent {
        event_id: event.id.clone(),
        scope_type: RoomResourceEventScope::Room,
        room_id: Some(room_id.as_i64()),
        user_id: None,
        aggregate_type: event.aggregate_type.clone(),
        aggregate_id: event.aggregate_id.clone(),
        resource_type,
        resource_id,
        event_type: event.event_type.clone(),
        event_version: event.event_version,
        aggregate_version: event.aggregate_version,
        actor_user_id,
        payload: Some(RoomResourceEventPayload::Realtime {
            event: event.payload.clone(),
        }),
        summary: RoomResourceEventSummary {
            event_type: event.event_type.clone(),
            room_id: Some(room_id.as_i64()),
            actor_user_id,
            resource_type,
            details,
        },
        occurred_at: *event.payload.timestamp(),
    }))
}

type ResourceSummary = (RoomResourceKind, String, RoomResourceEventSummaryDetails);

fn resource_summary_details(event: &NewRealtimeOutboxEvent) -> Result<Option<ResourceSummary>> {
    let summary = match &event.payload {
        RealtimeEvent::PlaybackStateChanged {
            user_id,
            username,
            state,
            ..
        } => (
            RoomResourceKind::PlaybackState,
            state.room_id.to_string(),
            RoomResourceEventSummaryDetails::PlaybackState {
                user_id: Some(user_id.as_i64()),
                username: Some(username.clone()),
                playback_version: Some(state.version),
                is_playing: state.is_playing,
                position: state.position,
                speed: state.speed,
                media_id: state.playing_media_id.map(|id| id.as_i64()),
                playlist_id: state.playing_playlist_id.map(|id| id.as_i64()),
                target_hash: state.target_hash()?,
            },
        ),
        RealtimeEvent::RoomSettingsChanged {
            room_id,
            user_id,
            username,
            version,
            ..
        } => (
            RoomResourceKind::RoomSettings,
            room_id.to_string(),
            RoomResourceEventSummaryDetails::RoomSettings {
                user_id: Some(user_id.as_i64()),
                username: Some(username.clone()),
                settings_version: Some(*version),
            },
        ),
        RealtimeEvent::MediaAdded {
            user_id,
            username,
            media_id,
            media_title,
            ..
        }
        | RealtimeEvent::MediaUpdated {
            user_id,
            username,
            media_id,
            media_title,
            ..
        } => (
            RoomResourceKind::Media,
            media_id.to_string(),
            RoomResourceEventSummaryDetails::Media {
                user_id: Some(user_id.as_i64()),
                username: Some(username.clone()),
                media_id: media_id.as_i64(),
                media_title: Some(media_title.clone()),
            },
        ),
        RealtimeEvent::MediaRemoved {
            user_id,
            username,
            media_id,
            ..
        } => (
            RoomResourceKind::Media,
            media_id.to_string(),
            RoomResourceEventSummaryDetails::Media {
                user_id: Some(user_id.as_i64()),
                username: Some(username.clone()),
                media_id: media_id.as_i64(),
                media_title: None,
            },
        ),
        RealtimeEvent::MediaRemovedBatch {
            room_id,
            user_id,
            username,
            media_ids,
            ..
        }
        | RealtimeEvent::PlaylistReordered {
            room_id,
            user_id,
            username,
            media_ids,
            ..
        } => (
            RoomResourceKind::PlaylistItems,
            room_id.to_string(),
            RoomResourceEventSummaryDetails::PlaylistItems {
                user_id: Some(user_id.as_i64()),
                username: Some(username.clone()),
                media_ids: media_ids
                    .iter()
                    .map(super::super::models::id::MediaId::as_i64)
                    .collect(),
            },
        ),
        RealtimeEvent::PlaylistCreated {
            user_id,
            username,
            playlist,
            ..
        }
        | RealtimeEvent::PlaylistUpdated {
            user_id,
            username,
            playlist,
            ..
        } => (
            RoomResourceKind::Playlist,
            playlist.id.to_string(),
            RoomResourceEventSummaryDetails::Playlist {
                user_id: Some(user_id.as_i64()),
                username: Some(username.clone()),
                playlist_id: playlist.id.as_i64(),
                playlist_name: Some(playlist.name.clone()),
                parent_id: playlist.parent_id.map(|id| id.as_i64()),
            },
        ),
        RealtimeEvent::PlaylistDeleted {
            user_id,
            username,
            playlist_id,
            ..
        } => (
            RoomResourceKind::Playlist,
            playlist_id.to_string(),
            RoomResourceEventSummaryDetails::Playlist {
                user_id: Some(user_id.as_i64()),
                username: Some(username.clone()),
                playlist_id: playlist_id.as_i64(),
                playlist_name: None,
                parent_id: None,
            },
        ),
        RealtimeEvent::UserJoined {
            user_id,
            username,
            role,
            permissions,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
            joined_at,
            ..
        } => (
            RoomResourceKind::RoomMemberEvents,
            user_id.to_string(),
            RoomResourceEventSummaryDetails::RoomMember {
                member: RoomMemberResourceSummary::User {
                    user_id: user_id.as_i64(),
                    username: Some(username.clone()),
                    role: Some(i64::from(i32::from(*role))),
                    permissions: Some(permission_bits(*permissions)),
                    added_permissions: Some(permission_bits(*added_permissions)),
                    removed_permissions: Some(permission_bits(*removed_permissions)),
                    admin_added_permissions: Some(permission_bits(*admin_added_permissions)),
                    admin_removed_permissions: Some(permission_bits(*admin_removed_permissions)),
                    joined_at: Some(*joined_at),
                },
            },
        ),
        RealtimeEvent::GuestJoined {
            guest_id,
            username,
            role,
            permissions,
            joined_at,
            ..
        } => (
            RoomResourceKind::RoomMemberEvents,
            guest_id.clone(),
            RoomResourceEventSummaryDetails::RoomMember {
                member: RoomMemberResourceSummary::Guest {
                    guest_id: guest_id.clone(),
                    username: Some(username.clone()),
                    role: Some(i64::from(i32::from(*role))),
                    permissions: Some(permission_bits(*permissions)),
                    joined_at: Some(*joined_at),
                },
            },
        ),
        RealtimeEvent::UserLeft {
            user_id, username, ..
        } => (
            RoomResourceKind::RoomMemberEvents,
            user_id.to_string(),
            RoomResourceEventSummaryDetails::RoomMember {
                member: RoomMemberResourceSummary::UserLeft {
                    user_id: user_id.as_i64(),
                    username: Some(username.clone()),
                },
            },
        ),
        RealtimeEvent::GuestLeft {
            guest_id, username, ..
        } => (
            RoomResourceKind::RoomMemberEvents,
            guest_id.clone(),
            RoomResourceEventSummaryDetails::RoomMember {
                member: RoomMemberResourceSummary::GuestLeft {
                    guest_id: guest_id.clone(),
                    username: Some(username.clone()),
                },
            },
        ),
        RealtimeEvent::PermissionChanged {
            target_user_id,
            target_username,
            changed_by,
            changed_by_username,
            role_changed,
            role,
            new_permissions,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
            ..
        } => (
            RoomResourceKind::RoomMemberEvents,
            target_user_id.to_string(),
            RoomResourceEventSummaryDetails::RoomMember {
                member: RoomMemberResourceSummary::PermissionChanged {
                    target_user_id: target_user_id.as_i64(),
                    target_username: Some(target_username.clone()),
                    changed_by: Some(changed_by.as_i64()),
                    changed_by_username: Some(changed_by_username.clone()),
                    role_changed: *role_changed,
                    role: Some(i64::from(i32::from(*role))),
                    new_permissions: Some(permission_bits(*new_permissions)),
                    added_permissions: Some(permission_bits(*added_permissions)),
                    removed_permissions: Some(permission_bits(*removed_permissions)),
                    admin_added_permissions: Some(permission_bits(*admin_added_permissions)),
                    admin_removed_permissions: Some(permission_bits(*admin_removed_permissions)),
                },
            },
        ),
        RealtimeEvent::KickPublisher {
            media_id, reason, ..
        } => (
            RoomResourceKind::PlaybackStream,
            media_id.to_string(),
            RoomResourceEventSummaryDetails::PlaybackStream {
                media_id: media_id.as_i64(),
                reason: Some(reason.clone()),
            },
        ),
        RealtimeEvent::KickUserFromRoom {
            user_id, reason, ..
        } => (
            RoomResourceKind::RoomMemberEvents,
            user_id.to_string(),
            RoomResourceEventSummaryDetails::RoomMember {
                member: RoomMemberResourceSummary::Kicked {
                    user_id: user_id.as_i64(),
                    reason: Some(reason.clone()),
                },
            },
        ),
        RealtimeEvent::RoomCreated {
            room_id,
            room_name,
            creator_id,
            ..
        } => (
            RoomResourceKind::Room,
            room_id.to_string(),
            RoomResourceEventSummaryDetails::Room {
                room_name: Some(room_name.clone()),
                creator_id: Some(creator_id.as_i64()),
                deleted_by: None,
                banned_by: None,
                owner_id: None,
                triggered_by: None,
            },
        ),
        RealtimeEvent::RoomDeleted {
            room_id,
            deleted_by,
            ..
        } => (
            RoomResourceKind::Room,
            room_id.to_string(),
            RoomResourceEventSummaryDetails::Room {
                room_name: None,
                creator_id: None,
                deleted_by: Some(deleted_by.as_i64()),
                banned_by: None,
                owner_id: None,
                triggered_by: None,
            },
        ),
        RealtimeEvent::RoomBanned {
            room_id, banned_by, ..
        } => (
            RoomResourceKind::Room,
            room_id.to_string(),
            RoomResourceEventSummaryDetails::Room {
                room_name: None,
                creator_id: None,
                deleted_by: None,
                banned_by: Some(banned_by.as_i64()),
                owner_id: None,
                triggered_by: None,
            },
        ),
        RealtimeEvent::RoomOwnerInactive {
            room_id,
            owner_id,
            triggered_by,
            ..
        } => (
            RoomResourceKind::Room,
            room_id.to_string(),
            RoomResourceEventSummaryDetails::Room {
                room_name: None,
                creator_id: None,
                deleted_by: None,
                banned_by: None,
                owner_id: Some(owner_id.as_i64()),
                triggered_by: Some(triggered_by.as_i64()),
            },
        ),
        _ => return Ok(None),
    };

    Ok(Some(summary))
}

fn permission_bits(permissions: RoomPermissionSet) -> i64 {
    i64::try_from(permissions.bits()).unwrap_or(i64::MAX)
}

fn retry_delay_seconds(attempts: i32) -> i64 {
    let capped = attempts.clamp(1, 8);
    i64::from(2_i32.pow(capped.cast_unsigned())).min(300)
}
