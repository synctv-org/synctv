use bytes::{BufMut as _, Bytes, BytesMut};
use fdk_aac::enc::{AudioObjectType, BitRate, ChannelMode, Encoder as AacEncoder, EncoderParams};
use opus::{Channels, Decoder as OpusDecoder};
use rtc::{
    rtp::{codec::h264::H264Packet, packet::Packet, packetizer::Depacketizer},
    rtp_transceiver::rtp_sender::RtpCodecKind,
};
use std::collections::VecDeque;

use crate::{
    flv::define::{self, aac_packet_type, avc_packet_type},
    streamhub::define::FrameData,
};

const H264_CLOCK_RATE: u32 = 90_000;
const OPUS_CLOCK_RATE: u32 = 48_000;
const AAC_SAMPLES_PER_FRAME: usize = 1_024;
const AAC_CHANNELS: usize = 2;
const MAX_OPUS_SAMPLES_PER_CHANNEL: usize = 5_760;
const MAX_AAC_OUTPUT_BYTES: usize = 16 * 1024;
const H264_NAL_TYPE_MASK: u8 = 0x1f;
const H264_NAL_IDR: u8 = 5;
const H264_NAL_SPS: u8 = 7;
const H264_NAL_PPS: u8 = 8;

#[derive(Debug, thiserror::Error)]
pub(crate) enum MediaConversionError {
    #[error("unsupported WebRTC codec: {0}")]
    UnsupportedCodec(String),
    #[error("invalid H.264 RTP payload: {0}")]
    InvalidH264(String),
    #[error("invalid Opus RTP payload: {0}")]
    InvalidOpus(String),
    #[error("AAC encoder failed: {0}")]
    AacEncoder(String),
    #[error("H.264 parameter set is too large")]
    ParameterSetTooLarge,
}

pub(crate) enum TrackFrameEncoder {
    Video(VideoFrameEncoder),
    Audio(AudioFrameEncoder),
}

impl TrackFrameEncoder {
    pub(crate) fn new(
        kind: RtpCodecKind,
        mime_type: &str,
        channels: u16,
    ) -> Result<Self, MediaConversionError> {
        match (kind, mime_type.to_ascii_lowercase().as_str()) {
            (RtpCodecKind::Video, "video/h264") => Ok(Self::Video(VideoFrameEncoder::default())),
            (RtpCodecKind::Audio, "audio/opus") => {
                AudioFrameEncoder::new(channels).map(Self::Audio)
            }
            _ => Err(MediaConversionError::UnsupportedCodec(
                mime_type.to_string(),
            )),
        }
    }

    pub(crate) fn push(&mut self, packet: &Packet) -> Result<Vec<FrameData>, MediaConversionError> {
        match self {
            Self::Video(encoder) => encoder.push(packet),
            Self::Audio(encoder) => encoder.push(packet),
        }
    }
}

fn timestamp_millis(base: u32, timestamp: u32, clock_rate: u32) -> u32 {
    let elapsed = u64::from(timestamp.wrapping_sub(base));
    let millis = elapsed.saturating_mul(1_000) / u64::from(clock_rate);
    u32::try_from(millis).unwrap_or(u32::MAX)
}

fn video_tag_body(keyframe: bool, packet_type: u8, payload: &[u8]) -> Bytes {
    let mut body = BytesMut::with_capacity(5 + payload.len());
    let frame_type = if keyframe {
        define::frame_type::KEY_FRAME
    } else {
        define::frame_type::INTER_FRAME
    };
    body.extend_from_slice(&[
        (frame_type << 4) | define::AvcCodecId::H264 as u8,
        packet_type,
        0,
        0,
        0,
    ]);
    body.extend_from_slice(payload);
    body.freeze()
}

fn audio_tag_body(packet_type: u8, payload: &[u8]) -> Bytes {
    let flags = ((define::SoundFormat::AAC as u8) << 4) | (3 << 2) | (1 << 1) | 1;
    let mut body = BytesMut::with_capacity(2 + payload.len());
    body.extend_from_slice(&[flags, packet_type]);
    body.extend_from_slice(payload);
    body.freeze()
}

fn split_length_prefixed_nalus(mut data: Bytes) -> Result<Vec<Bytes>, MediaConversionError> {
    let mut nalus = Vec::new();
    while !data.is_empty() {
        if data.len() < 4 {
            return Err(MediaConversionError::InvalidH264(
                "truncated AVC NAL length".to_string(),
            ));
        }
        let length = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let length = usize::try_from(length).map_err(|_| {
            MediaConversionError::InvalidH264("NAL length exceeds usize".to_string())
        })?;
        data = data.slice(4..);
        if length == 0 || length > data.len() {
            return Err(MediaConversionError::InvalidH264(
                "invalid AVC NAL length".to_string(),
            ));
        }
        nalus.push(data.slice(..length));
        data = data.slice(length..);
    }
    Ok(nalus)
}

fn avc_sequence_header(sps: &[u8], pps: &[u8]) -> Result<Bytes, MediaConversionError> {
    if sps.len() < 4 {
        return Err(MediaConversionError::InvalidH264(
            "SPS is too short".to_string(),
        ));
    }
    let sps_len =
        u16::try_from(sps.len()).map_err(|_| MediaConversionError::ParameterSetTooLarge)?;
    let pps_len =
        u16::try_from(pps.len()).map_err(|_| MediaConversionError::ParameterSetTooLarge)?;
    let mut config = BytesMut::with_capacity(11 + sps.len() + pps.len());
    config.extend_from_slice(&[1, sps[1], sps[2], sps[3], 0xff, 0xe1]);
    config.put_u16(sps_len);
    config.extend_from_slice(sps);
    config.put_u8(1);
    config.put_u16(pps_len);
    config.extend_from_slice(pps);
    Ok(video_tag_body(true, avc_packet_type::AVC_SEQHDR, &config))
}

fn avc_access_unit(nalus: &[Bytes]) -> Result<Bytes, MediaConversionError> {
    let payload_len = nalus.iter().try_fold(0_usize, |total, nalu| {
        u32::try_from(nalu.len())
            .map_err(|_| MediaConversionError::InvalidH264("NAL is too large".to_string()))?;
        total.checked_add(4 + nalu.len()).ok_or_else(|| {
            MediaConversionError::InvalidH264("access unit is too large".to_string())
        })
    })?;
    let mut payload = BytesMut::with_capacity(payload_len);
    for nalu in nalus {
        payload.put_u32(
            u32::try_from(nalu.len())
                .map_err(|_| MediaConversionError::InvalidH264("NAL is too large".to_string()))?,
        );
        payload.extend_from_slice(nalu);
    }
    let keyframe = nalus.iter().any(|nalu| {
        nalu.first()
            .is_some_and(|value| value & H264_NAL_TYPE_MASK == H264_NAL_IDR)
    });
    Ok(video_tag_body(
        keyframe,
        avc_packet_type::AVC_NALU,
        &payload,
    ))
}

#[derive(Default)]
pub(crate) struct VideoFrameEncoder {
    depacketizer: H264Packet,
    access_unit: Vec<Bytes>,
    access_unit_timestamp: Option<u32>,
    base_timestamp: Option<u32>,
    sps: Option<Bytes>,
    pps: Option<Bytes>,
    emitted_parameter_sets: Option<(Bytes, Bytes)>,
}

impl VideoFrameEncoder {
    fn flush_access_unit(&mut self) -> Result<Option<FrameData>, MediaConversionError> {
        let Some(timestamp) = self.access_unit_timestamp.take() else {
            return Ok(None);
        };
        if self.access_unit.is_empty() {
            return Ok(None);
        }
        let data = avc_access_unit(&self.access_unit)?;
        self.access_unit.clear();
        let base = *self.base_timestamp.get_or_insert(timestamp);
        Ok(Some(FrameData::Video {
            timestamp: timestamp_millis(base, timestamp, H264_CLOCK_RATE),
            data,
        }))
    }

    fn maybe_sequence_header(&mut self) -> Result<Option<FrameData>, MediaConversionError> {
        let (Some(sps), Some(pps)) = (&self.sps, &self.pps) else {
            return Ok(None);
        };
        if self
            .emitted_parameter_sets
            .as_ref()
            .is_some_and(|(old_sps, old_pps)| old_sps == sps && old_pps == pps)
        {
            return Ok(None);
        }
        let data = avc_sequence_header(sps, pps)?;
        self.emitted_parameter_sets = Some((sps.clone(), pps.clone()));
        Ok(Some(FrameData::Video { timestamp: 0, data }))
    }

    pub(crate) fn push(&mut self, packet: &Packet) -> Result<Vec<FrameData>, MediaConversionError> {
        self.depacketizer.is_avc = true;
        let mut frames = Vec::new();
        if self
            .access_unit_timestamp
            .is_some_and(|timestamp| timestamp != packet.header.timestamp)
        {
            if let Some(frame) = self.flush_access_unit()? {
                frames.push(frame);
            }
        }
        self.access_unit_timestamp = Some(packet.header.timestamp);

        let depacketized = self
            .depacketizer
            .depacketize(&packet.payload)
            .map_err(|error| MediaConversionError::InvalidH264(error.to_string()))?;
        for nalu in split_length_prefixed_nalus(depacketized)? {
            let Some(first) = nalu.first() else {
                continue;
            };
            match first & H264_NAL_TYPE_MASK {
                H264_NAL_SPS => self.sps = Some(nalu),
                H264_NAL_PPS => self.pps = Some(nalu),
                _ => self.access_unit.push(nalu),
            }
        }
        if let Some(sequence) = self.maybe_sequence_header()? {
            frames.push(sequence);
        }
        if packet.header.marker {
            if let Some(frame) = self.flush_access_unit()? {
                frames.push(frame);
            }
        }
        Ok(frames)
    }
}

pub(crate) struct AudioFrameEncoder {
    decoder: OpusDecoder,
    encoder: AacEncoder,
    input_channels: usize,
    pcm: Vec<i16>,
    base_timestamp: Option<u32>,
    pcm_start_timestamp: Option<u32>,
    next_packet_timestamp: Option<u32>,
    pending_frame_timestamps: VecDeque<u32>,
    sequence_header_sent: bool,
}

impl AudioFrameEncoder {
    fn new(channels: u16) -> Result<Self, MediaConversionError> {
        let (opus_channels, input_channels) = if channels == 1 {
            (Channels::Mono, 1)
        } else {
            (Channels::Stereo, 2)
        };
        let decoder = OpusDecoder::new(OPUS_CLOCK_RATE, opus_channels)
            .map_err(|error| MediaConversionError::InvalidOpus(error.to_string()))?;
        let encoder = AacEncoder::new(EncoderParams {
            bit_rate: BitRate::VbrMedium,
            sample_rate: OPUS_CLOCK_RATE,
            transport: fdk_aac::enc::Transport::Raw,
            channels: ChannelMode::Stereo,
            audio_object_type: AudioObjectType::Mpeg4LowComplexity,
        })
        .map_err(|error| MediaConversionError::AacEncoder(format!("{error:?}")))?;
        Ok(Self {
            decoder,
            encoder,
            input_channels,
            pcm: Vec::with_capacity(AAC_SAMPLES_PER_FRAME * AAC_CHANNELS * 2),
            base_timestamp: None,
            pcm_start_timestamp: None,
            next_packet_timestamp: None,
            pending_frame_timestamps: VecDeque::new(),
            sequence_header_sent: false,
        })
    }

    fn decode(&mut self, payload: &[u8]) -> Result<u32, MediaConversionError> {
        let mut decoded = vec![0_i16; MAX_OPUS_SAMPLES_PER_CHANNEL * self.input_channels];
        let samples_per_channel = self
            .decoder
            .decode(payload, &mut decoded, false)
            .map_err(|error| MediaConversionError::InvalidOpus(error.to_string()))?;
        decoded.truncate(samples_per_channel * self.input_channels);
        if self.input_channels == 1 {
            self.pcm
                .extend(decoded.into_iter().flat_map(|sample| [sample, sample]));
        } else {
            self.pcm.extend(decoded);
        }
        u32::try_from(samples_per_channel).map_err(|_| {
            MediaConversionError::InvalidOpus("decoded sample count exceeds u32".to_string())
        })
    }

    pub(crate) fn push(&mut self, packet: &Packet) -> Result<Vec<FrameData>, MediaConversionError> {
        let mut frames = Vec::new();
        if !self.sequence_header_sent {
            frames.push(FrameData::Audio {
                timestamp: 0,
                data: audio_tag_body(aac_packet_type::AAC_SEQHDR, &[0x11, 0x90]),
            });
            self.sequence_header_sent = true;
        }
        let packet_timestamp = packet.header.timestamp;
        self.base_timestamp.get_or_insert(packet_timestamp);
        if self
            .next_packet_timestamp
            .is_some_and(|expected| expected != packet_timestamp)
        {
            self.pcm.clear();
            self.pcm_start_timestamp = Some(packet_timestamp);
        } else if self.pcm_start_timestamp.is_none() {
            self.pcm_start_timestamp = Some(packet_timestamp);
        }
        let decoded_samples = self.decode(&packet.payload)?;
        self.next_packet_timestamp = Some(packet_timestamp.wrapping_add(decoded_samples));

        let samples_per_frame = AAC_SAMPLES_PER_FRAME * AAC_CHANNELS;
        while self.pcm.len() >= samples_per_frame {
            let frame_timestamp = self.pcm_start_timestamp.ok_or_else(|| {
                MediaConversionError::InvalidOpus(
                    "AAC input is missing its RTP timestamp".to_string(),
                )
            })?;
            let pcm: Vec<_> = self.pcm.drain(..samples_per_frame).collect();
            self.pcm_start_timestamp = Some(frame_timestamp.wrapping_add(
                u32::try_from(AAC_SAMPLES_PER_FRAME).expect("AAC frame sample count fits in u32"),
            ));
            self.pending_frame_timestamps.push_back(frame_timestamp);
            let mut output = vec![0_u8; MAX_AAC_OUTPUT_BYTES];
            let info = self
                .encoder
                .encode(&pcm, &mut output)
                .map_err(|error| MediaConversionError::AacEncoder(format!("{error:?}")))?;
            if info.output_size == 0 {
                continue;
            }
            output.truncate(info.output_size);
            let output_timestamp = self.pending_frame_timestamps.pop_front().ok_or_else(|| {
                MediaConversionError::AacEncoder(
                    "AAC encoder produced output without an input timestamp".to_string(),
                )
            })?;
            let timestamp = timestamp_millis(
                self.base_timestamp.unwrap_or(output_timestamp),
                output_timestamp,
                OPUS_CLOCK_RATE,
            );
            frames.push(FrameData::Audio {
                timestamp,
                data: audio_tag_body(aac_packet_type::AAC_RAW, &output),
            });
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use opus::{Application, Encoder as OpusEncoder};

    use super::*;

    #[test]
    fn creates_avc_sequence_and_access_unit_tags() -> Result<(), MediaConversionError> {
        let sps = [0x67, 0x42, 0x00, 0x1f, 0xe5];
        let pps = [0x68, 0xce, 0x06, 0xe2];
        let sequence = avc_sequence_header(&sps, &pps)?;
        assert_eq!(&sequence[..5], &[0x17, 0, 0, 0, 0]);
        assert_eq!(sequence[5], 1);
        assert_eq!(sequence[6], 0x42);

        let frame = avc_access_unit(&[Bytes::from_static(&[0x65, 1, 2])])?;
        assert_eq!(&frame[..5], &[0x17, 1, 0, 0, 0]);
        assert_eq!(&frame[5..9], &[0, 0, 0, 3]);
        Ok(())
    }

    #[test]
    fn rejects_truncated_length_prefixed_nalus() {
        let error = split_length_prefixed_nalus(Bytes::from_static(&[0, 0, 0, 8, 1]))
            .expect_err("truncated NAL must fail");
        assert!(matches!(error, MediaConversionError::InvalidH264(_)));
    }

    #[test]
    fn rtp_timestamp_wraps_safely() {
        assert_eq!(
            timestamp_millis(u32::MAX - 89_999, 0, H264_CLOCK_RATE),
            1_000
        );
    }

    #[test]
    fn converts_opus_packets_to_aac_frames() -> Result<()> {
        let mut opus_encoder =
            OpusEncoder::new(OPUS_CLOCK_RATE, Channels::Stereo, Application::Audio)?;
        let mut converter = AudioFrameEncoder::new(2)?;
        let pcm = vec![0_i16; 960 * AAC_CHANNELS];
        let mut sequence_header_seen = false;
        let mut raw_frame_seen = false;

        for index in 0_u32..10 {
            let mut payload = vec![0_u8; 4_000];
            let payload_len = opus_encoder.encode(&pcm, &mut payload)?;
            payload.truncate(payload_len);
            let packet = Packet {
                header: rtc::rtp::header::Header {
                    timestamp: index * 960,
                    ..Default::default()
                },
                payload: Bytes::from(payload),
            };
            for frame in converter.push(&packet)? {
                if let FrameData::Audio { data, .. } = frame {
                    sequence_header_seen |=
                        data.get(1).copied() == Some(aac_packet_type::AAC_SEQHDR);
                    raw_frame_seen |= data.get(1).copied() == Some(aac_packet_type::AAC_RAW);
                }
            }
        }

        assert!(sequence_header_seen);
        assert!(raw_frame_seen);
        Ok(())
    }

    #[test]
    fn preserves_opus_rtp_timestamp_gaps_in_aac_output() -> Result<()> {
        let mut opus_encoder =
            OpusEncoder::new(OPUS_CLOCK_RATE, Channels::Stereo, Application::Audio)?;
        let mut converter = AudioFrameEncoder::new(2)?;
        let pcm = vec![0_i16; 960 * AAC_CHANNELS];
        let mut raw_timestamps = Vec::new();

        for timestamp in (0_u32..10)
            .map(|index| index * 960)
            .chain((0_u32..10).map(|index| 48_000 + index * 960))
        {
            let mut payload = vec![0_u8; 4_000];
            let payload_len = opus_encoder.encode(&pcm, &mut payload)?;
            payload.truncate(payload_len);
            let packet = Packet {
                header: rtc::rtp::header::Header {
                    timestamp,
                    ..Default::default()
                },
                payload: Bytes::from(payload),
            };
            for frame in converter.push(&packet)? {
                if let FrameData::Audio {
                    timestamp, data, ..
                } = frame
                {
                    if data.get(1).copied() == Some(aac_packet_type::AAC_RAW) {
                        raw_timestamps.push(timestamp);
                    }
                }
            }
        }

        assert!(raw_timestamps.iter().any(|timestamp| *timestamp >= 1_000));
        assert!(raw_timestamps.windows(2).all(|pair| pair[0] <= pair[1]));
        Ok(())
    }
}
