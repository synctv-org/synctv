use tracing::Level;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter,
};

const SQLX_POSTGRES_NOTICE_TARGET: &str = "sqlx::postgres::notice";

#[derive(Debug, Clone)]
pub struct LoggingOptions {
    pub level: String,
    pub format: String,
    pub filter: Option<String>,
    pub backtrace: bool,
    pub file_path: Option<String>,
}

impl Default for LoggingOptions {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: "pretty".to_string(),
            filter: None,
            backtrace: false,
            file_path: None,
        }
    }
}

/// Initialize structured logging based on configuration
///
/// Supports both JSON (production) and pretty (development) formats
/// with configurable log levels and optional file output with rotation.
///
/// Log rotation configuration:
/// - Rotation: Daily (at midnight local time)
/// - Filename format: synctv-YYYY-MM-DD.log
/// - No automatic file count limit (use external logrotate for cleanup)
///
/// Returns an optional `WorkerGuard` when file logging is enabled.
/// The caller **must** hold this guard alive (e.g. in `main()`) so that
/// buffered log entries are flushed on shutdown.
pub fn init_logging(
    config: &LoggingOptions,
) -> anyhow::Result<Option<tracing_appender::non_blocking::WorkerGuard>> {
    let env_filter = build_env_filter(config)?;

    let registry = tracing_subscriber::registry().with(env_filter);

    if config.format.as_str() == "json" {
        // JSON format for production (structured logging)
        let json_layer = fmt::layer()
            .json()
            .with_span_events(FmtSpan::CLOSE)
            .with_current_span(true)
            .with_span_list(true)
            .with_target(true)
            .with_line_number(true)
            .with_file(true);

        if let Some(file_path) = &config.file_path {
            // Extract directory and create rolling file appender
            let log_dir = std::path::Path::new(file_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));

            let file_appender = RollingFileAppender::builder()
                .rotation(Rotation::DAILY)
                .filename_prefix("synctv")
                .filename_suffix("log")
                .build(log_dir)?;

            let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
            let file_layer = json_layer.with_writer(non_blocking);

            registry.with(file_layer).init();

            return Ok(Some(guard));
        }
        registry.with(json_layer).init();
    } else {
        // Compact human-readable format for development. `pretty()` inserts
        // extra blank lines between events, which is too noisy for daemon logs.
        let pretty_layer = fmt::layer()
            .compact()
            .with_span_events(FmtSpan::CLOSE)
            .with_target(true)
            .with_line_number(true)
            .with_file(false);

        if let Some(file_path) = &config.file_path {
            // Extract directory and create rolling file appender
            let log_dir = std::path::Path::new(file_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));

            let file_appender = RollingFileAppender::builder()
                .rotation(Rotation::DAILY)
                .filename_prefix("synctv")
                .filename_suffix("log")
                .build(log_dir)?;

            let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
            let file_layer = pretty_layer.with_writer(non_blocking);

            registry.with(file_layer).init();

            return Ok(Some(guard));
        }
        registry.with(pretty_layer).init();
    }

    Ok(None)
}

fn build_env_filter(config: &LoggingOptions) -> anyhow::Result<EnvFilter> {
    let filter_spec = build_env_filter_spec(config)?;
    EnvFilter::try_new(filter_spec)
        .map_err(|e| anyhow::anyhow!("Invalid log filter specification: {e}"))
}

fn build_env_filter_spec(config: &LoggingOptions) -> anyhow::Result<String> {
    let mut filter_spec = if let Some(filter) = config.filter.as_deref().map(str::trim) {
        if filter.is_empty() {
            parse_log_level(&config.level)?.to_string()
        } else {
            filter.to_string()
        }
    } else {
        parse_log_level(&config.level)?.to_string()
    };

    if !filter_contains_target_directive(&filter_spec, SQLX_POSTGRES_NOTICE_TARGET) {
        let notice_level = if matches!(effective_log_level(config)?, Level::TRACE | Level::DEBUG) {
            "info"
        } else {
            "warn"
        };
        filter_spec.push(',');
        filter_spec.push_str(SQLX_POSTGRES_NOTICE_TARGET);
        filter_spec.push('=');
        filter_spec.push_str(notice_level);
    }

    Ok(filter_spec)
}

fn filter_contains_target_directive(filter_spec: &str, target: &str) -> bool {
    filter_spec.split(',').map(str::trim).any(|directive| {
        directive
            .strip_prefix(target)
            .is_some_and(|rest| rest.starts_with('='))
    })
}

/// Parse log level string to tracing Level
pub(crate) fn parse_log_level(level: &str) -> anyhow::Result<Level> {
    match level.to_lowercase().as_str() {
        "trace" => Ok(Level::TRACE),
        "debug" => Ok(Level::DEBUG),
        "info" => Ok(Level::INFO),
        "warn" | "warning" => Ok(Level::WARN),
        "error" => Ok(Level::ERROR),
        _ => Err(anyhow::anyhow!("Invalid log level: {level}")),
    }
}

pub fn effective_log_level(config: &LoggingOptions) -> anyhow::Result<Level> {
    if let Some(filter) = config.filter.as_deref().map(str::trim) {
        if !filter.is_empty() {
            for directive in filter.split(',').map(str::trim).filter(|d| !d.is_empty()) {
                if directive.contains('=') {
                    continue;
                }
                return parse_log_level(directive);
            }
        }
    }

    parse_log_level(&config.level)
}

/// Generate a trace ID for request tracing
#[must_use]
pub fn generate_trace_id() -> String {
    use rand::RngExt;
    let mut rng = rand::rng();
    let trace_id: u128 = rng.random();
    format!("{trace_id:032x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    #[test]
    fn test_parse_log_level() {
        assert!(parse_log_level("trace").is_ok());
        assert!(parse_log_level("debug").is_ok());
        assert!(parse_log_level("info").is_ok());
        assert!(parse_log_level("warn").is_ok());
        assert!(parse_log_level("error").is_ok());
        assert!(parse_log_level("invalid").is_err());
    }

    #[test]
    fn test_generate_trace_id() {
        let trace_id1 = generate_trace_id();
        let trace_id2 = generate_trace_id();

        assert_eq!(trace_id1.len(), 32);
        assert_eq!(trace_id2.len(), 32);
        assert_ne!(trace_id1, trace_id2);
    }

    #[test]
    fn test_build_env_filter_spec_uses_config_level_without_env_override() {
        let config = LoggingOptions {
            level: "debug".to_string(),
            ..LoggingOptions::default()
        };

        let spec = ok(build_env_filter_spec(&config), "filter spec should build");

        assert_eq!(spec.to_lowercase(), "debug,sqlx::postgres::notice=info");
    }

    #[test]
    fn test_build_env_filter_spec_prefers_explicit_logging_filter() {
        let config = LoggingOptions {
            level: "info".to_string(),
            filter: Some("warn,synctv=debug".to_string()),
            ..LoggingOptions::default()
        };

        let spec = ok(build_env_filter_spec(&config), "filter spec should build");

        assert_eq!(spec, "warn,synctv=debug,sqlx::postgres::notice=warn");
    }

    #[test]
    fn test_build_env_filter_spec_preserves_explicit_sqlx_notice_override() {
        let config = LoggingOptions {
            level: "info".to_string(),
            filter: Some("warn,sqlx::postgres::notice=info,synctv=debug".to_string()),
            ..LoggingOptions::default()
        };

        let spec = ok(build_env_filter_spec(&config), "filter spec should build");

        assert_eq!(spec, "warn,sqlx::postgres::notice=info,synctv=debug");
    }

    #[test]
    fn test_filter_contains_target_directive_matches_exact_target_only() {
        assert!(filter_contains_target_directive(
            "warn,sqlx::postgres::notice=warn",
            "sqlx::postgres::notice"
        ));
        assert!(!filter_contains_target_directive(
            "warn,sqlx::postgres::notice_extra=warn",
            "sqlx::postgres::notice"
        ));
    }

    #[test]
    fn test_effective_log_level_uses_config_level_only() {
        let config = LoggingOptions {
            level: "debug".to_string(),
            ..LoggingOptions::default()
        };

        let level = ok(
            effective_log_level(&config),
            "effective log level should resolve",
        );

        assert_eq!(level, Level::DEBUG);
    }

    #[test]
    fn test_effective_log_level_prefers_global_directive_from_logging_filter() {
        let config = LoggingOptions {
            level: "info".to_string(),
            filter: Some("warn,synctv=debug".to_string()),
            ..LoggingOptions::default()
        };

        let level = ok(
            effective_log_level(&config),
            "effective log level should resolve",
        );

        assert_eq!(level, Level::WARN);
    }
}
