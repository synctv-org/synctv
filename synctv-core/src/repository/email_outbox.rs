use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{Error, Result};

pub const EMAIL_OUTBOX_CHANNEL: &str = "email_outbox_new";
pub const EMAIL_OUTBOX_MAX_ATTEMPTS: i32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailOutboxKind {
    EmailBind,
    PasswordReset,
    EmailLogin,
    EmailRegistration,
}

impl EmailOutboxKind {
    #[must_use]
    pub const fn as_i16(self) -> i16 {
        match self {
            Self::EmailBind => 1,
            Self::PasswordReset => 2,
            Self::EmailLogin => 3,
            Self::EmailRegistration => 4,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EmailBind => "email_bind",
            Self::PasswordReset => "password_reset",
            Self::EmailLogin => "email_login",
            Self::EmailRegistration => "email_registration",
        }
    }
}

impl TryFrom<i16> for EmailOutboxKind {
    type Error = Error;

    fn try_from(value: i16) -> Result<Self> {
        match value {
            1 => Ok(Self::EmailBind),
            2 => Ok(Self::PasswordReset),
            3 => Ok(Self::EmailLogin),
            4 => Ok(Self::EmailRegistration),
            other => Err(Error::Internal(format!(
                "Unknown email outbox kind: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailOutboxStatus {
    Pending,
    Processing,
    Sent,
    Dead,
}

impl EmailOutboxStatus {
    #[must_use]
    pub const fn as_i16(self) -> i16 {
        match self {
            Self::Pending => 1,
            Self::Processing => 2,
            Self::Sent => 3,
            Self::Dead => 4,
        }
    }
}

impl TryFrom<i16> for EmailOutboxStatus {
    type Error = Error;

    fn try_from(value: i16) -> Result<Self> {
        match value {
            1 => Ok(Self::Pending),
            2 => Ok(Self::Processing),
            3 => Ok(Self::Sent),
            4 => Ok(Self::Dead),
            other => Err(Error::Internal(format!(
                "Unknown email outbox status: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewEmailOutboxJob {
    pub id: String,
    pub kind: EmailOutboxKind,
    pub recipient: String,
    pub encrypted_payload: String,
    pub dedupe_key: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct EmailOutboxJob {
    pub id: String,
    pub kind: EmailOutboxKind,
    pub recipient: String,
    pub encrypted_payload: String,
    pub status: EmailOutboxStatus,
    pub attempts: i32,
    pub lock_version: i64,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct EmailOutboxRepository {
    pool: PgPool,
}

impl EmailOutboxRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn insert(&self, job: &NewEmailOutboxJob) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let inserted = self.insert_with_executor(job, &mut tx).await?;
        tx.commit().await?;
        Ok(inserted)
    }

    pub async fn insert_with_executor(
        &self,
        job: &NewEmailOutboxJob,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<bool> {
        let inserted = sqlx::query_scalar::<_, String>(
            r"
            INSERT INTO email_outbox (
                id, kind, recipient, encrypted_payload, dedupe_key,
                status, expires_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (dedupe_key) DO NOTHING
            RETURNING id
            ",
        )
        .bind(&job.id)
        .bind(job.kind.as_i16())
        .bind(&job.recipient)
        .bind(&job.encrypted_payload)
        .bind(&job.dedupe_key)
        .bind(EmailOutboxStatus::Pending.as_i16())
        .bind(job.expires_at)
        .fetch_optional(&mut **tx)
        .await?;

        if let Some(id) = inserted {
            sqlx::query("SELECT pg_notify($1, $2)")
                .bind(EMAIL_OUTBOX_CHANNEL)
                .bind(id)
                .execute(&mut **tx)
                .await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub async fn claim_batch(&self, worker_id: &str, limit: i64) -> Result<Vec<EmailOutboxJob>> {
        let rows = sqlx::query(
            r"
            WITH picked AS (
                SELECT id
                FROM email_outbox
                WHERE status = $2
                  AND next_attempt_at <= NOW()
                  AND expires_at > NOW()
                ORDER BY next_attempt_at, created_at, id
                LIMIT $1
                FOR UPDATE SKIP LOCKED
            )
            UPDATE email_outbox AS job
            SET status = $3,
                locked_by = $4,
                locked_at = NOW(),
                lock_version = lock_version + 1
            FROM picked
            WHERE job.id = picked.id
            RETURNING job.id, job.kind, job.recipient, job.encrypted_payload,
                      job.status, job.attempts, job.lock_version,
                      job.expires_at, job.created_at
            ",
        )
        .bind(limit)
        .bind(EmailOutboxStatus::Pending.as_i16())
        .bind(EmailOutboxStatus::Processing.as_i16())
        .bind(worker_id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(EmailOutboxJob {
                    id: row.try_get("id")?,
                    kind: EmailOutboxKind::try_from(row.try_get::<i16, _>("kind")?)?,
                    recipient: row.try_get("recipient")?,
                    encrypted_payload: row.try_get("encrypted_payload")?,
                    status: EmailOutboxStatus::try_from(row.try_get::<i16, _>("status")?)?,
                    attempts: row.try_get("attempts")?,
                    lock_version: row.try_get("lock_version")?,
                    expires_at: row.try_get("expires_at")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()
    }

    pub async fn mark_sent(&self, id: &str, worker_id: &str, lock_version: i64) -> Result<bool> {
        let result = sqlx::query(
            r"
            UPDATE email_outbox
            SET status = $4,
                sent_at = NOW(),
                locked_by = NULL,
                locked_at = NULL,
                last_error = NULL
            WHERE id = $1
              AND locked_by = $2
              AND lock_version = $3
              AND status = $5
            ",
        )
        .bind(id)
        .bind(worker_id)
        .bind(lock_version)
        .bind(EmailOutboxStatus::Sent.as_i16())
        .bind(EmailOutboxStatus::Processing.as_i16())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn mark_failed(
        &self,
        job: &EmailOutboxJob,
        worker_id: &str,
        error: &str,
        retryable: bool,
    ) -> Result<Option<EmailOutboxStatus>> {
        let next_attempt = job.attempts.saturating_add(1);
        let terminal = !retryable || next_attempt >= EMAIL_OUTBOX_MAX_ATTEMPTS;
        let status = if terminal {
            EmailOutboxStatus::Dead
        } else {
            EmailOutboxStatus::Pending
        };
        let retry_delay = retry_delay_seconds(next_attempt, &job.id);

        let updated = sqlx::query_scalar::<_, i16>(
            r"
            UPDATE email_outbox
            SET status = $4,
                attempts = $5,
                next_attempt_at = CASE
                    WHEN $4 = $8 THEN NOW() + make_interval(secs => $6::DOUBLE PRECISION)
                    ELSE next_attempt_at
                END,
                locked_by = NULL,
                locked_at = NULL,
                last_error = $7
            WHERE id = $1
              AND locked_by = $2
              AND lock_version = $3
              AND status = $9
            RETURNING status
            ",
        )
        .bind(&job.id)
        .bind(worker_id)
        .bind(job.lock_version)
        .bind(status.as_i16())
        .bind(next_attempt)
        .bind(retry_delay)
        .bind(error)
        .bind(EmailOutboxStatus::Pending.as_i16())
        .bind(EmailOutboxStatus::Processing.as_i16())
        .fetch_optional(&self.pool)
        .await?;

        updated.map(EmailOutboxStatus::try_from).transpose()
    }

    pub async fn requeue_stale_processing(&self, stale_after_seconds: i64) -> Result<u64> {
        let result = sqlx::query(
            r"
            UPDATE email_outbox
            SET status = $2,
                locked_by = NULL,
                locked_at = NULL,
                next_attempt_at = NOW()
            WHERE status = $3
              AND locked_at < NOW() - make_interval(secs => $1::DOUBLE PRECISION)
            ",
        )
        .bind(stale_after_seconds)
        .bind(EmailOutboxStatus::Pending.as_i16())
        .bind(EmailOutboxStatus::Processing.as_i16())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn expire_pending(&self) -> Result<u64> {
        let result = sqlx::query(
            r"
            UPDATE email_outbox
            SET status = $1,
                last_error = 'delivery job expired before it could be sent'
            WHERE status = $2
              AND expires_at <= NOW()
            ",
        )
        .bind(EmailOutboxStatus::Dead.as_i16())
        .bind(EmailOutboxStatus::Pending.as_i16())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn load_cleanup_pending(&self, limit: i64) -> Result<Vec<EmailOutboxJob>> {
        let rows = sqlx::query(
            r"
            SELECT id, kind, recipient, encrypted_payload, status, attempts,
                   lock_version, expires_at, created_at
            FROM email_outbox
            WHERE status = $1
              AND cleanup_completed_at IS NULL
            ORDER BY created_at, id
            LIMIT $2
            ",
        )
        .bind(EmailOutboxStatus::Dead.as_i16())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_job).collect()
    }

    pub async fn mark_cleanup_completed(&self, id: &str) -> Result<bool> {
        let result = sqlx::query(
            r"
            UPDATE email_outbox
            SET cleanup_completed_at = NOW()
            WHERE id = $1
              AND status = $2
              AND cleanup_completed_at IS NULL
            ",
        )
        .bind(id)
        .bind(EmailOutboxStatus::Dead.as_i16())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn purge_terminal(&self, retention_seconds: i64) -> Result<u64> {
        let result = sqlx::query(
            r"
            DELETE FROM email_outbox
            WHERE (status = $2 OR (status = $3 AND cleanup_completed_at IS NOT NULL))
              AND COALESCE(sent_at, created_at)
                  < NOW() - make_interval(secs => $1::DOUBLE PRECISION)
            ",
        )
        .bind(retention_seconds)
        .bind(EmailOutboxStatus::Sent.as_i16())
        .bind(EmailOutboxStatus::Dead.as_i16())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn pending_count(&self) -> Result<i64> {
        let count =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM email_outbox WHERE status = $1")
                .bind(EmailOutboxStatus::Pending.as_i16())
                .fetch_one(&self.pool)
                .await?;
        Ok(count)
    }
}

fn row_to_job(row: &sqlx::postgres::PgRow) -> Result<EmailOutboxJob> {
    Ok(EmailOutboxJob {
        id: row.try_get("id")?,
        kind: EmailOutboxKind::try_from(row.try_get::<i16, _>("kind")?)?,
        recipient: row.try_get("recipient")?,
        encrypted_payload: row.try_get("encrypted_payload")?,
        status: EmailOutboxStatus::try_from(row.try_get::<i16, _>("status")?)?,
        attempts: row.try_get("attempts")?,
        lock_version: row.try_get("lock_version")?,
        expires_at: row.try_get("expires_at")?,
        created_at: row.try_get("created_at")?,
    })
}

fn retry_delay_seconds(attempt: i32, id: &str) -> i32 {
    let exponent = u32::try_from(attempt.clamp(1, 8) - 1).unwrap_or_default();
    let base = 5u64.saturating_mul(2u64.saturating_pow(exponent));
    let jitter = id
        .bytes()
        .fold(0u64, |accumulator, byte| accumulator + u64::from(byte))
        % 5;
    i32::try_from(base.saturating_add(jitter).min(900)).unwrap_or(900)
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    fn job(id: &str) -> NewEmailOutboxJob {
        NewEmailOutboxJob {
            id: id.to_string(),
            kind: EmailOutboxKind::EmailLogin,
            recipient: format!("{id}@example.com"),
            encrypted_payload: "enc:test".to_string(),
            dedupe_key: format!("dedupe-{id}"),
            expires_at: Utc::now() + Duration::hours(1),
        }
    }

    #[test]
    fn retry_delay_is_bounded_and_increases() {
        let first = retry_delay_seconds(1, "job-a");
        let fourth = retry_delay_seconds(4, "job-a");
        let final_attempt = retry_delay_seconds(100, "job-a");
        assert!(first >= 5);
        assert!(fourth > first);
        assert!(final_attempt <= 900);
    }

    #[tokio::test]
    #[ignore = "Requires Docker (testcontainers)"]
    async fn concurrent_workers_claim_distinct_jobs() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let repository = EmailOutboxRepository::new(pool);
        repository.insert(&job("claim-a")).await.expect("insert a");
        repository.insert(&job("claim-b")).await.expect("insert b");

        let (first, second) = tokio::join!(
            repository.claim_batch("worker-a", 1),
            repository.claim_batch("worker-b", 1),
        );
        let first = first.expect("worker a claim");
        let second = second.expect("worker b claim");
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_ne!(first[0].id, second[0].id);
    }

    #[tokio::test]
    #[ignore = "Requires Docker (testcontainers)"]
    async fn stale_lease_is_recovered_and_old_worker_is_fenced() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let repository = EmailOutboxRepository::new(pool.clone());
        repository
            .insert(&job("stale-lease"))
            .await
            .expect("insert job");
        let first = repository
            .claim_batch("worker-a", 1)
            .await
            .expect("first claim")
            .remove(0);
        sqlx::query("UPDATE email_outbox SET locked_at = NOW() - INTERVAL '3 minutes'")
            .execute(&pool)
            .await
            .expect("age lease");
        assert_eq!(
            repository
                .requeue_stale_processing(120)
                .await
                .expect("requeue stale"),
            1
        );
        let second = repository
            .claim_batch("worker-b", 1)
            .await
            .expect("second claim")
            .remove(0);
        assert!(second.lock_version > first.lock_version);
        assert!(!repository
            .mark_sent(&first.id, "worker-a", first.lock_version)
            .await
            .expect("old acknowledgement"));
        assert!(repository
            .mark_sent(&second.id, "worker-b", second.lock_version)
            .await
            .expect("current acknowledgement"));
    }

    #[tokio::test]
    #[ignore = "Requires Docker (testcontainers)"]
    async fn rollback_removes_outbox_insert_and_notification_source() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let repository = EmailOutboxRepository::new(pool.clone());
        let mut tx = pool.begin().await.expect("begin transaction");
        assert!(repository
            .insert_with_executor(&job("rolled-back"), &mut tx)
            .await
            .expect("insert job"));
        tx.rollback().await.expect("rollback transaction");
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM email_outbox WHERE id = 'rolled-back'",
        )
        .fetch_one(&pool)
        .await
        .expect("count jobs");
        assert_eq!(count, 0);
    }

    #[tokio::test]
    #[ignore = "Requires Docker (testcontainers)"]
    async fn retry_budget_moves_job_to_recoverable_dead_cleanup() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let repository = EmailOutboxRepository::new(pool.clone());
        repository
            .insert(&job("retry-budget"))
            .await
            .expect("insert job");

        for attempt in 1..=EMAIL_OUTBOX_MAX_ATTEMPTS {
            let claimed = repository
                .claim_batch("retry-worker", 1)
                .await
                .expect("claim retry")
                .remove(0);
            let status = repository
                .mark_failed(&claimed, "retry-worker", "smtp delivery failed", true)
                .await
                .expect("record retry")
                .expect("lease should remain valid");
            if attempt < EMAIL_OUTBOX_MAX_ATTEMPTS {
                assert_eq!(status, EmailOutboxStatus::Pending);
                sqlx::query(
                    "UPDATE email_outbox SET next_attempt_at = NOW() WHERE id = 'retry-budget'",
                )
                .execute(&pool)
                .await
                .expect("make retry due");
            } else {
                assert_eq!(status, EmailOutboxStatus::Dead);
            }
        }

        let cleanup = repository
            .load_cleanup_pending(10)
            .await
            .expect("load cleanup jobs");
        assert_eq!(cleanup.len(), 1);
        assert_eq!(cleanup[0].id, "retry-budget");
    }
}
