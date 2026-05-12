use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Row};

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
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::Sent => "sent",
            Self::Dead => "dead",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "processing" => Ok(Self::Processing),
            "sent" => Ok(Self::Sent),
            "dead" => Ok(Self::Dead),
            other => Err(Error::Internal(format!(
                "Unknown realtime outbox status: {other}"
            ))),
        }
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

impl RealtimeOutboxEvent {
    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self> {
        let status: String = row.try_get("status")?;
        Ok(Self {
            id: row.try_get("id")?,
            aggregate_type: row.try_get("aggregate_type")?,
            aggregate_id: row.try_get("aggregate_id")?,
            event_type: row.try_get("event_type")?,
            event_version: row.try_get("event_version")?,
            aggregate_version: row.try_get("aggregate_version")?,
            payload: row.try_get("payload")?,
            status: RealtimeOutboxStatus::parse(&status)?,
            attempts: row.try_get("attempts")?,
            next_retry_at: row.try_get("next_retry_at")?,
            locked_by: row.try_get("locked_by")?,
            locked_at: row.try_get("locked_at")?,
            created_at: row.try_get("created_at")?,
            dispatched_at: row.try_get("dispatched_at")?,
            last_error: row.try_get("last_error")?,
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
        sqlx::query(
            r"
            INSERT INTO realtime_outbox (
                id,
                aggregate_type,
                aggregate_id,
                event_type,
                event_version,
                aggregate_version,
                payload
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ",
        )
        .bind(&event.id)
        .bind(&event.aggregate_type)
        .bind(&event.aggregate_id)
        .bind(&event.event_type)
        .bind(event.event_version)
        .bind(event.aggregate_version)
        .bind(&event.payload)
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
                WHERE status = 'pending'
                  AND next_retry_at <= NOW()
                ORDER BY created_at, id
                LIMIT $1
                FOR UPDATE SKIP LOCKED
            )
            UPDATE realtime_outbox o
            SET status = 'processing',
                locked_by = $2,
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
        .bind(worker_id)
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(RealtimeOutboxEvent::from_row).collect()
    }

    pub async fn mark_sent(&self, id: &str) -> Result<()> {
        sqlx::query(
            r"
            UPDATE realtime_outbox
            SET status = 'sent',
                dispatched_at = NOW(),
                locked_by = NULL,
                locked_at = NULL,
                last_error = NULL
            WHERE id = $1
            ",
        )
        .bind(id)
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

        sqlx::query(
            r"
            UPDATE realtime_outbox
            SET status = $2,
                attempts = $3,
                next_retry_at = NOW() + ($4::TEXT || ' seconds')::INTERVAL,
                locked_by = NULL,
                locked_at = NULL,
                last_error = $5
            WHERE id = $1
            ",
        )
        .bind(id)
        .bind(status.as_str())
        .bind(next_attempt)
        .bind(delay_seconds)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn requeue_stale_processing(&self, stale_after_seconds: i64) -> Result<u64> {
        let result = sqlx::query(
            r"
            UPDATE realtime_outbox
            SET status = 'pending',
                locked_by = NULL,
                locked_at = NULL,
                next_retry_at = NOW()
            WHERE status = 'processing'
              AND locked_at < NOW() - ($1::TEXT || ' seconds')::INTERVAL
            ",
        )
        .bind(stale_after_seconds)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn notify_dispatchers(&self) -> Result<()> {
        sqlx::query("SELECT pg_notify($1, '')")
            .bind(REALTIME_OUTBOX_CHANNEL)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

fn retry_delay_seconds(attempts: i32) -> i64 {
    let capped = attempts.clamp(1, 8);
    i64::from(2_i32.pow(capped.cast_unsigned())).min(300)
}
