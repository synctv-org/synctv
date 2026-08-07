use std::sync::Mutex;

use symphonia::core::{
    errors::Error as SymphoniaError,
    formats::{probe::Hint, FormatOptions, FormatReader, TrackType},
    io::{MediaSource, MediaSourceStream},
    meta::MetadataOptions,
    units::{Duration as SymphoniaDuration, TimeBase, Timestamp},
};

use crate::{
    models::FileUploadPolicy,
    service::file_storage::{validation::validate_file_audio_metadata, FileObjectReader},
    Error, Result,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AudioMetadata {
    pub duration_seconds: i32,
    pub bitrate_bps: i32,
    pub sample_rate_hz: Option<i32>,
    pub channels: Option<i32>,
}

pub(crate) async fn validate_audio_object_reader(
    policy: &FileUploadPolicy,
    mime_type: String,
    size_bytes: i64,
    reader: FileObjectReader,
) -> Result<Option<AudioMetadata>> {
    if !mime_type.trim().to_ascii_lowercase().starts_with("audio/") {
        return Ok(None);
    }
    let metadata = probe_audio_metadata_reader(mime_type.clone(), size_bytes, reader).await?;
    validate_file_audio_metadata(
        policy,
        &mime_type,
        Some(metadata.duration_seconds),
        Some(metadata.bitrate_bps),
    )?;
    Ok(Some(metadata))
}

async fn probe_audio_metadata_reader(
    mime_type: String,
    size_bytes: i64,
    reader: FileObjectReader,
) -> Result<AudioMetadata> {
    tokio::task::spawn_blocking(move || {
        let reader = tokio_util::io::SyncIoBridge::new(reader);
        probe_audio_metadata_sync(&mime_type, size_bytes, reader)
    })
    .await
    .map_err(|error| Error::Internal(format!("audio metadata task failed: {error}")))?
}

fn probe_audio_metadata_sync<R>(
    mime_type: &str,
    size_bytes: i64,
    reader: R,
) -> Result<AudioMetadata>
where
    R: std::io::Read + std::io::Seek + Send + 'static,
{
    let media_source = MediaSourceStream::new(
        Box::new(ReaderMediaSource {
            reader: Mutex::new(reader),
            size_bytes,
        }),
        Default::default(),
    );
    let mut hint = Hint::new();
    if let Some(extension) = audio_extension_from_mime_type(mime_type) {
        hint.with_extension(extension);
    }
    let probed = symphonia::default::get_probe()
        .probe(
            &hint,
            media_source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|error| {
            Error::InvalidInput(format!("unsupported or invalid audio data: {error}"))
        })?;
    let mut format = probed;
    let (track_id, sample_rate, time_base, duration, duration_frames, channels) = {
        let track = format
            .first_track_known_codec(TrackType::Audio)
            .ok_or_else(|| Error::InvalidInput("audio track was not found".to_string()))?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(symphonia::core::codecs::CodecParameters::audio)
            .ok_or_else(|| Error::InvalidInput("audio track was not found".to_string()))?;
        (
            track.id,
            params.sample_rate,
            track.time_base,
            track.duration,
            track.num_frames,
            params
                .channels
                .as_ref()
                .map(|channels| i32::try_from(channels.count()).unwrap_or(i32::MAX)),
        )
    };
    let sample_rate = sample_rate.ok_or_else(|| {
        Error::InvalidInput("audio sample rate could not be determined".to_string())
    })?;
    let duration_seconds = duration_seconds(
        duration_frames,
        sample_rate,
        time_base,
        duration,
        track_id,
        &mut format,
    )?;
    let bitrate_bps = i32::try_from(div_ceil_i64(
        size_bytes.checked_mul(8).ok_or_else(|| {
            Error::InvalidInput("audio bitrate exceeds supported limit".to_string())
        })?,
        i64::from(duration_seconds),
    ))
    .map_err(|_| Error::InvalidInput("audio bitrate exceeds supported limit".to_string()))?;
    Ok(AudioMetadata {
        duration_seconds,
        bitrate_bps,
        sample_rate_hz: i32::try_from(sample_rate).ok(),
        channels,
    })
}

struct ReaderMediaSource<R> {
    reader: Mutex<R>,
    size_bytes: i64,
}

impl<R> std::io::Read for ReaderMediaSource<R>
where
    R: std::io::Read,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader
            .lock()
            .map_err(|_| std::io::Error::other("audio reader lock poisoned"))?
            .read(buf)
    }
}

impl<R> std::io::Seek for ReaderMediaSource<R>
where
    R: std::io::Seek,
{
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.reader
            .lock()
            .map_err(|_| std::io::Error::other("audio reader lock poisoned"))?
            .seek(pos)
    }
}

impl<R> MediaSource for ReaderMediaSource<R>
where
    R: std::io::Read + std::io::Seek + Send,
{
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        u64::try_from(self.size_bytes).ok()
    }
}

fn duration_seconds(
    duration_frames: Option<u64>,
    sample_rate: u32,
    time_base: Option<TimeBase>,
    duration: Option<SymphoniaDuration>,
    track_id: u32,
    format: &mut Box<dyn FormatReader>,
) -> Result<i32> {
    if let Some(frames) = duration_frames {
        return positive_duration_seconds(div_ceil_u64(frames, u64::from(sample_rate)));
    }
    let time_base = time_base
        .ok_or_else(|| Error::InvalidInput("audio duration could not be determined".to_string()))?;
    if let Some(duration) = duration {
        return duration_from_timebase(time_base, duration);
    }
    let mut last_tick = None::<Timestamp>;
    loop {
        match format.next_packet() {
            Ok(Some(packet)) => {
                if packet.track_id != track_id {
                    continue;
                }
                last_tick = packet.pts.checked_add(packet.dur).or(Some(packet.pts));
            }
            Ok(None) => break,
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => {}
            Err(error) => {
                return Err(Error::InvalidInput(format!(
                    "audio duration could not be determined: {error}"
                )));
            }
        }
    }
    let ticks = last_tick
        .ok_or_else(|| Error::InvalidInput("audio duration could not be determined".to_string()))?;
    let duration = ticks
        .checked_delta(Timestamp::ZERO)
        .and_then(|delta| u64::try_from(delta.get()).ok())
        .map(SymphoniaDuration::new)
        .ok_or_else(|| Error::InvalidInput("audio duration could not be determined".to_string()))?;
    duration_from_timebase(time_base, duration)
}

fn duration_from_timebase(time_base: TimeBase, duration: SymphoniaDuration) -> Result<i32> {
    let time = time_base
        .calc_time(Timestamp::ZERO.checked_add(duration).ok_or_else(|| {
            Error::InvalidInput("audio duration exceeds supported limit".to_string())
        })?)
        .ok_or_else(|| Error::InvalidInput("audio duration exceeds supported limit".to_string()))?;
    let (seconds, nanos) = time.parts();
    let seconds = u64::try_from(seconds)
        .map_err(|_| Error::InvalidInput("audio duration could not be determined".to_string()))?;
    let seconds = seconds
        .checked_add(u64::from(nanos > 0))
        .ok_or_else(|| Error::InvalidInput("audio duration exceeds supported limit".to_string()))?;
    positive_duration_seconds(seconds)
}

fn positive_duration_seconds(seconds: u64) -> Result<i32> {
    let duration_seconds = i32::try_from(seconds)
        .map_err(|_| Error::InvalidInput("audio duration exceeds supported limit".to_string()))?;
    if duration_seconds <= 0 {
        return Err(Error::InvalidInput(
            "audio duration must be positive".to_string(),
        ));
    }
    Ok(duration_seconds)
}

fn div_ceil_u64(value: u64, divisor: u64) -> u64 {
    if value == 0 {
        0
    } else {
        ((value - 1) / divisor) + 1
    }
}

fn div_ceil_i64(value: i64, divisor: i64) -> i64 {
    if value == 0 {
        0
    } else {
        ((value - 1) / divisor) + 1
    }
}

fn audio_extension_from_mime_type(mime_type: &str) -> Option<&'static str> {
    let mime_type = mime_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match mime_type.as_str() {
        "audio/aac" => Some("aac"),
        "audio/flac" | "audio/x-flac" => Some("flac"),
        "audio/mpeg" | "audio/mp3" => Some("mp3"),
        "audio/mp4" | "audio/x-m4a" => Some("m4a"),
        "audio/ogg" | "application/ogg" => Some("ogg"),
        "audio/wav" | "audio/wave" | "audio/x-wav" => Some("wav"),
        "audio/webm" => Some("webm"),
        _ => None,
    }
}
