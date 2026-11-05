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
use tokio::sync::{mpsc, Semaphore};
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

        // Destructure before spawning: a spawn failure must hand the input
        // back to the scheduler intact for the next worker.
        let TranscodeInput {
            mut frames,
            source_resolution,
        } = input;
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(build_ffmpeg_args(&job))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return Err((
                    TranscodeError::Failed(format!("failed to spawn ffmpeg: {error}")),
                    TranscodeInput {
                        frames,
                        source_resolution,
                    },
                ));
            }
        };

        let mut stdin = child.stdin.take().expect("ffmpeg stdin piped");
        let mut stdout = child.stdout.take().expect("ffmpeg stdout piped");
        let mut stderr = child.stderr.take().expect("ffmpeg stderr piped");

        let cancel = CancellationToken::new();
        let (output_tx, output_rx) = mpsc::channel::<FrameData>(OUTPUT_CHANNEL_CAPACITY);
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let stderr_tail = Arc::new(Mutex::new(Vec::new()));

        // stdin pump: FLV-mux the source frames into ffmpeg.
        let stdin_cancel = cancel.clone();
        tokio::spawn(async move {
            if stdin.write_all(FLV_HEADER).await.is_err() {
                return;
            }
            loop {
                let frame = tokio::select! {
                    () = stdin_cancel.cancelled() => break,
                    frame = frames.recv() => match frame {
                        Some(frame) => frame,
                        None => break,
                    },
                };
                match &frame {
                    FrameData::MetaData { data, .. } => {
                        if let Some(resolution) = super::parse_metadata_resolution(data) {
                            let mut slot =
                                source_resolution.lock().expect("resolution slot poisoned");
                            if slot.is_none() {
                                *slot = Some(resolution);
                            }
                        }
                    }
                    FrameData::MediaInfo { .. } => continue,
                    FrameData::Video { .. } | FrameData::Audio { .. } => {}
                }
                if write_flv_tag(&mut stdin, &frame).await.is_err() {
                    break;
                }
            }
            // Closing stdin lets ffmpeg finish its output and exit cleanly.
            let _ = stdin.shutdown().await;
        });

        // stdout pump: demux ffmpeg's FLV output back into frames.
        let stdout_cancel = cancel.clone();
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
                    if output_tx.send(frame).await.is_err() {
                        // Consumer gone; the publisher side ends the job.
                        return;
                    }
                }
            }
        });

        // stderr: keep a bounded tail for failure diagnostics and surface
        // complete lines live, so an input ffmpeg cannot decode is visible
        // while the job runs instead of only after it exits.
        let stderr_cancel = cancel.clone();
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
        let watcher_cancel = cancel.clone();
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

        Ok((
            TranscodeJobHandle {
                done: done_rx,
                cancel: cancel.clone(),
            },
            output_rx,
        ))
    }
}

/// ffmpeg invocation for one rung: FLV in on stdin, scaled/re-encoded FLV
/// out on stdout.
fn build_ffmpeg_args(job: &TranscodeJob) -> Vec<String> {
    let preset = &job.preset;
    let height = preset.height;
    let fps = preset.max_fps;
    let video_kbps = preset.video_kbps;
    let max_rate = video_kbps + video_kbps / 4;
    let buf_size = video_kbps * 2;
    let gop = (fps * 2).max(2);

    [
        "-hide_banner",
        "-loglevel",
        "warning",
        // Input: the source stream's FLV frames on stdin.
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
        "-vf",
        &format!("scale=w=-2:h='min({height},ih)':flags=bicubic,fps=fps={fps}"),
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-tune",
        "zerolatency",
        "-pix_fmt",
        "yuv420p",
        "-profile:v",
        "main",
        "-b:v",
        &format!("{video_kbps}k"),
        "-maxrate",
        &format!("{max_rate}k"),
        "-bufsize",
        &format!("{buf_size}k"),
        // Fixed 2-second GOP so HLS segments cut cleanly on keyframes.
        "-g",
        &gop.to_string(),
        "-keyint_min",
        &fps.to_string(),
        "-sc_threshold",
        "0",
        "-c:a",
        "aac",
        "-b:a",
        &format!("{}k", preset.audio_kbps),
        "-ar",
        "44100",
        "-ac",
        "2",
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
        let args = build_ffmpeg_args(&job);
        let joined = args.join(" ");
        assert!(joined.contains("min(480,ih)"), "{joined}");
        assert!(joined.contains("fps=fps=30"), "{joined}");
        assert!(joined.contains("-g 60"), "{joined}");
        assert!(joined.contains("-b:v 1000k"), "{joined}");
        assert!(joined.contains("-b:a 64k"), "{joined}");
        assert!(joined.ends_with("pipe:1"));
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

    fn frame_data(frame: &FrameData) -> bytes::Bytes {
        match frame {
            FrameData::Video { data, .. } => data.clone(),
            _ => unreachable!(),
        }
    }
}
