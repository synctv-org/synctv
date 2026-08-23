use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use tracing::Level;
use tracing_appender::non_blocking::{ErrorCounter, NonBlocking, NonBlockingBuilder, WorkerGuard};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{
    filter::FilterFn,
    fmt::{self, writer::BoxMakeWriter},
    layer::{Layer, Layered, SubscriberExt},
    registry::Registry,
    util::SubscriberInitExt,
};

const SQLX_POSTGRES_NOTICE_TARGET: &str = "sqlx::postgres::notice";
const LOG_BUFFERED_LINES_LIMIT: usize = 128_000;
static LOGGING_ERROR_COUNTERS: OnceLock<Vec<ComponentErrorCounter>> = OnceLock::new();

#[derive(Debug, Clone)]
struct ComponentErrorCounter {
    component: String,
    counter: ErrorCounter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogColor {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogRotation {
    Daily,
    Hourly,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStyle {
    Diagnostic,
    Access,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogOutput {
    Stdout,
    Stderr,
    File {
        path: PathBuf,
        rotation: LogRotation,
        max_files: usize,
    },
}

#[derive(Debug, Clone)]
pub struct ComponentLoggingOptions {
    pub name: String,
    pub style: LogStyle,
    pub targets: Vec<String>,
    pub level: String,
    pub format: String,
    pub output: LogOutput,
    pub color: LogColor,
}

#[derive(Debug, Clone)]
pub struct LoggingOptions {
    pub global: ComponentLoggingOptions,
    pub components: Vec<ComponentLoggingOptions>,
}

impl Default for LoggingOptions {
    fn default() -> Self {
        Self {
            global: ComponentLoggingOptions {
                name: "global".to_string(),
                style: LogStyle::Diagnostic,
                targets: Vec::new(),
                level: "info".to_string(),
                format: "text".to_string(),
                output: LogOutput::Stdout,
                color: LogColor::Auto,
            },
            components: Vec::new(),
        }
    }
}

/// Keeps every non-blocking logging worker alive until application shutdown.
#[derive(Debug)]
pub struct LoggingGuards {
    workers: Vec<WorkerGuard>,
    error_counters: Vec<ComponentErrorCounter>,
}

impl LoggingGuards {
    #[must_use]
    pub fn len(&self) -> usize {
        self.workers.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.workers.is_empty()
    }

    /// Return cumulative dropped log lines for every configured component.
    #[must_use]
    pub fn dropped_lines(&self) -> Vec<(String, usize)> {
        dropped_lines(&self.error_counters)
    }
}

fn dropped_lines(counters: &[ComponentErrorCounter]) -> Vec<(String, usize)> {
    counters
        .iter()
        .map(|entry| (entry.component.clone(), entry.counter.dropped_lines()))
        .collect()
}

pub(crate) fn dropped_lines_by_component() -> Vec<(String, usize)> {
    LOGGING_ERROR_COUNTERS
        .get()
        .map_or_else(Vec::new, |counters| dropped_lines(counters))
}

/// Build one filtered fmt layer per configured output and install them on a
/// single registry. Routing is exclusive: configured targets are handled by
/// their components, while the global output receives every remaining target.
pub fn init_logging(config: &LoggingOptions) -> anyhow::Result<LoggingGuards> {
    let (subscriber, guards) = build_subscriber(config)?;
    subscriber
        .try_init()
        .map_err(|error| anyhow::anyhow!("failed to install global logging subscriber: {error}"))?;
    let _ = LOGGING_ERROR_COUNTERS.set(guards.error_counters.clone());
    Ok(guards)
}

type LoggingSubscriber = Layered<Vec<Box<dyn Layer<Registry> + Send + Sync>>, Registry>;

fn build_subscriber(config: &LoggingOptions) -> anyhow::Result<(LoggingSubscriber, LoggingGuards)> {
    validate_component_routes(config)?;

    let component_targets: Vec<String> = config
        .components
        .iter()
        .flat_map(|component| component.targets.iter().cloned())
        .collect();

    let mut layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = Vec::new();
    let mut writers = WriterPool::new();

    for (component, is_global) in std::iter::once((&config.global, true))
        .chain(config.components.iter().map(|component| (component, false)))
    {
        let level = parse_log_level(&component.level)?;
        let targets = component.targets.clone();
        let component_targets_for_global = component_targets.clone();
        let filter = FilterFn::new(move |metadata| {
            if *metadata.level() > level {
                return false;
            }
            if is_global && metadata.target() == SQLX_POSTGRES_NOTICE_TARGET {
                let notice_level = if matches!(level, Level::TRACE | Level::DEBUG) {
                    Level::INFO
                } else {
                    Level::WARN
                };
                if *metadata.level() > notice_level {
                    return false;
                }
            }
            target_is_owned_by_component(
                metadata.target(),
                is_global,
                &targets,
                &component_targets_for_global,
            )
        });

        let writer = writers.writer_for(component)?;
        let is_access_log = component.style == LogStyle::Access;
        let layer = if component.format.eq_ignore_ascii_case("json") && is_access_log {
            fmt::layer()
                .json()
                .with_current_span(false)
                .with_span_list(false)
                .with_target(false)
                .with_line_number(false)
                .with_file(false)
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(filter)
                .boxed()
        } else if component.format.eq_ignore_ascii_case("json") {
            fmt::layer()
                .json()
                .with_current_span(true)
                .with_span_list(true)
                .with_target(true)
                .with_line_number(true)
                .with_file(true)
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(filter)
                .boxed()
        } else if component.format.eq_ignore_ascii_case("text") && is_access_log {
            fmt::layer()
                .compact()
                .with_target(false)
                .with_line_number(false)
                .with_file(false)
                .with_ansi(ansi_enabled(component))
                .with_writer(writer)
                .with_filter(filter)
                .boxed()
        } else if component.format.eq_ignore_ascii_case("text") {
            fmt::layer()
                .compact()
                .with_target(true)
                .with_line_number(true)
                .with_file(false)
                .with_ansi(ansi_enabled(component))
                .with_writer(writer)
                .with_filter(filter)
                .boxed()
        } else {
            return Err(anyhow::anyhow!(
                "invalid log format '{}' for component '{}'",
                component.format,
                component.name
            ));
        };
        layers.push(layer);
    }

    Ok((
        tracing_subscriber::registry().with(layers),
        LoggingGuards {
            workers: writers.workers,
            error_counters: writers.error_counters,
        },
    ))
}

fn validate_component_routes(config: &LoggingOptions) -> anyhow::Result<()> {
    if config.global.name.trim().is_empty() {
        return Err(anyhow::anyhow!(
            "global logging component name must be non-empty"
        ));
    }
    if !config.global.targets.is_empty() {
        return Err(anyhow::anyhow!(
            "global logging component must not define targets"
        ));
    }

    let mut names = HashSet::new();
    validate_component_options(&config.global, &mut names)?;
    for component in &config.components {
        if component.targets.is_empty() {
            return Err(anyhow::anyhow!(
                "logging component '{}' requires at least one target",
                component.name
            ));
        }
        validate_component_options(component, &mut names)?;
    }

    for (index, left) in config.components.iter().enumerate() {
        for right in config.components.iter().skip(index + 1) {
            for left_target in &left.targets {
                for right_target in &right.targets {
                    if target_prefixes_overlap(left_target, right_target) {
                        return Err(anyhow::anyhow!(
                            "logging targets '{}' and '{}' overlap across components '{}' and '{}'",
                            left_target,
                            right_target,
                            left.name,
                            right.name
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_component_options(
    component: &ComponentLoggingOptions,
    names: &mut HashSet<String>,
) -> anyhow::Result<()> {
    if component.name.trim().is_empty() || !names.insert(component.name.clone()) {
        return Err(anyhow::anyhow!(
            "logging component names must be non-empty and unique"
        ));
    }
    parse_log_level(&component.level).map_err(|error| {
        anyhow::anyhow!(
            "invalid log level for component '{}': {error}",
            component.name
        )
    })?;
    if !matches!(
        component.format.to_ascii_lowercase().as_str(),
        "text" | "json"
    ) {
        return Err(anyhow::anyhow!(
            "invalid log format '{}' for component '{}'",
            component.format,
            component.name
        ));
    }
    if matches!(&component.output, LogOutput::File { path, .. } if path.as_os_str().is_empty()) {
        return Err(anyhow::anyhow!(
            "file output path for component '{}' must not be empty",
            component.name
        ));
    }
    if matches!(&component.output, LogOutput::File { max_files: 0, .. }) {
        return Err(anyhow::anyhow!(
            "file retention for component '{}' must keep at least one file",
            component.name
        ));
    }
    Ok(())
}

fn target_prefixes_overlap(left: &str, right: &str) -> bool {
    let is_prefix = |prefix: &str, target: &str| {
        target == prefix
            || target
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with("::"))
    };
    is_prefix(left, right) || is_prefix(right, left)
}

fn target_is_owned_by_component(
    target: &str,
    is_global: bool,
    component_targets: &[String],
    all_component_targets: &[String],
) -> bool {
    let matches = |configured: &[String]| {
        configured.iter().any(|prefix| {
            target == prefix
                || target
                    .strip_prefix(prefix)
                    .is_some_and(|suffix| suffix.starts_with("::"))
        })
    };
    if is_global {
        !matches(all_component_targets)
    } else {
        matches(component_targets)
    }
}

struct WriterPool {
    workers: Vec<WorkerGuard>,
    error_counters: Vec<ComponentErrorCounter>,
}

impl WriterPool {
    const fn new() -> Self {
        Self {
            workers: Vec::new(),
            error_counters: Vec::new(),
        }
    }

    fn writer_for(&mut self, component: &ComponentLoggingOptions) -> anyhow::Result<BoxMakeWriter> {
        let thread_name = format!("synctv-log-{}", component.name);
        match &component.output {
            LogOutput::Stdout => {
                Ok(self.component_writer(&component.name, std::io::stdout(), &thread_name))
            }
            LogOutput::Stderr => {
                Ok(self.component_writer(&component.name, std::io::stderr(), &thread_name))
            }
            LogOutput::File {
                path,
                rotation,
                max_files,
            } => {
                let appender = build_file_appender(path, *rotation, *max_files, &component.name)?;
                Ok(self.component_writer(&component.name, appender, &thread_name))
            }
        }
    }

    fn component_writer<T>(
        &mut self,
        component: &str,
        output: T,
        thread_name: &str,
    ) -> BoxMakeWriter
    where
        T: std::io::Write + Send + 'static,
    {
        let (writer, worker) = non_blocking_writer(output, thread_name);
        self.error_counters.push(ComponentErrorCounter {
            component: component.to_string(),
            counter: writer.error_counter(),
        });
        self.workers.push(worker);
        BoxMakeWriter::new(writer)
    }
}

fn non_blocking_writer<T>(writer: T, thread_name: &str) -> (NonBlocking, WorkerGuard)
where
    T: std::io::Write + Send + 'static,
{
    NonBlockingBuilder::default()
        .buffered_lines_limit(LOG_BUFFERED_LINES_LIMIT)
        .lossy(true)
        .thread_name(thread_name)
        .finish(writer)
}

fn build_file_appender(
    file_path: &Path,
    rotation: LogRotation,
    max_files: usize,
    component_name: &str,
) -> anyhow::Result<RollingFileAppender> {
    let log_dir = file_path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(log_dir)?;
    let prefix = file_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map_or_else(|| format!("synctv-{component_name}"), str::to_string);
    let rotation = match rotation {
        LogRotation::Daily => Rotation::DAILY,
        LogRotation::Hourly => Rotation::HOURLY,
        LogRotation::Never => Rotation::NEVER,
    };
    RollingFileAppender::builder()
        .rotation(rotation)
        .filename_prefix(prefix)
        .filename_suffix("log")
        .max_log_files(max_files)
        .build(log_dir)
        .map_err(Into::into)
}

fn ansi_enabled(config: &ComponentLoggingOptions) -> bool {
    if matches!(config.output, LogOutput::File { .. }) {
        return false;
    }
    match config.color {
        LogColor::Always => true,
        LogColor::Never => false,
        LogColor::Auto => match config.output {
            LogOutput::Stderr => std::io::IsTerminal::is_terminal(&std::io::stderr()),
            LogOutput::Stdout => std::io::IsTerminal::is_terminal(&std::io::stdout()),
            LogOutput::File { .. } => false,
        },
    }
}

pub(crate) fn parse_log_level(level: &str) -> anyhow::Result<Level> {
    match level.to_ascii_lowercase().as_str() {
        "trace" => Ok(Level::TRACE),
        "debug" => Ok(Level::DEBUG),
        "info" => Ok(Level::INFO),
        "warn" | "warning" => Ok(Level::WARN),
        "error" => Ok(Level::ERROR),
        _ => Err(anyhow::anyhow!("invalid log level: {level}")),
    }
}

pub fn effective_log_level(config: &LoggingOptions) -> anyhow::Result<Level> {
    parse_log_level(&config.global.level)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::Command};

    use tempfile::tempdir;

    #[test]
    fn default_logging_has_a_global_output() {
        let config = LoggingOptions::default();
        assert_eq!(config.global.name, "global");
        assert!(config.global.targets.is_empty());
        assert!(config.components.is_empty());
    }

    #[test]
    fn effective_level_comes_from_global_output() {
        let config = LoggingOptions {
            global: ComponentLoggingOptions {
                level: "debug".to_string(),
                ..default_component("global")
            },
            components: vec![ComponentLoggingOptions {
                level: "error".to_string(),
                targets: vec!["synctv::health".to_string()],
                ..default_component("health")
            }],
        };
        assert_eq!(
            effective_log_level(&config).expect("global level should be valid"),
            Level::DEBUG
        );
    }

    #[test]
    fn component_targets_are_excluded_from_global_layer() {
        let component_targets = vec!["synctv::health".to_string()];
        let health = vec!["synctv::health".to_string()];
        assert!(target_is_owned_by_component(
            "synctv::health::probe",
            false,
            &health,
            &component_targets
        ));
        assert!(!target_is_owned_by_component(
            "synctv::health::probe",
            true,
            &[],
            &component_targets
        ));
        assert!(target_is_owned_by_component(
            "synctv_api_http",
            true,
            &[],
            &component_targets
        ));
    }

    #[test]
    fn overlapping_specialized_routes_are_rejected() {
        let config = LoggingOptions {
            global: default_component("global"),
            components: vec![
                ComponentLoggingOptions {
                    targets: vec!["synctv::cluster".to_string()],
                    ..default_component("cluster")
                },
                ComponentLoggingOptions {
                    targets: vec!["synctv::cluster::health".to_string()],
                    ..default_component("cluster_health")
                },
            ],
        };
        assert!(validate_component_routes(&config).is_err());
    }

    #[test]
    fn components_sharing_standard_output_have_independent_workers() {
        let config = LoggingOptions {
            global: default_component("global"),
            components: vec![ComponentLoggingOptions {
                targets: vec!["synctv::health".to_string()],
                ..default_component("health")
            }],
        };
        let (_subscriber, guards) =
            build_subscriber(&config).expect("logging subscriber should build");
        assert_eq!(guards.len(), 2);
        assert_eq!(
            guards.dropped_lines(),
            vec![("global".to_string(), 0), ("health".to_string(), 0)]
        );
    }

    #[test]
    fn duplicate_global_initialization_returns_an_error() {
        const CHILD_ENV: &str = "SYNCTV_TEST_DUPLICATE_LOGGING_INIT";
        if std::env::var_os(CHILD_ENV).is_some() {
            tracing_subscriber::registry()
                .try_init()
                .expect("first global subscriber should install");
            let error = init_logging(&LoggingOptions::default())
                .expect_err("second global subscriber should return an error");
            assert!(error
                .to_string()
                .contains("failed to install global logging subscriber"));
            return;
        }

        let status = Command::new(std::env::current_exe().expect("test executable should exist"))
            .arg("--exact")
            .arg("logging::tests::duplicate_global_initialization_returns_an_error")
            .env(CHILD_ENV, "1")
            .status()
            .expect("duplicate initialization child test should run");
        assert!(status.success());
    }

    #[test]
    fn zero_file_retention_is_rejected() {
        let config = LoggingOptions {
            global: ComponentLoggingOptions {
                output: LogOutput::File {
                    path: PathBuf::from("global.log"),
                    rotation: LogRotation::Daily,
                    max_files: 0,
                },
                ..default_component("global")
            },
            components: Vec::new(),
        };
        assert!(validate_component_routes(&config).is_err());
    }

    #[test]
    fn file_appender_prunes_logs_to_retention_limit() {
        let dir = tempdir().expect("temporary log directory should be created");
        for date in ["2026-07-27", "2026-07-28", "2026-07-29"] {
            fs::write(dir.path().join(format!("server.{date}.log")), "old log")
                .expect("old log fixture should be written");
        }

        let appender = build_file_appender(
            &dir.path().join("server.log"),
            LogRotation::Daily,
            2,
            "server",
        )
        .expect("rolling appender should build");
        drop(appender);

        let retained = fs::read_dir(dir.path())
            .expect("log directory should be readable")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("server.") && name.ends_with(".log"))
            })
            .count();
        assert_eq!(retained, 2);
    }

    #[test]
    fn component_layers_use_independent_routes_levels_formats_and_files() {
        let dir = tempdir().expect("temporary log directory should be created");
        let config = LoggingOptions {
            global: ComponentLoggingOptions {
                format: "json".to_string(),
                output: LogOutput::File {
                    path: dir.path().join("global.log"),
                    rotation: LogRotation::Never,
                    max_files: 2,
                },
                ..default_component("global")
            },
            components: vec![ComponentLoggingOptions {
                targets: vec!["synctv::health".to_string()],
                level: "warn".to_string(),
                output: LogOutput::File {
                    path: dir.path().join("health.log"),
                    rotation: LogRotation::Never,
                    max_files: 2,
                },
                ..default_component("health")
            }],
        };
        let (subscriber, guards) =
            build_subscriber(&config).expect("logging subscriber should build");
        assert_eq!(guards.len(), 2);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "synctv_core::cache", "global-only-event");
            tracing::info!(target: "synctv::health", "filtered-health-event");
            tracing::warn!(target: "synctv::health", "health-only-event");
            let span = tracing::info_span!("silent-span");
            drop(span.enter());
        });
        drop(guards);

        let global_log = read_log_with_prefix(dir.path(), "global");
        let health_log = read_log_with_prefix(dir.path(), "health");
        assert!(global_log.contains("global-only-event"));
        assert!(global_log.contains("\"target\":\"synctv_core::cache\""));
        assert!(!global_log.contains("health-only-event"));
        assert!(!global_log.contains("filtered-health-event"));
        assert!(!global_log.contains("silent-span"));
        assert!(health_log.contains("health-only-event"));
        assert!(!health_log.contains("filtered-health-event"));
        assert!(!health_log.contains("global-only-event"));
        assert!(!health_log.trim_start().starts_with('{'));
    }

    #[test]
    fn access_component_uses_compact_context_free_text() {
        let dir = tempdir().expect("temporary log directory should be created");
        let config = LoggingOptions {
            global: default_component("global"),
            components: vec![ComponentLoggingOptions {
                name: "access".to_string(),
                style: LogStyle::Access,
                targets: vec!["synctv::access".to_string()],
                output: LogOutput::File {
                    path: dir.path().join("access.log"),
                    rotation: LogRotation::Never,
                    max_files: 2,
                },
                ..default_component("access")
            }],
        };
        let (subscriber, guards) =
            build_subscriber(&config).expect("logging subscriber should build");

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("request", secret = "must-not-be-inherited");
            let _guard = span.enter();
            tracing::info!(
                target: "synctv::access",
                protocol = "http",
                status = 200,
                "request completed"
            );
        });
        drop(guards);

        let access_log = read_log_with_prefix(dir.path(), "access");
        assert!(access_log.contains("request completed"));
        assert!(access_log.contains("protocol=\"http\""));
        assert!(access_log.contains("status=200"));
        assert!(!access_log.contains("synctv::access"));
        assert!(!access_log.contains("must-not-be-inherited"));
        assert!(!access_log.contains("logging.rs"));
    }

    fn read_log_with_prefix(dir: &Path, prefix: &str) -> String {
        let path = fs::read_dir(dir)
            .expect("log directory should be readable")
            .map(|entry| {
                entry
                    .expect("log directory entry should be readable")
                    .path()
            })
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(prefix))
            })
            .expect("component log file should exist");
        fs::read_to_string(path).expect("component log file should be readable")
    }

    fn default_component(name: &str) -> ComponentLoggingOptions {
        ComponentLoggingOptions {
            name: name.to_string(),
            style: LogStyle::Diagnostic,
            targets: Vec::new(),
            level: "info".to_string(),
            format: "text".to_string(),
            output: LogOutput::Stdout,
            color: LogColor::Auto,
        }
    }
}
