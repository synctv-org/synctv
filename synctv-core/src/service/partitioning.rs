use chrono::{Datelike, NaiveDate};
use sqlx::PgPool;

use crate::{Error, InternalExt, Result};

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
    use chrono::NaiveDate;

    use super::{add_months, start_of_month};

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
