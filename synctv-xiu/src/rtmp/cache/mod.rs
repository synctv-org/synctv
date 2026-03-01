pub mod errors;
pub mod gop;
pub mod metadata;

use {
    self::gop::Gops,
    crate::bytesio::bytes_reader::BytesReader,
    crate::flv::{
        define,
        flv_tag_header::{AudioTagHeader, VideoTagHeader},
        mpeg4_aac::Mpeg4AacProcessor,
        mpeg4_avc::Mpeg4AvcProcessor,
        Unmarshal,
    },
    crate::streamhub::define::{FrameData, StatisticData, StatisticDataSender},
    bytes::BytesMut,
    errors::CacheError,
    gop::Gop,
    parking_lot::RwLock,
    std::collections::VecDeque,
};

/// Video sequence header cache with timestamp.
/// Updated infrequently (only on AVC sequence header).
#[derive(Default)]
pub struct VideoSeqCache {
    pub data: BytesMut,
    pub timestamp: u32,
}

/// Audio sequence header cache with timestamp.
/// Updated infrequently (only on AAC sequence header).
#[derive(Default)]
pub struct AudioSeqCache {
    pub data: BytesMut,
    pub timestamp: u32,
}

/// Metadata cache with timestamp.
/// Updated infrequently (only on metadata frames).
#[derive(Default)]
pub struct MetadataCache {
    pub data: metadata::MetaData,
    pub timestamp: u32,
}

/// Split cache structure with independent locks for video/audio/metadata/gops.
///
/// This design reduces lock contention under high concurrency:
/// - Video frames only need `video_seq` + `gops` locks
/// - Audio frames only need `audio_seq` + `gops` locks
/// - Metadata only needs `metadata` lock
/// - Readers can access different components concurrently
///
/// The GOP cache uses a single write lock because frames must be saved in order,
/// but reads can happen concurrently with other component reads.
pub struct SplitCache {
    /// Video sequence header (infrequent updates)
    video_seq: RwLock<VideoSeqCache>,
    /// Audio sequence header (infrequent updates)
    audio_seq: RwLock<AudioSeqCache>,
    /// Metadata (infrequent updates)
    metadata: RwLock<MetadataCache>,
    /// GOP cache (frequent updates from both audio and video)
    gops: RwLock<Gops>,
    /// Statistics sender (read-only after init)
    statistic_data_sender: Option<StatisticDataSender>,
}

impl SplitCache {
    /// Create a new split cache with the given GOP count and optional per-stream memory limit.
    #[must_use]
    pub fn new(
        gop_num: usize,
        max_total_bytes: Option<usize>,
        statistic_data_sender: Option<StatisticDataSender>,
    ) -> Self {
        Self {
            video_seq: RwLock::new(VideoSeqCache::default()),
            audio_seq: RwLock::new(AudioSeqCache::default()),
            metadata: RwLock::new(MetadataCache::default()),
            gops: RwLock::new(Gops::new(gop_num, max_total_bytes)),
            statistic_data_sender,
        }
    }

    /// Save metadata frame (low frequency operation).
    pub fn save_metadata(&self, chunk_body: &BytesMut, timestamp: u32) {
        let mut meta = self.metadata.write();
        meta.data.save(chunk_body);
        meta.timestamp = timestamp;
    }

    /// Get metadata as FrameData (read-only operation).
    #[must_use]
    pub fn get_metadata(&self) -> Option<FrameData> {
        let meta = self.metadata.read();
        let data = meta.data.get_chunk_body();
        if data.is_empty() {
            None
        } else {
            Some(FrameData::MetaData {
                timestamp: meta.timestamp,
                data: data.freeze(),
            })
        }
    }

    /// Save audio data (high frequency operation).
    /// Uses separate locks for audio_seq and gops to minimize contention.
    pub fn save_audio_data(&self, chunk_body: &BytesMut, timestamp: u32) -> Result<(), CacheError> {
        // Save to GOP cache first (most frequent operation)
        let channel_data = FrameData::Audio {
            timestamp,
            data: bytes::Bytes::copy_from_slice(chunk_body),
        };
        self.gops.write().save_frame_data(channel_data, false);

        // Parse header and check for sequence header (infrequent)
        let mut reader = BytesReader::new(chunk_body.clone());
        let tag_header = AudioTagHeader::unmarshal(&mut reader)?;
        let remain_bytes = reader.extract_remaining_bytes();

        // Update audio sequence header if this is an AAC config
        if remain_bytes.len() >= 2
            && tag_header.sound_format == define::SoundFormat::AAC as u8
            && tag_header.aac_packet_type == define::aac_packet_type::AAC_SEQHDR
        {
            // Only acquire write lock for sequence header updates
            let mut audio_seq = self.audio_seq.write();
            audio_seq.data = chunk_body.clone();
            audio_seq.timestamp = timestamp;
            drop(audio_seq);

            // Send codec statistics (non-blocking)
            if let Some(sender) = &self.statistic_data_sender {
                let mut aac_processor = Mpeg4AacProcessor::default();
                let aac = aac_processor
                    .extend_data(&remain_bytes)?
                    .audio_specific_config_load()?;

                let statistic_audio_codec = StatisticData::AudioCodec {
                    sound_format: define::SoundFormat::AAC,
                    profile: define::u8_2_aac_profile(aac.mpeg4_aac.object_type),
                    samplerate: aac.mpeg4_aac.sampling_frequency,
                    channels: aac.mpeg4_aac.channels,
                };
                if let Err(err) = sender.try_send(statistic_audio_codec) {
                    tracing::error!("send statistic_data err: {err}");
                }
            }
        }

        // Send frame statistics (non-blocking)
        if let Some(sender) = &self.statistic_data_sender {
            let statistic_audio_data = StatisticData::Audio {
                uuid: None,
                data_size: chunk_body.len(),
                aac_packet_type: tag_header.aac_packet_type,
                duration: 0,
            };
            if let Err(err) = sender.try_send(statistic_audio_data) {
                tracing::error!("send statistic_data err: {err}");
            }
        }

        Ok(())
    }

    /// Get audio sequence header as FrameData (read-only operation).
    #[must_use]
    pub fn get_audio_seq(&self) -> Option<FrameData> {
        let audio_seq = self.audio_seq.read();
        if !audio_seq.data.is_empty() {
            return Some(FrameData::Audio {
                timestamp: audio_seq.timestamp,
                data: bytes::Bytes::copy_from_slice(&audio_seq.data),
            });
        }
        None
    }

    /// Get video sequence header as FrameData (read-only operation).
    #[must_use]
    pub fn get_video_seq(&self) -> Option<FrameData> {
        let video_seq = self.video_seq.read();
        if !video_seq.data.is_empty() {
            return Some(FrameData::Video {
                timestamp: video_seq.timestamp,
                data: bytes::Bytes::copy_from_slice(&video_seq.data),
            });
        }
        None
    }

    /// Save video data (high frequency operation).
    /// Uses separate locks for video_seq and gops to minimize contention.
    pub fn save_video_data(&self, chunk_body: &BytesMut, timestamp: u32) -> Result<(), CacheError> {
        // Parse header first (before acquiring GOP lock)
        let mut reader = BytesReader::new(chunk_body.clone());
        let tag_header = VideoTagHeader::unmarshal(&mut reader)?;
        let is_key_frame = tag_header.frame_type == define::frame_type::KEY_FRAME;

        // Save to GOP cache (most frequent operation)
        let channel_data = FrameData::Video {
            timestamp,
            data: bytes::Bytes::copy_from_slice(chunk_body),
        };
        self.gops
            .write()
            .save_frame_data(channel_data, is_key_frame);

        // Update video sequence header if this is an AVC config (infrequent)
        if is_key_frame && tag_header.avc_packet_type == define::avc_packet_type::AVC_SEQHDR {
            // Only acquire write lock for sequence header updates
            let mut video_seq = self.video_seq.write();
            video_seq.data = chunk_body.clone();
            video_seq.timestamp = timestamp;
            drop(video_seq);

            // Send codec statistics (non-blocking)
            if let Some(sender) = &self.statistic_data_sender {
                let mut avc_processor = Mpeg4AvcProcessor::default();
                avc_processor.decoder_configuration_record_load(&mut reader)?;

                let statistic_video_codec = StatisticData::VideoCodec {
                    codec: define::AvcCodecId::H264,
                    profile: define::u8_2_avc_profile(avc_processor.mpeg4_avc.profile),
                    level: define::u8_2_avc_level(avc_processor.mpeg4_avc.level),
                    width: avc_processor.mpeg4_avc.width,
                    height: avc_processor.mpeg4_avc.height,
                };
                if let Err(err) = sender.try_send(statistic_video_codec) {
                    tracing::error!("send statistic_data err: {err}");
                }
            }
        }

        // Send frame statistics (non-blocking)
        if let Some(sender) = &self.statistic_data_sender {
            let statistic_video_data = StatisticData::Video {
                uuid: None,
                data_size: chunk_body.len(),
                frame_count: 1,
                is_key_frame: Some(is_key_frame),
                duration: 0,
            };
            if let Err(err) = sender.try_send(statistic_video_data) {
                tracing::error!("send statistic_data err: {err}");
            }
        }

        Ok(())
    }

    /// Get GOPs data for sending prior data to subscribers.
    #[must_use]
    pub fn get_gops_data(&self) -> Option<VecDeque<Gop>> {
        let mut gops = self.gops.write();
        if gops.is_enabled() {
            // Return a clone of the GOPs data for zero-contention sending
            Some(gops.get_gops().clone())
        } else {
            None
        }
    }
}
