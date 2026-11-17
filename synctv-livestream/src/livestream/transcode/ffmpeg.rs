//! External ffmpeg binary worker.
//!
//! Runs the configured ffmpeg executable as a subprocess: the source
//! stream's FLV frames are muxed into its stdin, and its stdout FLV is
//! demuxed back into frames. SyncTV never links or bundles ffmpeg, so its
//! license obligations stay with the operator-provided binary.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use bytes::{Buf, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use synctv_xiu::streamhub::define::FrameData;

use super::config::FfmpegWorkerConfig;
use super::worker::{
    TranscodeError, TranscodeInput, TranscodeJob, TranscodeJobHandle, TranscodeOutputReceiver,
    TranscodeWorker,
};

/// FLV framing constants for the stdin muxer and stdout demuxer.
const FLV_HEADER: &[u8; 13] = b"FLV\x01\x05\x00\x00\x00\x09\x00\x00\x00\x00";
const FLV_TAG_HEADER_SIZE: usize = 11;
const FLV_PREV_TAG_SIZE_LEN: usize = 4;
const FLV_TAG_AUDIO: u8 = 8;
const FLV_TAG_VIDEO: u8 = 9;
const FLV_TAG_SCRIPT_DATA: u8 = 18;
/// Cap on buffered stdout bytes before a tag completes; guards against a
/// corrupt stream exhausting memory.
const MAX_FLV_BUFFER_SIZE: usize = 32 * 1024 * 1024;
const MAX_FLV_TAG_SIZE: usize = 16 * 1024 * 1024;
/// Bounded channel so a slow rung consumer applies backpressure to ffmpeg's
/// stdout pipe (ffmpeg blocks on write) instead of buffering without bound.
const OUTPUT_CHANNEL_CAPACITY: usize = 64;
/// Bytes of stderr kept as a ring for failure diagnostics.
const STDERR_TAIL_BYTES: usize = 8 * 1024;

/// External ffmpeg binary engine. Capacity is a local-slot semaphore the
/// scheduler never sees; it only observes [`TranscodeError::CapacityExhausted`].
pub struct FfmpegWorker {
    binary: PathBuf,
    x264_preset: super::config::X264Preset,
    slots: Arc<Semaphore>,
}

impl FfmpegWorker {
    /// Resolve the ffmpeg binary once. A missing binary is logged here and
    /// surfaces per job as a clear failure.
    #[must_use]
    pub fn new(config: &FfmpegWorkerConfig) -> Arc<Self> {
        let binary = resolve_binary(&config.binary);
        if binary.is_none() {
            warn!(
                binary = %config.binary,
                "Configured ffmpeg binary not found; livestream transcode jobs will fail until it is installed"
            );
        }
        Arc::new(Self {
            binary: binary.unwrap_or_else(|| PathBuf::from(&config.binary)),
            x264_preset: config.x264_preset,
            slots: Arc::new(Semaphore::new(config.max_concurrent)),
        })
    }

    #[must_use]
    pub fn binary(&self) -> &std::path::Path {
        &self.binary
    }
}

fn resolve_binary(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let path = PathBuf::from(name);
        return path.is_file().then_some(path);
    }
    let path = std::env::var("PATH").ok()?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

/// How long the worker waits for the source's first `onMetaData` before
/// falling back to tier-default encoding parameters. The tag is the first
/// FLV tag of a live stream, so this wait ends within milliseconds in
/// practice; only metadata-less sources pay the full wait.
const SOURCE_METADATA_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

impl TranscodeWorker for FfmpegWorker {
    fn name(&self) -> &'static str {
        "ffmpeg"
    }

    fn start(
        &self,
        job: TranscodeJob,
        input: TranscodeInput,
    ) -> Result<(TranscodeJobHandle, TranscodeOutputReceiver), (TranscodeError, TranscodeInput)>
    {
        // Pre-check the binary so a missing installation is a clear, instant
        // failure instead of a confusing spawn error.
        if !self.binary.is_file() {
            return Err((
                TranscodeError::Failed(format!(
                    "ffmpeg binary '{}' not found",
                    self.binary.display()
                )),
                input,
            ));
        }
        // Capacity is a non-blocking slot check: a full worker rejects
        // immediately and the scheduler re-picks after a short backoff. The
        // permit lives until the watcher task ends, which releases the slot.
        let Ok(permit) = self.slots.clone().try_acquire_owned() else {
            return Err((TranscodeError::CapacityExhausted, input));
        };

        let binary = self.binary.clone();
        let x264_preset = self.x264_preset;
        // Destructure before spawning: everything after this point belongs
        // to the orchestrator task; only an immediate rejection above hands
        // the input back untouched.
        let TranscodeInput {
            mut frames,
            source_resolution,
            source_frame_rate,
            source_video_bitrate_kbps,
        } = input;

        let (output_tx, output_rx) = mpsc::channel::<FrameData>(OUTPUT_CHANNEL_CAPACITY);
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let cancel = CancellationToken::new();
        let stderr_tail = Arc::new(Mutex::new(Vec::new()));

        // Orchestrator: wait for the source's first metadata (buffering the
        // frames that arrive meanwhile), then spawn ffmpeg sized to the
        // actual source shape and run the stdio pumps. Failures past this
        // point surface through the `done` channel, not `start`.
        let orchestrator_cancel = cancel.clone();
        tokio::spawn(async move {
            let (buffered, metadata) = wait_for_source_metadata(
                &mut frames,
                &source_resolution,
                &source_frame_rate,
                &source_video_bitrate_kbps,
                SOURCE_METADATA_WAIT,
                &orchestrator_cancel,
            )
            .await;
            if orchestrator_cancel.is_cancelled() {
                let _ = done_tx.send(Ok(()));
                return;
            }

            let args = build_ffmpeg_args(x264_preset, &job, &metadata);
            let mut command = tokio::process::Command::new(&binary);
            command
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(error) => {
                    let _ = done_tx.send(Err(TranscodeError::Failed(format!(
                        "failed to spawn ffmpeg: {error}"
                    ))));
                    return;
                }
            };

            let mut stdin = child.stdin.take().expect("ffmpeg stdin piped");
            let mut stdout = child.stdout.take().expect("ffmpeg stdout piped");
            let mut stderr = child.stderr.take().expect("ffmpeg stderr piped");

            // stdout pump: demux ffmpeg's FLV output back into frames.
            let stdout_cancel = orchestrator_cancel.clone();
            let stdout_output = output_tx.clone();
            tokio::spawn(async move {
                let mut demuxer = FlvIncrementalDemuxer::default();
                let mut buffer = BytesMut::new();
                let mut chunk = vec![0u8; 64 * 1024];
                loop {
                    let read = tokio::select! {
                        () = stdout_cancel.cancelled() => break,
                        read = stdout.read(&mut chunk) => match read {
                            Ok(0) | Err(_) => break,
                            Ok(read) => read,
                        },
                    };
                    if buffer.len() + read > MAX_FLV_BUFFER_SIZE {
                        warn!("ffmpeg stdout exceeded FLV buffer limit");
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..read]);
                    while let Some(frame) = demuxer.next_tag(&mut buffer) {
                        if stdout_output.send(frame).await.is_err() {
                            // Consumer gone; the publisher side ends the job.
                            return;
                        }
                    }
                }
            });

            // stderr: keep a bounded tail for failure diagnostics and surface
            // complete lines live, so an input ffmpeg cannot decode is visible
            // while the job runs instead of only after it exits.
            let stderr_cancel = orchestrator_cancel.clone();
            let stderr_tail_writer = Arc::clone(&stderr_tail);
            tokio::spawn(async move {
                let mut tail = Vec::new();
                let mut partial_line = Vec::new();
                let mut chunk = [0u8; 4 * 1024];
                loop {
                    let read = tokio::select! {
                        () = stderr_cancel.cancelled() => break,
                        read = stderr.read(&mut chunk) => match read {
                            Ok(0) | Err(_) => break,
                            Ok(read) => read,
                        },
                    };
                    if tail.len() + read > STDERR_TAIL_BYTES {
                        let overflow = tail.len() + read - STDERR_TAIL_BYTES;
                        tail.drain(..overflow.min(tail.len()));
                    }
                    tail.extend_from_slice(&chunk[..read]);
                    stderr_tail_writer
                        .lock()
                        .expect("stderr tail poisoned")
                        .clone_from(&tail);
                    partial_line.extend_from_slice(&chunk[..read]);
                    while let Some(newline) = partial_line.iter().position(|byte| *byte == b'\n') {
                        let line: Vec<u8> = partial_line.drain(..=newline).collect();
                        let line = String::from_utf8_lossy(&line);
                        let line = line.trim_end();
                        if !line.is_empty() {
                            debug!(target: "synctv::ffmpeg", "{line}");
                        }
                    }
                }
                let leftover = String::from_utf8_lossy(&partial_line);
                let leftover = leftover.trim_end();
                if !leftover.is_empty() {
                    debug!(target: "synctv::ffmpeg", "{leftover}");
                }
            });

            // watcher: await the process and produce the job outcome. Holding
            // `permit` here ties the capacity slot to the job's lifetime.
            let watcher_cancel = orchestrator_cancel.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let cancelled_upfront = watcher_cancel.is_cancelled();
                let status = tokio::select! {
                    status = child.wait() => status,
                    () = watcher_cancel.cancelled() => {
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                        let _ = done_tx.send(Ok(()));
                        return;
                    }
                };
                let outcome = match status {
                    Ok(status) if status.success() || cancelled_upfront => Ok(()),
                    Ok(status) => {
                        let tail_bytes = stderr_tail.lock().expect("stderr tail poisoned");
                        let tail = String::from_utf8_lossy(&tail_bytes);
                        let tail = tail.trim();
                        let detail = if tail.is_empty() {
                            format!("ffmpeg exited with {status}")
                        } else {
                            format!("ffmpeg exited with {status}: {tail}")
                        };
                        Err(TranscodeError::Failed(detail))
                    }
                    Err(error) => Err(TranscodeError::Failed(format!(
                        "failed to wait for ffmpeg: {error}"
                    ))),
                };
                let _ = done_tx.send(outcome);
            });

            // stdin pump (inline): FLV-mux the buffered prefix and the source
            // frames that keep arriving into ffmpeg's stdin.
            if stdin.write_all(FLV_HEADER).await.is_err() {
                return;
            }
            for frame in &buffered {
                if write_flv_tag(&mut stdin, frame).await.is_err() {
                    return;
                }
            }
            loop {
                let frame = tokio::select! {
                    () = orchestrator_cancel.cancelled() => break,
                    frame = frames.recv() => match frame {
                        Some(frame) => frame,
                        None => break,
                    },
                };
                if write_flv_tag(&mut stdin, &frame).await.is_err() {
                    break;
                }
            }
            // Closing stdin lets ffmpeg finish its output and exit cleanly.
            let _ = stdin.shutdown().await;
        });

        Ok((
            TranscodeJobHandle {
                done: done_rx,
                cancel: cancel.clone(),
            },
            output_rx,
        ))
    }
}

/// Wait (bounded) for the source's first `onMetaData`, buffering the frames
/// that arrive meanwhile so nothing is lost while ffmpeg is not yet running.
///
/// `onMetaData` carries everything the encoder publishes in a single tag, so
/// the wait ends at the first metadata frame; sources without the tag fall
/// through to the deadline and the rung encodes with tier defaults. The
/// parsed values are also written into the shared slots so the manager's
/// metadata watcher records them for future eligibility decisions.
async fn wait_for_source_metadata(
    frames: &mut synctv_xiu::streamhub::define::FrameDataReceiver,
    resolution_slot: &Arc<Mutex<Option<(u32, u32)>>>,
    frame_rate_slot: &Arc<Mutex<Option<f64>>>,
    video_bitrate_slot: &Arc<Mutex<Option<u32>>>,
    deadline: std::time::Duration,
    cancel: &CancellationToken,
) -> (Vec<FrameData>, super::SourceMetadata) {
    let mut buffered = Vec::new();
    let mut metadata = super::SourceMetadata::default();
    let sleep = tokio::time::sleep(deadline);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            () = &mut sleep => break,
            frame = frames.recv() => match frame {
                None => break,
                Some(FrameData::MetaData { data, timestamp }) => {
                    metadata = super::parse_source_metadata(&data);
                    if let Some(resolution) = metadata.resolution {
                        *resolution_slot.lock().expect("resolution slot poisoned") = Some(resolution);
                    }
                    if let Some(frame_rate) = metadata.frame_rate {
                        *frame_rate_slot.lock().expect("frame-rate slot poisoned") = Some(frame_rate);
                    }
                    if let Some(video_bitrate) = metadata.video_bitrate_kbps {
                        *video_bitrate_slot.lock().expect("bitrate slot poisoned") = Some(video_bitrate);
                    }
                    buffered.push(FrameData::MetaData { data, timestamp });
                    break;
                }
                Some(frame) => buffered.push(frame),
            },
        }
    }
    (buffered, metadata)
}

/// ffmpeg invocation for one rung: FLV in on stdin, scaled/re-encoded FLV
/// out on stdout.
///
/// `metadata` is the source's `onMetaData` when it was observed in time.
/// The frame-rate ceiling and the never-upscale scale filter already adapt
/// to any source shape; the metadata additionally lets the video bitrate
/// track the actual output: `video_kbps` is the reference bitrate for the
/// tier at a 16:9 output and the `max_fps` ceiling, and it is scaled
/// proportionally for other pixel counts (vertical, ultrawide, non-standard
/// sizes) and lower frame rates. The GOP size is likewise derived from the
/// source's published frame rate capped at the ceiling, so the keyframe
/// cadence stays at two seconds for every source rate.
fn build_ffmpeg_args(
    x264_preset: super::config::X264Preset,
    job: &TranscodeJob,
    metadata: &super::SourceMetadata,
) -> Vec<String> {
    let preset = &job.preset;
    let height = preset.height;
    let fps = preset.max_fps;
    let video_kbps = super::effective_video_kbps(preset, metadata);
    // Constrained ABR with the ceiling equal to the target — the form OBS,
    // PeerTube and Jellyfin all use — and a 2-second VBV window, which
    // keeps downstream HLS segment sizes even without paying CBR filler.
    let buf_size = video_kbps * 2;
    // GOP sized for a 2-second cadence at the frames the encoder actually
    // emits: the source's published rate capped at the tier ceiling (a
    // 24 fps film source gets g=48, not the ceiling's 60 frames = 2.5 s).
    // Metadata-less sources fall back to the ceiling; `keyint_min` equals
    // `g` so the cadence is fixed (scene-cut keyframes are disabled below,
    // so the minimum never overrides the maximum in practice).
    let gop_fps = metadata
        .frame_rate
        .filter(|rate| rate.is_finite() && *rate > 0.0)
        .map_or(f64::from(fps), |rate| {
            rate.round().min(f64::from(fps)).max(1.0)
        });
    // Range-checked by the clamp (the fps ceiling is validated to 1..=120).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let gop = ((gop_fps * 2.0).round().clamp(2.0, f64::from(u32::MAX)) as u32).max(2);

    [
        "-hide_banner",
        "-loglevel",
        "warning",
        // Input: the source stream's FLV frames on stdin. `find_stream_info`
        // on a live pipe never concludes on its own: the FLV script-data
        // stream (the `onMetaData` tag) reports no codec parameters, so the
        // default analysis window burns its full five seconds of realtime
        // input before the first frame is encoded. A/V parameters come from
        // the sequence headers within the first packets; 100ms of budget
        // bounds rung startup latency (measured: 1.67s -> 0.15s on an A/V
        // source, and a short video-only source would otherwise produce no
        // output at all before it ends) while leaving room for an audio
        // sequence header that trails the video one by a few milliseconds.
        "-analyzeduration",
        "100000",
        "-f",
        "flv",
        "-i",
        "pipe:0",
        // Map whichever of video/audio exists; a missing stream is not fatal.
        "-map",
        "0:v?",
        "-map",
        "0:a?",
        // Never upscale: cap the output height at the source height.
        // Frame rate is a ceiling enforced by timestamp gaps, never a
        // target: a frame is kept only when at least 1/fps seconds passed
        // since the previously kept frame, so a faster source is dropped
        // down to the ceiling while a slower source (e.g. 24 fps film)
        // passes through untouched — frames are never duplicated. The 1ms
        // epsilon rescues sources sitting exactly at the ceiling whose
        // container timestamps are rounded to whole milliseconds (a 24 fps
        // source carries 41/42 ms gaps against a 41.67 ms threshold; 100µs
        // let a third of those frames drop). One millisecond stays an order
        // of magnitude below any real frame interval (≥ 1/120 s) and cannot
        // weaken a genuine cap (e.g. 60→30 compares 16.7 ms gaps against a
        // 32.3 ms threshold). Timestamps are kept variable-frame-rate
        // (`-fps_mode vfr` below), which preserves the source timing of
        // whatever frames survive. The selection runs before the scale so
        // frames the ceiling drops are never scaled first (measured
        // 1080p60 → 720p30: -18% encoder CPU, bit-identical output).
        "-vf",
        &format!(
            "select='isnan(prev_selected_t)+gte(t-prev_selected_t,1/{fps}-0.001)',\
             scale=w=-2:h='min({height},ih)':flags=bicubic"
        ),
        "-c:v",
        "libx264",
        "-preset",
        x264_preset.as_str(),
        "-tune",
        "zerolatency",
        "-pix_fmt",
        "yuv420p",
        // High profile: what OBS auto-selects and Owncast, go2rtc and
        // Jellyfin all pin; Main additionally forces x264's 8x8 DCT off
        // (`8x8dct=0` in the encoder banner), costing compression
        // efficiency at the same bitrate with no modern player excluded.
        "-profile:v",
        "high",
        // Closed GOP: ffmpeg's libx264 wrapper enables open GOP unless
        // told otherwise, which would let a keyframe reference the
        // previous segment's frames once B-frames exist. With
        // zerolatency's bframes=0 every keyframe is already IDR, so this
        // is a no-op today; ffmpeg's own HLS guidance passes `+cgop`, and
        // the invariant then survives any future tune change.
        "-flags",
        "+cgop",
        "-b:v",
        &format!("{video_kbps}k"),
        "-maxrate",
        &format!("{video_kbps}k"),
        "-bufsize",
        &format!("{buf_size}k"),
        // Fixed 2-second GOP so HLS segments cut cleanly on keyframes.
        "-g",
        &gop.to_string(),
        "-keyint_min",
        &gop.to_string(),
        "-sc_threshold",
        "0",
        "-c:a",
        "aac",
        "-b:a",
        &format!("{}k", preset.audio_kbps),
        // 48 kHz: the broadcast default (OBS ships it) and what modern
        // AAC/HLS pipelines standardize on; the 44.1 kHz FLV constraint
        // documented by SRS only ever applied to legacy RTMP-era players
        // and real rates travel in the AAC config, not the FLV header.
        "-ar",
        "48000",
        "-ac",
        "2",
        // Keep the variable frame rate produced by the timestamp-gap
        // selection; a CFR mode would re-time frames and duplicate to fill
        // gaps, which the frame-rate ceiling must never do.
        "-fps_mode",
        "vfr",
        // Output: FLV on stdout.
        "-f",
        "flv",
        "-flvflags",
        "no_duration_filesize",
        "pipe:1",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// Write one frame as an FLV tag (header + body + previous-tag size).
async fn write_flv_tag<W>(writer: &mut W, frame: &FrameData) -> std::io::Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    const MAX_FLV_TAG_BODY: u32 = 0x00ff_ffff;
    let (tag_type, timestamp, data) = match frame {
        FrameData::Video { timestamp, data } => (FLV_TAG_VIDEO, *timestamp, data),
        FrameData::Audio { timestamp, data } => (FLV_TAG_AUDIO, *timestamp, data),
        FrameData::MetaData { timestamp, data } => (FLV_TAG_SCRIPT_DATA, *timestamp, data),
        FrameData::MediaInfo { .. } => return Ok(()),
    };
    let data_size = u32::try_from(data.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "FLV tag body exceeds u32 size",
        )
    })?;
    if data_size > MAX_FLV_TAG_BODY {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "FLV tag body exceeds the 24-bit size field",
        ));
    }
    // Tag header layout: type, 24-bit size, 24-bit timestamp with the high
    // byte in slot 7, three reserved zero bytes.
    let mut header = [0u8; FLV_TAG_HEADER_SIZE];
    header[0] = tag_type;
    header[1..4].copy_from_slice(&data_size.to_be_bytes()[1..]);
    let timestamp = timestamp.to_be_bytes();
    header[4] = timestamp[1];
    header[5] = timestamp[2];
    header[6] = timestamp[3];
    header[7] = timestamp[0];
    writer.write_all(&header).await?;
    writer.write_all(data).await?;
    let total = u32::try_from(FLV_TAG_HEADER_SIZE + data.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "FLV tag total size exceeds u32",
        )
    })?;
    writer.write_all(&total.to_be_bytes()).await
}

/// Incremental FLV tag parser over a growing buffer, mirroring the
/// HTTP-FLV puller's framing: header on first bytes, then tag header, body,
/// previous-tag-size units.
#[derive(Default)]
struct FlvIncrementalDemuxer {
    header_parsed: bool,
}

impl FlvIncrementalDemuxer {
    /// Extract the next complete tag from `buffer`, consuming its bytes.
    /// Returns `None` when more data is needed.
    fn next_tag(&mut self, buffer: &mut BytesMut) -> Option<FrameData> {
        if !self.header_parsed {
            const HEADER_LEN: usize = 9 + FLV_PREV_TAG_SIZE_LEN;
            if buffer.len() < HEADER_LEN {
                return None;
            }
            if &buffer[0..3] != b"FLV" {
                warn!("ffmpeg stdout is not FLV");
                return None;
            }
            buffer.advance(HEADER_LEN);
            self.header_parsed = true;
        }
        loop {
            if buffer.len() < FLV_TAG_HEADER_SIZE {
                return None;
            }
            let tag_type = buffer[0];
            let data_size = (usize::from(buffer[1]) << 16)
                | (usize::from(buffer[2]) << 8)
                | usize::from(buffer[3]);
            if data_size > MAX_FLV_TAG_SIZE {
                warn!("ffmpeg stdout FLV tag too large: {data_size} bytes");
                return None;
            }
            let total = FLV_TAG_HEADER_SIZE + data_size + FLV_PREV_TAG_SIZE_LEN;
            if buffer.len() < total {
                return None;
            }
            let timestamp = (u32::from(buffer[7]) << 24)
                | (u32::from(buffer[4]) << 16)
                | (u32::from(buffer[5]) << 8)
                | u32::from(buffer[6]);
            buffer.advance(FLV_TAG_HEADER_SIZE);
            let data = buffer.split_to(data_size).freeze();
            buffer.advance(FLV_PREV_TAG_SIZE_LEN);
            match tag_type {
                FLV_TAG_VIDEO => {
                    return Some(FrameData::Video { timestamp, data });
                }
                FLV_TAG_AUDIO => {
                    return Some(FrameData::Audio { timestamp, data });
                }
                FLV_TAG_SCRIPT_DATA => {
                    return Some(FrameData::MetaData { timestamp, data });
                }
                _ => debug!("Skipping unknown FLV tag type {tag_type} from ffmpeg"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset_480p() -> super::super::config::TranscodePreset {
        super::super::config::TranscodePreset {
            name: "480p".to_string(),
            height: 480,
            video_kbps: 1000,
            audio_kbps: 64,
            max_fps: 30,
        }
    }

    #[test]
    fn ffmpeg_args_never_upscale_and_fix_gop() {
        let job = TranscodeJob {
            room_id: "room".to_string(),
            media_id: "media".to_string(),
            preset: preset_480p(),
        };
        let args = build_ffmpeg_args(
            super::super::config::X264Preset::Veryfast,
            &job,
            &super::super::SourceMetadata::default(),
        );
        let joined = args.join(" ");
        assert!(joined.contains("min(480,ih)"), "{joined}");
        // The frame rate is a ceiling enforced by timestamp gaps, never a
        // target: only frames arriving faster than 1/30 s apart are
        // dropped, and slower sources pass through without duplication.
        // Selection precedes the scale so dropped frames are not scaled.
        let vf = &args[args
            .iter()
            .position(|arg| arg == "-vf")
            .expect("-vf present")
            + 1];
        let (select_at, scale_at) = (
            vf.find("select=").expect("select present"),
            vf.find("scale=").expect("scale present"),
        );
        assert!(select_at < scale_at, "select must run before scale: {vf}");
        assert!(
            vf.contains("select='isnan(prev_selected_t)+gte(t-prev_selected_t,1/30-0.001)'"),
            "{vf}"
        );
        // Live-pipe input analysis must be bounded or the first frame of a
        // rung waits out the default five-second window.
        assert!(joined.contains("-analyzeduration 100000"), "{joined}");
        assert!(joined.contains("-fps_mode vfr"), "{joined}");
        // Metadata-less sources size the GOP from the tier ceiling with a
        // fixed cadence (keyint_min == g), a High profile and closed GOPs.
        assert!(joined.contains("-g 60"), "{joined}");
        assert!(joined.contains("-keyint_min 60"), "{joined}");
        assert!(joined.contains("-profile:v high"), "{joined}");
        assert!(joined.contains("+cgop"), "{joined}");
        // Constrained ABR: the rate ceiling equals the target bitrate.
        assert!(joined.contains("-b:v 1000k"), "{joined}");
        assert!(joined.contains("-maxrate 1000k"), "{joined}");
        assert!(joined.contains("-bufsize 2000k"), "{joined}");
        assert!(joined.contains("-b:a 64k"), "{joined}");
        assert!(joined.contains("-ar 48000"), "{joined}");
        assert!(joined.ends_with("pipe:1"));
    }

    #[test]
    fn ffmpeg_args_derive_gop_from_the_source_frame_rate() {
        let job = TranscodeJob {
            room_id: "room".to_string(),
            media_id: "media".to_string(),
            preset: preset_480p(),
        };
        let gop_for = |frame_rate: Option<f64>| -> String {
            let args = build_ffmpeg_args(
                super::super::config::X264Preset::Veryfast,
                &job,
                &super::super::SourceMetadata {
                    frame_rate,
                    ..super::super::SourceMetadata::default()
                },
            );
            let g = args.iter().position(|arg| arg == "-g").expect("-g present") + 1;
            args[g].clone()
        };
        // A 24 fps film source under a 30 ceiling: 2-second cadence means
        // 48 frames, not the ceiling's 60.
        assert_eq!(gop_for(Some(24.0)), "48");
        // A source above the ceiling is capped by it.
        assert_eq!(gop_for(Some(59.94)), "60");
        // Sub-1 fps sources still get a valid (minimum) GOP.
        assert_eq!(gop_for(Some(0.4)), "2");
        // Junk is ignored; the tier ceiling applies.
        assert_eq!(gop_for(Some(f64::NAN)), "60");
        assert_eq!(gop_for(None), "60");
    }

    #[test]
    fn ffmpeg_args_pass_the_configured_x264_preset_through() {
        let job = TranscodeJob {
            room_id: "room".to_string(),
            media_id: "media".to_string(),
            preset: preset_480p(),
        };
        for preset in [
            super::super::config::X264Preset::Ultrafast,
            super::super::config::X264Preset::Superfast,
            super::super::config::X264Preset::Veryfast,
            super::super::config::X264Preset::Faster,
            super::super::config::X264Preset::Fast,
            super::super::config::X264Preset::Medium,
            super::super::config::X264Preset::Slow,
            super::super::config::X264Preset::Slower,
            super::super::config::X264Preset::Veryslow,
            super::super::config::X264Preset::Placebo,
        ] {
            let args = build_ffmpeg_args(preset, &job, &super::super::SourceMetadata::default());
            let joined = args.join(" ");
            assert!(
                joined.contains(&format!("-preset {}", preset.as_str())),
                "{joined}"
            );
        }
    }

    /// Runs the exact `-vf` value produced by [`build_ffmpeg_args`] through a
    /// local ffmpeg binary and counts output frames, so the shipped filter
    /// expression is verified against real filter behavior.
    #[test]
    fn fps_filter_expression_caps_without_duplicating() {
        let Some(ffmpeg) = local_ffmpeg() else {
            eprintln!("skipping: ffmpeg binary not found on PATH");
            return;
        };
        let job = TranscodeJob {
            room_id: "room".to_string(),
            media_id: "media".to_string(),
            preset: preset_480p(),
        };
        let args = build_ffmpeg_args(
            super::super::config::X264Preset::Veryfast,
            &job,
            &super::super::SourceMetadata::default(),
        );
        let vf_index = args
            .iter()
            .position(|arg| arg == "-vf")
            .expect("-vf present");
        let vf = &args[vf_index + 1];
        let fps_mode_index = args
            .iter()
            .position(|arg| arg == "-fps_mode")
            .expect("-fps_mode present");
        let fps_mode = args[fps_mode_index + 1].clone();

        let run = |input_rate: u32| -> u64 {
            let output = std::process::Command::new(&ffmpeg)
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("testsrc2=size=320x240:rate={input_rate}"),
                    "-t",
                    "2",
                    "-vf",
                    vf,
                    "-fps_mode",
                    &fps_mode,
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "gray",
                    "pipe:1",
                ])
                .output()
                .expect("ffmpeg runs");
            assert!(
                output.status.success(),
                "ffmpeg failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            u64::try_from(output.stdout.len() / (320 * 240)).expect("frame count fits u64")
        };

        // 60 fps in 2 s: capped to 30 fps -> 60 frames.
        let frames = run(60);
        assert!((58..=62).contains(&frames), "60fps -> {frames} frames");
        // 30 fps in 2 s: already at the ceiling -> 60 frames, untouched.
        let frames = run(30);
        assert!((58..=62).contains(&frames), "30fps -> {frames} frames");
        // 24 fps in 2 s: untouched (48 frames), NOT duplicated to 60.
        let frames = run(24);
        assert!((46..=50).contains(&frames), "24fps -> {frames} frames");
    }

    /// Runs the complete argument vector from [`build_ffmpeg_args`] through a
    /// real ffmpeg over the production pipe topology (FLV on stdin, FLV on
    /// stdout) and verifies the encoded output at the byte level: High
    /// profile in the AVC sequence header, a keyframe cadence of exactly two
    /// seconds derived from each source's own rate, and 48 kHz AAC produced
    /// from a 44.1 kHz source.
    #[test]
    fn full_args_encode_high_profile_2s_gops_and_48k_audio() {
        let Some(ffmpeg) = local_ffmpeg() else {
            eprintln!("skipping: ffmpeg binary not found on PATH");
            return;
        };
        let dir = std::env::temp_dir().join(format!(
            "synctv-transcode-args-e2e-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after the epoch")
                .as_millis()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");

        let job = TranscodeJob {
            room_id: "room".to_string(),
            media_id: "media".to_string(),
            preset: preset_480p(),
        };

        for source_rate in [30u32, 24] {
            let src = dir.join(format!("src{source_rate}.flv"));
            let out = dir.join(format!("out{source_rate}.flv"));

            // A 6-second source with a 3-second GOP and 44.1 kHz audio: any
            // keyframe cadence in the output other than the rung's own
            // 2-second one (source IDRs sit at 0/3/6 s) and any 44.1 kHz
            // audio proves the output is the rung's encoding, not a pass
            // through of the source's parameters.
            let make = std::process::Command::new(&ffmpeg)
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("testsrc2=size=640x360:rate={source_rate}"),
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:sample_rate=44100",
                    "-t",
                    "6",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-g",
                    // 3-second GOP in frames for either source rate.
                    &(source_rate * 3).to_string(),
                    "-pix_fmt",
                    "yuv420p",
                    "-c:a",
                    "aac",
                    "-ar",
                    "44100",
                    "-ac",
                    "2",
                    "-f",
                    "flv",
                ])
                .arg(&src)
                .output()
                .expect("generate source");
            assert!(
                make.status.success(),
                "source generation failed: {}",
                String::from_utf8_lossy(&make.stderr)
            );

            let args = build_ffmpeg_args(
                super::super::config::X264Preset::Veryfast,
                &job,
                &super::super::SourceMetadata {
                    resolution: Some((640, 360)),
                    frame_rate: Some(f64::from(source_rate)),
                    video_bitrate_kbps: None,
                },
            );
            let stdin = std::fs::File::open(&src).expect("open source");
            let stdout = std::fs::File::create(&out).expect("create output");
            let run = std::process::Command::new(&ffmpeg)
                .args(&args)
                .stdin(std::process::Stdio::from(stdin))
                .stdout(std::process::Stdio::from(stdout))
                .output()
                .expect("run rung ffmpeg");
            assert!(
                run.status.success(),
                "rung ffmpeg failed: {}",
                String::from_utf8_lossy(&run.stderr)
            );

            let bytes = std::fs::read(&out).expect("read output");
            let tags = flv_tags(&bytes);

            // High profile: the AVC sequence header's decoder configuration
            // record starts configurationVersion, profile_idc.
            let seq = tags
                .iter()
                .find(|(kind, _, body)| *kind == FLV_TAG_VIDEO && body.len() >= 8 && body[1] == 0)
                .expect("video sequence header tag");
            assert_eq!(
                seq.2[6], 100,
                "profile_idc must be High (100), got {}",
                seq.2[6]
            );

            // 48 kHz audio: the AAC sequence header's AudioSpecificConfig
            // packs samplingFrequencyIndex around the first byte boundary.
            let audio_seq = tags
                .iter()
                .find(|(kind, _, body)| *kind == FLV_TAG_AUDIO && body.len() >= 4 && body[1] == 0)
                .expect("audio sequence header tag");
            let frequency_index = ((audio_seq.2[2] & 0x07) << 1) | (audio_seq.2[3] >> 7);
            assert_eq!(
                frequency_index, 3,
                "AAC sampling index must be 48000 Hz (3), got {frequency_index}"
            );

            // Two-second keyframe cadence at the source's own rate. The
            // invariant is exact in frames — IDRs land every derived-GOP
            // frames (60 at 30 fps, 48 at 24; the source's own 3-second
            // GOP would give 90/72) — while FLV's millisecond timestamps
            // quantize the wall-clock gaps (33.33 ms frames land on a
            // 33/34 ms grid; measured gaps 2040/1966 ms), so the
            // timestamps get a tolerance band instead of equality.
            let video_nalus: Vec<(u32, bool)> = tags
                .iter()
                .filter(|(kind, _, body)| *kind == FLV_TAG_VIDEO && body.len() >= 2 && body[1] == 1)
                .map(|&(_, timestamp, body)| (timestamp, body[0] >> 4 == 1))
                .collect();
            let idr_indices: Vec<usize> = video_nalus
                .iter()
                .enumerate()
                .filter(|(_, (_, keyframe))| *keyframe)
                .map(|(index, _)| index)
                .collect();
            assert_eq!(
                idr_indices.len(),
                3,
                "{source_rate} fps source must produce exactly 3 IDRs in 6 s"
            );
            for pair in idr_indices.windows(2) {
                assert_eq!(
                    pair[1] - pair[0],
                    source_rate as usize * 2,
                    "{source_rate} fps source must space IDRs by the derived GOP"
                );
            }
            let keyframe_ts: Vec<u32> = idr_indices
                .iter()
                .map(|&index| video_nalus[index].0)
                .collect();
            assert!(
                keyframe_ts[0] < 100,
                "first IDR must sit at the stream start, got {}",
                keyframe_ts[0]
            );
            let gaps: Vec<u32> = keyframe_ts
                .windows(2)
                .map(|pair| pair[1] - pair[0])
                .collect();
            assert!(
                gaps.iter().all(|gap| (1900..=2100).contains(gap)),
                "{source_rate} fps IDR gaps must stay within a 2 s ± 5% band, got {gaps:?}"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Walk an FLV byte stream into `(tag-type, timestamp-ms, body)` triples,
    /// mirroring the framing the stdout demuxer consumes.
    fn flv_tags(bytes: &[u8]) -> Vec<(u8, u32, &[u8])> {
        let mut tags = Vec::new();
        if bytes.len() < 9 + 4 {
            return tags;
        }
        let mut pos = 9 + 4;
        while pos + FLV_TAG_HEADER_SIZE <= bytes.len() {
            let tag_type = bytes[pos];
            let data_size = (usize::from(bytes[pos + 1]) << 16)
                | (usize::from(bytes[pos + 2]) << 8)
                | usize::from(bytes[pos + 3]);
            let timestamp = (u32::from(bytes[pos + 4]) << 16)
                | (u32::from(bytes[pos + 5]) << 8)
                | u32::from(bytes[pos + 6])
                | (u32::from(bytes[pos + 7]) << 24);
            let body = pos + FLV_TAG_HEADER_SIZE;
            if body + data_size + 4 > bytes.len() {
                break;
            }
            tags.push((tag_type, timestamp, &bytes[body..body + data_size]));
            pos = body + data_size + 4;
        }
        tags
    }

    fn local_ffmpeg() -> Option<std::path::PathBuf> {
        let path = std::env::var("PATH").ok()?;
        std::env::split_paths(&path)
            .map(|directory| directory.join("ffmpeg"))
            .find(|candidate| candidate.is_file())
    }

    #[test]
    fn flv_tag_framing_round_trips_through_demuxer() {
        let body = [0x17u8, 0x00, 0x00, 0x00, 0x01];
        // FLV header + one video tag with an extended timestamp.
        let mut stream: Vec<u8> = FLV_HEADER.to_vec();
        let timestamp: u32 = 0x0102_0304;
        stream.push(FLV_TAG_VIDEO);
        stream.extend_from_slice(
            &u32::try_from(body.len())
                .expect("body fits u32")
                .to_be_bytes()[1..4],
        );
        stream.extend_from_slice(&timestamp.to_be_bytes()[1..4]);
        stream.push(timestamp.to_be_bytes()[0]);
        stream.extend_from_slice(&[0, 0, 0]);
        stream.extend_from_slice(&body);
        let total = u32::try_from(FLV_TAG_HEADER_SIZE + body.len()).expect("tag size fits u32");
        stream.extend_from_slice(&total.to_be_bytes());

        let mut demuxer = FlvIncrementalDemuxer::default();
        let mut buffer = BytesMut::from(&stream[..]);
        let FrameData::Video {
            timestamp: parsed_ts,
            data: parsed_body,
        } = demuxer.next_tag(&mut buffer).expect("tag should parse")
        else {
            panic!("expected video tag");
        };
        assert_eq!(parsed_ts, timestamp);
        assert_eq!(parsed_body.as_ref(), body);
        assert!(demuxer.next_tag(&mut buffer).is_none());
    }

    #[tokio::test]
    async fn flv_tag_write_matches_framing() {
        let frame = FrameData::Video {
            timestamp: 0x0102_0304,
            data: bytes::Bytes::from_static(&[0x17, 0x00, 0x00, 0x00, 0x01]),
        };
        let mut sink = Vec::new();
        write_flv_tag(&mut sink, &frame)
            .await
            .expect("in-memory write succeeds");

        let mut demuxer = FlvIncrementalDemuxer::default();
        demuxer.header_parsed = true;
        let mut buffer = BytesMut::from(&sink[..]);
        let FrameData::Video { timestamp, data } =
            demuxer.next_tag(&mut buffer).expect("tag should parse")
        else {
            panic!("expected video tag");
        };
        assert_eq!(timestamp, 0x0102_0304);
        assert_eq!(data.as_ref(), frame_data(&frame).as_ref());
    }

    #[test]
    fn effective_bitrate_tracks_output_shape_and_frame_rate() {
        use super::super::SourceMetadata;

        let preset = preset_480p();
        // No metadata: the tier reference bitrate is used verbatim.
        assert_eq!(
            super::super::effective_video_kbps(&preset, &SourceMetadata::default()),
            1000
        );

        // A 16:9 source lands exactly on the reference frame.
        let meta = SourceMetadata {
            resolution: Some((1920, 1080)),
            frame_rate: Some(30.0),
            video_bitrate_kbps: None,
        };
        assert_eq!(super::super::effective_video_kbps(&preset, &meta), 1000);

        // A higher-than-ceiling frame rate caps at the ceiling: reference.
        let meta = SourceMetadata {
            resolution: Some((1920, 1080)),
            frame_rate: Some(60.0),
            video_bitrate_kbps: None,
        };
        assert_eq!(super::super::effective_video_kbps(&preset, &meta), 1000);

        // A 24 fps source below the ceiling gets proportionally fewer bits.
        let meta = SourceMetadata {
            resolution: Some((1920, 1080)),
            frame_rate: Some(24.0),
            video_bitrate_kbps: None,
        };
        assert_eq!(super::super::effective_video_kbps(&preset, &meta), 800);

        // Vertical 1080x1920 at a 480p tier: ~0.32 of the reference pixels.
        let meta = SourceMetadata {
            resolution: Some((1080, 1920)),
            frame_rate: Some(30.0),
            video_bitrate_kbps: None,
        };
        assert_eq!(super::super::effective_video_kbps(&preset, &meta), 316);

        // Ultrawide 2560x1080 at a 720p tier: ~1.33x of the reference pixels.
        let meta = SourceMetadata {
            resolution: Some((2560, 1080)),
            frame_rate: Some(30.0),
            video_bitrate_kbps: None,
        };
        let ultrawide_preset = super::super::config::TranscodePreset {
            name: "720p".to_string(),
            height: 720,
            video_kbps: 2500,
            audio_kbps: 96,
            max_fps: 30,
        };
        assert_eq!(
            super::super::effective_video_kbps(&ultrawide_preset, &meta),
            3333
        );

        // Both dimensions combine: vertical 24 fps at ~0.32 pixels and 0.8
        // frame rate, with the 0.5 frame-rate floor never undercut.
        let meta = SourceMetadata {
            resolution: Some((1080, 1920)),
            frame_rate: Some(24.0),
            video_bitrate_kbps: None,
        };
        assert_eq!(super::super::effective_video_kbps(&preset, &meta), 253);
    }

    #[test]
    fn effective_bitrate_floors_extreme_shapes() {
        use super::super::SourceMetadata;
        // A 1:12 sliver of a frame bottoms out at the pixel-count clamp.
        let preset = preset_480p();
        let meta = SourceMetadata {
            resolution: Some((160, 1920)),
            frame_rate: Some(30.0),
            video_bitrate_kbps: None,
        };
        assert_eq!(super::super::effective_video_kbps(&preset, &meta), 250);

        // A tiny configured bitrate with a full-width frame reaches the
        // absolute 64 kbps floor only after every factor.
        let tiny = super::super::config::TranscodePreset {
            name: "tiny".to_string(),
            height: 480,
            video_kbps: 200,
            audio_kbps: 32,
            max_fps: 30,
        };
        let meta = SourceMetadata {
            resolution: Some((854, 480)),
            frame_rate: Some(24.0),
            video_bitrate_kbps: None,
        };
        // 200 * 1.0 (16:9) * 0.8 (24/30) = 160 stays above the floor...
        assert_eq!(super::super::effective_video_kbps(&tiny, &meta), 160);
        let meta = SourceMetadata {
            resolution: Some((854, 480)),
            frame_rate: Some(6.0),
            video_bitrate_kbps: None,
        };
        // ...while 200 * 0.5 (frame-rate floor) = 100 stays above it too;
        // only a preset below 128 kbps can reach it at 16:9.
        assert_eq!(super::super::effective_video_kbps(&tiny, &meta), 100);
        let sliver = super::super::config::TranscodePreset {
            video_kbps: 100,
            ..tiny
        };
        let meta = SourceMetadata {
            resolution: Some((160, 1920)),
            frame_rate: Some(6.0),
            video_bitrate_kbps: None,
        };
        // 100 * 0.25 (pixel clamp) * 0.5 (fps floor) = 12.5 -> floor 64.
        assert_eq!(super::super::effective_video_kbps(&sliver, &meta), 64);
    }

    #[tokio::test(start_paused = true)]
    async fn metadata_wait_returns_first_onmetadata_and_fills_slots() {
        use synctv_xiu::streamhub::define::FrameDataReceiver;

        let (meta_tx, frames) = tokio::sync::mpsc::unbounded_channel();
        let mut frames = FrameDataReceiver::Unbounded(frames);
        let resolution_slot = Arc::new(Mutex::new(None));
        let frame_rate_slot = Arc::new(Mutex::new(None));
        let video_bitrate_slot = Arc::new(Mutex::new(None));
        let cancel = CancellationToken::new();

        // One video tag before the metadata must be buffered, not lost.
        meta_tx
            .send(FrameData::Video {
                timestamp: 40,
                data: bytes::Bytes::from_static(&[0x17, 0x01]),
            })
            .expect("send");
        let metadata_body = metadata_onbody(1920.0, 1080.0, Some(60.0), Some(4500.0));
        meta_tx
            .send(FrameData::MetaData {
                timestamp: 0,
                data: metadata_body.clone(),
            })
            .expect("send");

        let (buffered, metadata) = wait_for_source_metadata(
            &mut frames,
            &resolution_slot,
            &frame_rate_slot,
            &video_bitrate_slot,
            std::time::Duration::from_secs(2),
            &cancel,
        )
        .await;

        assert_eq!(
            metadata,
            super::super::SourceMetadata {
                resolution: Some((1920, 1080)),
                frame_rate: Some(60.0),
                video_bitrate_kbps: Some(4500),
            }
        );
        assert_eq!(*resolution_slot.lock().expect("slot"), Some((1920, 1080)));
        assert_eq!(*frame_rate_slot.lock().expect("slot"), Some(60.0));
        assert_eq!(*video_bitrate_slot.lock().expect("slot"), Some(4500));
        assert_eq!(buffered.len(), 2, "video prefix + metadata tag");
    }

    #[tokio::test(start_paused = true)]
    async fn metadata_wait_times_out_and_buffers_frames() {
        use synctv_xiu::streamhub::define::FrameDataReceiver;

        let (meta_tx, frames) = tokio::sync::mpsc::unbounded_channel();
        let mut frames = FrameDataReceiver::Unbounded(frames);
        let resolution_slot = Arc::new(Mutex::new(None));
        let frame_rate_slot = Arc::new(Mutex::new(None));
        let video_bitrate_slot = Arc::new(Mutex::new(None));
        let cancel = CancellationToken::new();

        // Only video tags: no metadata ever arrives. The paused clock
        // fast-forwards through the deadline.
        for timestamp in [0, 40, 80] {
            meta_tx
                .send(FrameData::Video {
                    timestamp,
                    data: bytes::Bytes::from_static(&[0x17, 0x01]),
                })
                .expect("send");
        }

        let (buffered, metadata) = wait_for_source_metadata(
            &mut frames,
            &resolution_slot,
            &frame_rate_slot,
            &video_bitrate_slot,
            std::time::Duration::from_secs(2),
            &cancel,
        )
        .await;

        assert_eq!(metadata, super::super::SourceMetadata::default());
        assert_eq!(buffered.len(), 3, "the deadline buffers every frame");
    }

    /// Serialize a minimal onMetaData body with the given dimensions and
    /// optional frame rate, the way the RTMP message path delivers it.
    fn metadata_onbody(
        width: f64,
        height: f64,
        frame_rate: Option<f64>,
        video_bitrate: Option<f64>,
    ) -> bytes::Bytes {
        use synctv_xiu::flv::amf0::{Amf0ValueType, amf0_writer::Amf0Writer};

        let mut properties = indexmap::IndexMap::new();
        properties.insert("width".to_string(), Amf0ValueType::Number(width));
        properties.insert("height".to_string(), Amf0ValueType::Number(height));
        if let Some(frame_rate) = frame_rate {
            properties.insert("framerate".to_string(), Amf0ValueType::Number(frame_rate));
        }
        if let Some(video_bitrate) = video_bitrate {
            properties.insert(
                "videodatarate".to_string(),
                Amf0ValueType::Number(video_bitrate),
            );
        }
        let mut writer = Amf0Writer::new();
        writer
            .write_string(&"onMetaData".to_string())
            .expect("AMF0 string writes");
        writer
            .write_eacm_array(&properties)
            .expect("AMF0 array writes");
        writer.extract_current_bytes().freeze()
    }

    fn frame_data(frame: &FrameData) -> bytes::Bytes {
        match frame {
            FrameData::Video { data, .. } => data.clone(),
            _ => unreachable!(),
        }
    }
}
