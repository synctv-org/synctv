use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{postgres::PgRow, PgPool, Row};

use crate::{Error, Result};

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
        sqlx::query!(
            r"
            WITH inserted AS (
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
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
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
            REALTIME_OUTBOX_CHANNEL
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
        let rows = sqlx::query(
            r"
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
                o.payload,
                o.status,
                o.attempts,
                o.next_retry_at,
                o.locked_by,
                o.locked_at,
                o.created_at,
                o.dispatched_at,
                o.last_error
            ",
        )
        .bind(limit)
        .bind(RealtimeOutboxStatus::Pending.as_i16())
        .bind(RealtimeOutboxStatus::Processing.as_i16())
        .bind(worker_id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| realtime_outbox_event_from_row(&row))
            .collect()
    }

    pub async fn mark_sent(&self, id: &str) -> Result<()> {
        sqlx::query!(
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

        sqlx::query!(
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

fn retry_delay_seconds(attempts: i32) -> i64 {
    let capped = attempts.clamp(1, 8);
    i64::from(2_i32.pow(capped.cast_unsigned())).min(300)
}

fn realtime_outbox_event_from_row(row: &PgRow) -> Result<RealtimeOutboxEvent> {
    let status_code: i16 = row.try_get("status")?;
    Ok(RealtimeOutboxEvent {
        id: row.try_get("id")?,
        aggregate_type: row.try_get("aggregate_type")?,
        aggregate_id: row.try_get("aggregate_id")?,
        event_type: row.try_get("event_type")?,
        event_version: row.try_get("event_version")?,
        aggregate_version: row.try_get("aggregate_version")?,
        payload: row.try_get("payload")?,
        status: RealtimeOutboxStatus::try_from(status_code).map_err(Error::Internal)?,
        attempts: row.try_get("attempts")?,
        next_retry_at: row.try_get("next_retry_at")?,
        locked_by: row.try_get("locked_by")?,
        locked_at: row.try_get("locked_at")?,
        created_at: row.try_get("created_at")?,
        dispatched_at: row.try_get("dispatched_at")?,
        last_error: row.try_get("last_error")?,
    })
}
