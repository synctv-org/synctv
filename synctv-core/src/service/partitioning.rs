use chrono::{Datelike, NaiveDate};
use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::LeaderCheck;
use crate::{Error, InternalExt, Result};

/// Retry interval, in seconds, while a partition manager waits for cluster
/// leadership before performing its initial maintenance run.
pub(crate) const INITIAL_LEADER_RETRY_INTERVAL_SECS: u64 = 5;

/// Whether per-replica startup initialization performs retention cleanup.
///
/// Startup initialization runs on every node so partitions exist before any
/// node inserts data. Retention cleanup stays leader-gated in the background
/// task to avoid duplicate startup DDL, so this is always `false`.
pub(crate) const STARTUP_RUNS_RETENTION_CLEANUP: bool = false;

#[derive(sqlx::FromRow)]
pub(crate) struct PartitionNameRow {
    pub(crate) tablename: String,
}

#[derive(sqlx::FromRow)]
pub(crate) struct PartitionSizeRow {
    pub(crate) size_bytes: i64,
}

pub(crate) fn len_to_i32(len: usize, field: &'static str) -> Result<i32> {
    i32::try_from(len).map_err(|_| Error::Internal(format!("{field} exceeds i32::MAX")))
}

pub(crate) fn len_to_i64(len: usize, field: &'static str) -> Result<i64> {
    i64::try_from(len).map_err(|_| Error::Internal(format!("{field} exceeds i64::MAX")))
}

pub(crate) fn len_to_u64(len: usize, field: &'static str) -> Result<u64> {
    u64::try_from(len).map_err(|_| Error::Internal(format!("{field} exceeds u64::MAX")))
}

pub(crate) fn u32_to_i32(value: u32, field: &'static str) -> Result<i32> {
    i32::try_from(value).map_err(|_| Error::Internal(format!("{field} exceeds i32::MAX")))
}

pub(crate) fn retention_seconds_to_i64(value: u64, field: &'static str) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::Internal(format!("{field} exceeds i64::MAX")))
}

/// Block until this node becomes the cluster leader, returning `true` once
/// leadership is established or `false` if `cancel` fires first.
///
/// Shared by the partition managers so their initial maintenance run begins as
/// soon as leadership is acquired rather than after a full check interval.
pub(crate) async fn wait_for_initial_leader(
    leader_check: Arc<dyn LeaderCheck>,
    cancel: CancellationToken,
    task_name: &'static str,
) -> bool {
    let mut logged_wait = false;

    loop {
        if leader_check.is_leader() {
            return true;
        }

        if !logged_wait {
            info!("Delaying initial {task_name} run until cluster leadership is established");
            logged_wait = true;
        }

        tokio::select! {
            () = cancel.cancelled() => return false,
            () = tokio::time::sleep(std::time::Duration::from_secs(INITIAL_LEADER_RETRY_INTERVAL_SECS)) => {}
        }
    }
}

pub(crate) fn quote_ident(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

pub(crate) fn start_of_month(date: NaiveDate) -> Result<NaiveDate> {
    NaiveDate::from_ymd_opt(date.year(), date.month(), 1)
        .ok_or_else(|| Error::Internal(format!("invalid first day for month containing {date}")))
}

pub(crate) fn add_months(date: NaiveDate, months: i32) -> Result<NaiveDate> {
    let month0 = i32::try_from(date.month0())
        .map_err(|_| Error::Internal(format!("invalid zero-based month for {date}")))?;
    let month_index = date
        .year()
        .checked_mul(12)
        .and_then(|year_months| year_months.checked_add(month0))
        .and_then(|base| base.checked_add(months))
        .ok_or_else(|| {
            Error::Internal(format!(
                "month arithmetic overflow for {date} plus {months} months"
            ))
        })?;
    let year = month_index.div_euclid(12);
    let month = month_index.rem_euclid(12) as u32 + 1;
    NaiveDate::from_ymd_opt(year, month, 1).ok_or_else(|| {
        Error::Internal(format!(
            "invalid target month for {date} plus {months} months"
        ))
    })
}

pub(crate) async fn current_database_date(pool: &PgPool) -> Result<NaiveDate> {
    sqlx::query_scalar!(r#"SELECT CURRENT_DATE as "current_date!""#)
        .fetch_one(pool)
        .await
        .internal_with_err("Failed to read database current date")
}

pub(crate) async fn table_exists(pool: &PgPool, table_name: &str) -> Result<bool> {
    let exists = sqlx::query_scalar!(
        "SELECT EXISTS (
            SELECT 1
            FROM pg_tables
            WHERE schemaname = 'public'
              AND tablename = $1
        )",
        table_name,
    )
    .fetch_one(pool)
    .await
    .internal_with_err("Failed to check partition existence")?;
    exists.ok_or_else(|| {
        Error::Internal("partition existence query returned no scalar value".to_string())
    })
}

pub(crate) fn size_centi_mib(size_bytes: i64) -> i64 {
    centi_units(size_bytes, 1024 * 1024)
}

pub(crate) fn size_centi_gib(size_bytes: i64) -> i64 {
    centi_units(size_bytes, 1024 * 1024 * 1024)
}

fn centi_units(size_bytes: i64, unit_bytes: i64) -> i64 {
    if unit_bytes <= 0 {
        return 0;
    }
    let scaled = i128::from(size_bytes).saturating_mul(100);
    let unit = i128::from(unit_bytes);
    let rounded = if scaled >= 0 {
        (scaled + unit / 2) / unit
    } else {
        (scaled - unit / 2) / unit
    };
    i64::try_from(rounded).unwrap_or_else(|_| {
        if rounded.is_negative() {
            i64::MIN
        } else {
            i64::MAX
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use chrono::NaiveDate;
    use tokio_util::sync::CancellationToken;

    use super::super::LeaderCheck;
    use super::{
        add_months, start_of_month, wait_for_initial_leader, INITIAL_LEADER_RETRY_INTERVAL_SECS,
        STARTUP_RUNS_RETENTION_CLEANUP,
    };

    const _: () = assert!(
        !STARTUP_RUNS_RETENTION_CLEANUP,
        "per-replica startup initialization must avoid retention cleanup DDL"
    );

    fn some<T>(value: Option<T>, context: &str) -> T {
        match value {
            Some(value) => value,
            None => std::panic::panic_any(context.to_string()),
        }
    }

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_initial_leader_completes_before_full_check_interval() {
        struct ToggleLeader(AtomicBool);
        impl LeaderCheck for ToggleLeader {
            fn is_leader(&self) -> bool {
                self.0.load(Ordering::SeqCst)
            }
        }

        let leader = Arc::new(ToggleLeader(AtomicBool::new(false)));
        let cancel = CancellationToken::new();
        let wait_task = tokio::spawn(wait_for_initial_leader(
            leader.clone(),
            cancel,
            "partition management",
        ));

        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_secs(
            INITIAL_LEADER_RETRY_INTERVAL_SECS - 1,
        ))
        .await;
        assert!(
            !wait_task.is_finished(),
            "initial maintenance should still be waiting for leadership"
        );

        leader.0.store(true, Ordering::SeqCst);
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        assert!(
            ok(wait_task.await, "wait task should complete"),
            "leader election should trigger the initial maintenance wait to finish"
        );
    }

    #[test]
    fn month_helpers_return_errors_for_overflow() {
        let max_date = some(
            NaiveDate::from_ymd_opt(262_142, 12, 1),
            "valid max chrono month",
        );
        assert!(add_months(max_date, 1).is_err());
    }

    #[test]
    fn month_helpers_normalize_valid_dates() {
        let date = some(NaiveDate::from_ymd_opt(2026, 6, 5), "valid date");
        let month = ok(start_of_month(date), "valid month");
        assert_eq!(
            month,
            some(NaiveDate::from_ymd_opt(2026, 6, 1), "valid month start")
        );
        assert_eq!(
            ok(add_months(month, 8), "valid month addition"),
            some(NaiveDate::from_ymd_opt(2027, 2, 1), "valid target month")
        );
    }
}
