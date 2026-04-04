use std::sync::{OnceLock, RwLock};

use chrono::{DateTime, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeError {
    InvalidTimeZone(String),
    InvalidDateTime(String),
    AmbiguousLocalDateTime { input: String, timezone: String },
    NonexistentLocalDateTime { input: String, timezone: String },
}

impl std::fmt::Display for TimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTimeZone(value) => write!(f, "invalid timezone '{value}'"),
            Self::InvalidDateTime(value) => write!(f, "invalid datetime '{value}'"),
            Self::AmbiguousLocalDateTime { input, timezone } => write!(
                f,
                "datetime '{input}' is ambiguous in timezone '{timezone}'"
            ),
            Self::NonexistentLocalDateTime { input, timezone } => write!(
                f,
                "datetime '{input}' does not exist in timezone '{timezone}'"
            ),
        }
    }
}

impl std::error::Error for TimeError {}

fn timezone_state() -> &'static RwLock<Tz> {
    static DEFAULT_TIMEZONE: OnceLock<RwLock<Tz>> = OnceLock::new();
    DEFAULT_TIMEZONE.get_or_init(|| RwLock::new(Tz::UTC))
}

pub fn parse_timezone_name(value: &str) -> Result<Tz, TimeError> {
    value
        .trim()
        .parse::<Tz>()
        .map_err(|_| TimeError::InvalidTimeZone(value.trim().to_string()))
}

pub fn resolve_timezone_name_with(
    configured: Option<&str>,
    get_env: &impl Fn(&str) -> Option<String>,
) -> Result<String, TimeError> {
    if let Some(value) = configured.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(parse_timezone_name(value)?.to_string());
    }

    if let Some(value) = get_env("TZ").map(|value| value.trim().to_string()) {
        if !value.is_empty() {
            if let Ok(tz) = parse_timezone_name(&value) {
                return Ok(tz.to_string());
            }
        }
    }

    if let Ok(value) = iana_time_zone::get_timezone() {
        if let Ok(tz) = parse_timezone_name(&value) {
            return Ok(tz.to_string());
        }
    }

    Ok(Tz::UTC.to_string())
}

pub fn set_default_timezone_name(value: &str) -> Result<(), TimeError> {
    let timezone = parse_timezone_name(value)?;
    let mut guard = timezone_state()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = timezone;
    Ok(())
}

pub fn default_timezone() -> Tz {
    *timezone_state()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn default_timezone_name() -> String {
    default_timezone().to_string()
}

pub fn format_datetime_rfc3339(value: DateTime<Utc>) -> String {
    value.with_timezone(&default_timezone()).to_rfc3339()
}

pub fn format_datetime_display(value: DateTime<Utc>) -> String {
    let timezone = default_timezone();
    let localized = value.with_timezone(&timezone);
    format!(
        "{} {} ({})",
        localized.format("%Y-%m-%d %H:%M:%S"),
        localized.format("%:z"),
        timezone
    )
}

pub fn format_timestamp_secs_display(seconds: i64) -> Option<String> {
    let timestamp = DateTime::<Utc>::from_timestamp(seconds, 0)?;
    Some(format!(
        "{} ({seconds})",
        format_datetime_display(timestamp)
    ))
}

pub fn parse_datetime_to_utc(value: &str) -> Result<DateTime<Utc>, TimeError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(TimeError::InvalidDateTime(String::new()));
    }

    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Ok(parsed.with_timezone(&Utc));
    }

    for format in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(parsed) = NaiveDateTime::parse_from_str(value, format) {
            return localize_naive_datetime(parsed, value);
        }
    }

    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        let parsed = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| TimeError::InvalidDateTime(value.to_string()))?;
        return localize_naive_datetime(parsed, value);
    }

    Err(TimeError::InvalidDateTime(value.to_string()))
}

fn localize_naive_datetime(
    value: NaiveDateTime,
    original: &str,
) -> Result<DateTime<Utc>, TimeError> {
    let timezone = default_timezone();
    match timezone.from_local_datetime(&value) {
        LocalResult::Single(parsed) => Ok(parsed.with_timezone(&Utc)),
        LocalResult::Ambiguous(_, _) => Err(TimeError::AmbiguousLocalDateTime {
            input: original.to_string(),
            timezone: timezone.to_string(),
        }),
        LocalResult::None => Err(TimeError::NonexistentLocalDateTime {
            input: original.to_string(),
            timezone: timezone.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use chrono::TimeZone;

    use super::{
        default_timezone_name, format_datetime_display, format_datetime_rfc3339,
        parse_datetime_to_utc, resolve_timezone_name_with, set_default_timezone_name,
    };

    fn acquire_time_test_lock() -> MutexGuard<'static, ()> {
        static TIME_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        TIME_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    struct TimeZoneGuard {
        previous: String,
    }

    impl TimeZoneGuard {
        fn set(name: &str) -> Self {
            let previous = default_timezone_name();
            set_default_timezone_name(name).expect("timezone should be valid");
            Self { previous }
        }
    }

    impl Drop for TimeZoneGuard {
        fn drop(&mut self) {
            let _ = set_default_timezone_name(&self.previous);
        }
    }

    #[test]
    fn resolve_timezone_prefers_explicit_config() {
        let _lock = acquire_time_test_lock();
        let resolved = resolve_timezone_name_with(Some("Asia/Shanghai"), &|_| None)
            .expect("configured timezone should resolve");
        assert_eq!(resolved, "Asia/Shanghai");
    }

    #[test]
    fn resolve_timezone_falls_back_to_tz_env() {
        let _lock = acquire_time_test_lock();
        let resolved = resolve_timezone_name_with(None, &|name| {
            (name == "TZ").then(|| "America/New_York".to_string())
        })
        .expect("TZ env should resolve");
        assert_eq!(resolved, "America/New_York");
    }

    #[test]
    fn format_and_parse_use_default_timezone() {
        let _lock = acquire_time_test_lock();
        let _guard = TimeZoneGuard::set("Asia/Shanghai");
        let timestamp = chrono::Utc
            .with_ymd_and_hms(2026, 4, 4, 8, 24, 31)
            .single()
            .expect("timestamp should be valid");

        assert_eq!(
            format_datetime_display(timestamp),
            "2026-04-04 16:24:31 +08:00 (Asia/Shanghai)"
        );
        assert_eq!(
            format_datetime_rfc3339(timestamp),
            "2026-04-04T16:24:31+08:00"
        );
        assert_eq!(
            parse_datetime_to_utc("2026-04-04 16:24:31").expect("local datetime should parse"),
            timestamp
        );
    }
}
