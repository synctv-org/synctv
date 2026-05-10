use chrono::{Datelike, NaiveDate};
use sqlx::PgPool;

use crate::{InternalExt, Result};

pub(crate) fn quote_ident(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

pub(crate) fn start_of_month(date: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(date.year(), date.month(), 1).expect("valid first day of month")
}

pub(crate) fn add_months(date: NaiveDate, months: i32) -> NaiveDate {
    let month_index = date.year() * 12 + date.month0() as i32 + months;
    let year = month_index.div_euclid(12);
    let month = month_index.rem_euclid(12) as u32 + 1;
    NaiveDate::from_ymd_opt(year, month, 1).expect("valid first day of target month")
}

pub(crate) async fn current_database_date(pool: &PgPool) -> Result<NaiveDate> {
    sqlx::query_scalar::<_, NaiveDate>("SELECT CURRENT_DATE")
        .fetch_one(pool)
        .await
        .internal_with_err("Failed to read database current date")
}

pub(crate) async fn table_exists(pool: &PgPool, table_name: &str) -> Result<bool> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
            SELECT 1
            FROM pg_tables
            WHERE schemaname = 'public'
              AND tablename = $1
        )",
    )
    .bind(table_name)
    .fetch_one(pool)
    .await
    .internal_with_err("Failed to check partition existence")
}

pub(crate) fn size_mb(size_bytes: i64) -> f64 {
    round_2(size_bytes as f64 / 1024.0 / 1024.0)
}

pub(crate) fn size_gb(size_bytes: i64) -> f64 {
    round_2(size_bytes as f64 / 1024.0 / 1024.0 / 1024.0)
}

fn round_2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
