//! Engine-agnostic transcode worker contract.
//!
//! The scheduler knows nothing about worker internals such as local capacity:
//! it repeatedly submits the most-demanded job and reacts to the structured
//! [`TranscodeError::CapacityExhausted`] rejection by backing off briefly and
//! re-evaluating demand, which may select a different job by then.

use std::sync::Arc;

use synctv_xiu::streamhub::define::{FrameData, FrameDataReceiver};

use super::config::TranscodePreset;

/// A single transcode assignment: one source stream, one output rung.
#[derive(Debug, Clone)]
pub struct TranscodeJob {
    pub room_id: String,
    pub media_id: String,
    pub preset: TranscodePreset,
}

impl TranscodeJob {
    /// Registry/StreamHub stream name of the rung produced by this job.
    #[must_use]
    pub fn rung_stream_name(&self) -> String {
        super::rung_stream_name(&self.media_id, &self.preset.name)
    }
}

/// Structured errors the scheduler understands. Anything else aborts only the
/// affected job.
#[derive(Debug, thiserror::Error)]
pub enum TranscodeError {
    /// The worker cannot accept the job right now. The scheduler backs off
    /// and re-picks the most-demanded job, which may be a different one.
    #[error("worker capacity is exhausted")]
    CapacityExhausted,
    /// The job can never run (e.g. the source codec cannot be decoded).
    /// The scheduler drops it for the lifetime of the source stream.
    #[error("job unsupported: {0}")]
    Unsupported(String),
    /// The job started and then failed at runtime.
    #[error("transcode failed: {0}")]
    Failed(String),
}

/// Live FLV-format frames flowing out of a running job.
pub type TranscodeOutputReceiver = tokio::sync::mpsc::Receiver<FrameData>;

/// Handle to a running job. Dropping it cancels the job.
pub struct TranscodeJobHandle {
    /// Completes with the job outcome after the job ends.
    pub done: tokio::sync::oneshot::Receiver<Result<(), TranscodeError>>,
    /// Cancels the job when dropped.
    pub cancel: tokio_util::sync::CancellationToken,
}

/// Input side handed to a worker: a subscription of the source stream's
/// FLV-format frames plus the stream's first metadata (used to skip rungs at
/// or above the source resolution before the job is even submitted).
pub struct TranscodeInput {
    /// FLV-format frames of the source stream.
    pub frames: FrameDataReceiver,
    /// `width`/`height` parsed from the source `onMetaData`, when observed.
    pub source_resolution: Arc<std::sync::Mutex<Option<(u32, u32)>>>,
}

/// A transcode engine. Implementations own their capacity policy and expose
/// it to the scheduler only through [`TranscodeError::CapacityExhausted`].
pub trait TranscodeWorker: Send + Sync {
    /// Human-readable engine name used in logs.
    fn name(&self) -> &'static str;

    /// Start a job, or reject it. Every immediate rejection returns the
    /// input untouched so the scheduler can offer the job to the next
    /// worker: [`TranscodeError::CapacityExhausted`] must be returned
    /// without side effects when the worker is full.
    fn start(
        &self,
        job: TranscodeJob,
        input: TranscodeInput,
    ) -> Result<(TranscodeJobHandle, TranscodeOutputReceiver), (TranscodeError, TranscodeInput)>;
}
