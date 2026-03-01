use tracing::Level;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter,
};

use crate::config::LoggingConfig;

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
    config: &LoggingConfig,
) -> anyhow::Result<Option<tracing_appender::non_blocking::WorkerGuard>> {
    let log_level = parse_log_level(&config.level)?;

    // Create env filter from config level
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&config.level))
        .unwrap_or_else(|_| EnvFilter::new(log_level.to_string()));

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
        // Pretty format for development (human-readable)
        let pretty_layer = fmt::layer()
            .pretty()
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

/// Parse log level string to tracing Level
fn parse_log_level(level: &str) -> anyhow::Result<Level> {
    match level.to_lowercase().as_str() {
        "trace" => Ok(Level::TRACE),
        "debug" => Ok(Level::DEBUG),
        "info" => Ok(Level::INFO),
        "warn" | "warning" => Ok(Level::WARN),
        "error" => Ok(Level::ERROR),
        _ => Err(anyhow::anyhow!("Invalid log level: {level}")),
    }
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
}
