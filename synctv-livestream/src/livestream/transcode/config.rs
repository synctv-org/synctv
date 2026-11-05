//! Transcode configuration.
//!
//! The scheduler never reads worker internals such as `max_concurrent`;
//! capacity surfaces only through the structured
//! [`CapacityExhausted`](super::worker::TranscodeError::CapacityExhausted)
//! rejection, and workers are tried in priority order until one accepts.
//!
//! File shape (`livestream.transcode`):
//!
//! ```toml
//! [livestream.transcode]
//! enabled = true
//!
//! [[livestream.transcode.workers]]
//! engine = "ffmpeg"
//! name = "local"
//! priority = 100
//! max_concurrent = 2
//! binary = "ffmpeg"
//!
//! [[livestream.transcode.presets]]
//! name = "720p"
//! height = 720
//! video_kbps = 2500
//! audio_kbps = 96
//! max_fps = 30
//! ```
//!
//! Workers may mix engines and repeat engines; a job goes to the
//! highest-priority worker that accepts it, so a lower-priority remote
//! transcoder naturally acts as overflow capacity.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

pub const DEFAULT_WORKER_PRIORITY: u32 = 100;

/// Root of `livestream.transcode` configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeConfig {
    /// Master switch. Transcode is an expensive, opt-in capability.
    #[serde(default)]
    pub enabled: bool,
    /// Transcode workers in priority order; the first worker that accepts a
    /// job runs it. Later entries act as overflow for capacity rejections.
    #[serde(default = "default_workers")]
    pub workers: Vec<WorkerConfig>,
    /// Quality rungs offered on live streams, ordered best-first.
    #[serde(default = "default_presets")]
    pub presets: Vec<TranscodePreset>,
}

/// One transcode worker instance: scheduling attributes plus an engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerConfig {
    /// Instance label used in logs; defaults to the engine name plus index.
    #[serde(default)]
    pub name: Option<String>,
    /// Higher priority workers receive jobs first; ties keep list order.
    #[serde(default = "default_worker_priority")]
    pub priority: u32,
    /// The engine and its settings.
    #[serde(flatten)]
    pub engine: WorkerEngine,
}

const fn default_worker_priority() -> u32 {
    DEFAULT_WORKER_PRIORITY
}

/// Engine selection, tagged by `engine` like the storage backends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "engine", rename_all = "snake_case")]
pub enum WorkerEngine {
    /// External ffmpeg binary engine.
    Ffmpeg(FfmpegWorkerConfig),
}

impl WorkerEngine {
    /// Engine name used in logs.
    #[must_use]
    pub const fn engine_name(&self) -> &'static str {
        match self {
            Self::Ffmpeg(_) => "ffmpeg",
        }
    }
}

/// External ffmpeg binary engine settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FfmpegWorkerConfig {
    /// Maximum transcode jobs this worker runs concurrently.
    pub max_concurrent: usize,
    /// ffmpeg executable. Resolved through `PATH` unless it contains a path
    /// separator. The binary is external to SyncTV, so SyncTV distributions
    /// carry no ffmpeg license obligations.
    pub binary: String,
}

impl Default for FfmpegWorkerConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 1,
            binary: "ffmpeg".to_string(),
        }
    }
}

/// One quality rung of the transcode ladder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodePreset {
    /// Short label exposed to viewers, e.g. `480p`.
    pub name: String,
    /// Output height; width follows the source aspect ratio. Rungs at or
    /// above the source resolution are skipped per stream.
    pub height: u32,
    /// Target video bitrate in kbps.
    pub video_kbps: u32,
    /// Target audio bitrate in kbps.
    pub audio_kbps: u32,
    /// Output frame-rate ceiling.
    pub max_fps: u32,
}

/// Default quality ladder: a single 720p rung. Lower rungs rarely add
/// reach and cost another encoder pass each.
pub const DEFAULT_PRESETS: &[(&str, u32, u32, u32, u32)] = &[("720p", 720, 2500, 96, 30)];

fn default_workers() -> Vec<WorkerConfig> {
    vec![WorkerConfig {
        name: None,
        priority: DEFAULT_WORKER_PRIORITY,
        engine: WorkerEngine::Ffmpeg(FfmpegWorkerConfig::default()),
    }]
}

fn default_presets() -> Vec<TranscodePreset> {
    DEFAULT_PRESETS
        .iter()
        .map(
            |&(name, height, video_kbps, audio_kbps, max_fps)| TranscodePreset {
                name: name.to_string(),
                height,
                video_kbps,
                audio_kbps,
                max_fps,
            },
        )
        .collect()
}

impl Default for TranscodeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            workers: default_workers(),
            presets: default_presets(),
        }
    }
}

impl WorkerConfig {
    /// Stable label for logs.
    #[must_use]
    pub fn label(&self, index: usize) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format!("{}#{index}", self.engine.engine_name()))
    }
}

impl TranscodeConfig {
    /// Validate the configuration; returns a human-readable problem list.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.workers.is_empty() {
            problems.push("livestream.transcode.workers must not be empty".to_string());
        }
        let mut names = HashSet::new();
        for (index, worker) in self.workers.iter().enumerate() {
            let label = worker.label(index);
            if let Some(name) = &worker.name {
                if name.trim().is_empty() {
                    problems.push(format!("transcode worker '{label}' has an empty name"));
                } else if !names.insert(name.clone()) {
                    problems.push(format!("transcode worker name '{name}' is duplicated"));
                }
            }
            match &worker.engine {
                WorkerEngine::Ffmpeg(ffmpeg) => {
                    if ffmpeg.max_concurrent == 0 {
                        problems.push(format!(
                            "transcode worker '{label}' max_concurrent must be at least 1"
                        ));
                    }
                    if ffmpeg.binary.trim().is_empty() {
                        problems.push(format!(
                            "transcode worker '{label}' binary must not be empty"
                        ));
                    }
                }
            }
        }
        if self.presets.is_empty() {
            problems.push("livestream.transcode.presets must not be empty".to_string());
        }
        let mut preset_names = HashSet::new();
        for preset in &self.presets {
            if preset.name.trim().is_empty() {
                problems.push("transcode preset names must not be empty".to_string());
            } else if let Err(error) = crate::util::validate_stream_id_component(
                &super::rung_stream_name("0", &preset.name),
                "rung stream name",
            ) {
                // The preset name rides inside the rung stream key, so it
                // must satisfy the same identifier rules the registry and
                // relay enforce (`:`, path separators, control characters).
                problems.push(format!(
                    "transcode preset name '{}' is not usable as a stream key: {error}",
                    preset.name
                ));
            } else if !preset_names.insert(preset.name.clone()) {
                problems.push(format!(
                    "transcode preset name '{}' is duplicated",
                    preset.name
                ));
            }
            if preset.height == 0 || preset.height > 4320 {
                problems.push(format!(
                    "transcode preset '{}' height {} is out of range (1..=4320)",
                    preset.name, preset.height
                ));
            }
            if preset.video_kbps == 0 || preset.video_kbps > 100_000 {
                problems.push(format!(
                    "transcode preset '{}' video_kbps {} is out of range (1..=100000)",
                    preset.name, preset.video_kbps
                ));
            }
            if preset.audio_kbps == 0 || preset.audio_kbps > 512 {
                problems.push(format!(
                    "transcode preset '{}' audio_kbps {} is out of range (1..=512)",
                    preset.name, preset.audio_kbps
                ));
            }
            if preset.max_fps == 0 || preset.max_fps > 120 {
                problems.push(format!(
                    "transcode preset '{}' max_fps {} is out of range (1..=120)",
                    preset.name, preset.max_fps
                ));
            }
        }
        if !problems.is_empty() && self.enabled {
            problems.insert(
                0,
                "livestream.transcode.enabled is true but the configuration is invalid".to_string(),
            );
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let config = TranscodeConfig::default();
        assert!(config.validate().is_empty(), "{:?}", config.validate());
        assert_eq!(config.workers.len(), 1);
        assert!(matches!(config.workers[0].engine, WorkerEngine::Ffmpeg(_)));
        assert_eq!(config.workers[0].label(0), "ffmpeg#0");
    }

    #[test]
    fn unknown_engine_type_is_rejected_by_serde() {
        let result = toml::from_str::<TranscodeConfig>(
            "enabled = true\n[[workers]]\nengine = \"remote-x\"\n",
        );
        let error = result.expect_err("unknown engine must be rejected");
        assert!(
            error.to_string().contains("remote-x"),
            "error should name the unknown engine: {error}"
        );
    }

    #[test]
    fn worker_names_must_be_unique_and_non_empty() {
        let mut config = TranscodeConfig::default();
        config.workers[0].name = Some("gpu".to_string());
        config.workers.push(WorkerConfig {
            name: Some("gpu".to_string()),
            priority: 10,
            engine: WorkerEngine::Ffmpeg(FfmpegWorkerConfig::default()),
        });
        let problems = config.validate();
        assert!(problems
            .iter()
            .any(|problem| problem.contains("duplicated")));

        config.workers[0].name = Some("  ".to_string());
        let problems = config.validate();
        assert!(problems
            .iter()
            .any(|problem| problem.contains("empty name")));
    }

    #[test]
    fn preset_names_must_be_unique_and_stream_key_safe() {
        let mut config = TranscodeConfig::default();
        let base = config.presets[0].clone();
        config.presets.push(base.clone());
        config.presets.push(TranscodePreset {
            name: "480:p".to_string(),
            ..base
        });
        let problems = config.validate();
        assert!(problems
            .iter()
            .any(|problem| problem.contains("duplicated")));
        assert!(problems
            .iter()
            .any(|problem| problem.contains("not usable as a stream key")));
    }

    #[test]
    fn zero_max_concurrent_is_reported() {
        let mut config = TranscodeConfig::default();
        config.workers[0].engine = WorkerEngine::Ffmpeg(FfmpegWorkerConfig {
            max_concurrent: 0,
            binary: "ffmpeg".to_string(),
        });
        assert!(config
            .validate()
            .iter()
            .any(|problem| problem.contains("max_concurrent must be at least 1")));
    }

    #[test]
    fn deserializes_the_documented_shape() {
        let config: TranscodeConfig = toml::from_str(
            r#"
            enabled = true
            [[workers]]
            engine = "ffmpeg"
            name = "local"
            priority = 100
            max_concurrent = 2
            binary = "/usr/local/bin/ffmpeg"
            [[workers]]
            engine = "ffmpeg"
            name = "overflow"
            priority = 10
            max_concurrent = 1
            [[presets]]
            name = "540p"
            height = 540
            video_kbps = 1400
            audio_kbps = 96
            max_fps = 30
            "#,
        )
        .expect("documented shape must parse");
        assert!(config.enabled);
        assert_eq!(config.workers.len(), 2);
        assert_eq!(config.workers[0].name.as_deref(), Some("local"));
        assert_eq!(config.workers[1].priority, 10);
        let WorkerEngine::Ffmpeg(ffmpeg) = &config.workers[0].engine;
        assert_eq!(ffmpeg.binary, "/usr/local/bin/ffmpeg");
        assert_eq!(config.presets.len(), 1);
        assert_eq!(config.presets[0].name, "540p");
        assert!(config.validate().is_empty());
    }

    #[test]
    fn defaults_round_trip_with_missing_sections() {
        let config: TranscodeConfig =
            toml::from_str("enabled = true").expect("sparse shape must parse");
        assert_eq!(config.workers.len(), 1);
        assert_eq!(config.workers[0].priority, DEFAULT_WORKER_PRIORITY);
        assert_eq!(config.presets.len(), DEFAULT_PRESETS.len());
        assert!(config.validate().is_empty());
    }
}
