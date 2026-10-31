use {
    super::errors::{MpegAacError, MpegErrorValue},
    crate::bytesio::{bytes_reader::BytesReader, bytes_writer::BytesWriter},
    bytes::BytesMut,
};

const AAC_FREQUENCE_SIZE: usize = 13;
const AAC_FREQUENCE: [u32; AAC_FREQUENCE_SIZE] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];
const ADTS_HEADER_LEN: usize = 7;
const ADTS_SYNCWORD_HIGH: u8 = 0xFF;
const ADTS_SYNCWORD_LOW_MASK: u8 = 0xF0;
const ADTS_PROTECTION_ABSENT: u8 = 0x01;
const ADTS_BUFFER_FULLNESS_PARTIAL: u8 = 0x1F;
const ADTS_FRAME_COUNT_ONE: u8 = 0xFC;

fn usize_to_u32(value: usize) -> Result<u32, MpegAacError> {
    u32::try_from(value).map_err(|_| MpegAacError {
        value: MpegErrorValue::IntegerRange {
            value: value as u128,
            target: "u32",
        },
    })
}

fn u32_to_u8(value: u32) -> Result<u8, MpegAacError> {
    u8::try_from(value).map_err(|_| MpegAacError {
        value: MpegErrorValue::IntegerRange {
            value: u128::from(value),
            target: "u8",
        },
    })
}

#[derive(Debug, Clone, Default)]
pub struct Mpeg4Aac {
    pub object_type: u8,
    pub sampling_frequency_index: u8,
    pub channel_configuration: u8,

    pub sampling_frequency: u32,
    pub channels: u8,
}

impl Mpeg4Aac {
    pub fn new(
        object_type: u8,
        sampling_frequency: u32,
        channel_configuration: u8,
    ) -> Result<Self, MpegAacError> {
        let sampling_frequency_index = match sampling_frequency {
            96000 => 0,
            88200 => 1,
            64000 => 2,
            48000 => 3,
            44100 => 4,
            32000 => 5,
            24000 => 6,
            22050 => 7,
            16000 => 8,
            12000 => 9,
            11025 => 10,
            8000 => 11,
            7350 => 12,
            _ => {
                return Err(MpegAacError {
                    value: MpegErrorValue::NotSupportedSamplingFrequency,
                });
            }
        };

        Ok(Self {
            object_type,
            sampling_frequency_index,
            channel_configuration,
            sampling_frequency,
            ..Default::default()
        })
    }
}

pub struct Mpeg4AacProcessor {
    pub bytes_reader: BytesReader,
    pub bytes_writer: BytesWriter,
    pub mpeg4_aac: Mpeg4Aac,
}

impl Default for Mpeg4AacProcessor {
    fn default() -> Self {
        Self::new()
    }
}
impl Mpeg4AacProcessor {
    #[must_use]
    pub fn new() -> Self {
        Self {
            bytes_reader: BytesReader::new(BytesMut::new()),
            bytes_writer: BytesWriter::new(),
            mpeg4_aac: Mpeg4Aac::default(),
        }
    }

    pub fn extend_data(&mut self, data: &BytesMut) -> Result<&mut Self, MpegAacError> {
        self.bytes_reader.extend_from_slice(&data[..])?;
        Ok(self)
    }

    pub fn audio_specific_config_load(&mut self) -> Result<&mut Self, MpegAacError> {
        //11 88 56 E5
        let byte_0 = self.bytes_reader.read_u8()?;
        self.mpeg4_aac.object_type = (byte_0 >> 3) & 0x1F;

        // 0 is not a legal MPEG-4 Audio Object Type; reject it before any
        // derived arithmetic (adts_save computes profile - 1).
        if self.mpeg4_aac.object_type == 0 {
            return Err(MpegAacError {
                value: MpegErrorValue::IntegerRange {
                    value: u128::from(self.mpeg4_aac.object_type),
                    target: "audio_object_type (must be >= 1)",
                },
            });
        }

        let byte_1 = self.bytes_reader.read_u8()?;
        self.mpeg4_aac.sampling_frequency_index = ((byte_0 & 0x07) << 1) | ((byte_1 >> 7) & 0x01);
        self.mpeg4_aac.channel_configuration = (byte_1 >> 3) & 0x0F;
        self.mpeg4_aac.channels = self.mpeg4_aac.channel_configuration;

        // Validate sampling_frequency_index to prevent array out of bounds
        let freq_index = usize::from(self.mpeg4_aac.sampling_frequency_index);
        if freq_index >= AAC_FREQUENCE_SIZE {
            return Err(MpegAacError {
                value: MpegErrorValue::NotSupportedSamplingFrequency,
            });
        }
        self.mpeg4_aac.sampling_frequency = AAC_FREQUENCE[freq_index];

        self.bytes_reader.extract_remaining_bytes();

        Ok(self)
    }

    pub(crate) fn adts_save(&mut self) -> Result<(), MpegAacError> {
        let mpeg_version_id = 0u8;
        let len = usize_to_u32(self.bytes_reader.len() + ADTS_HEADER_LEN)?;
        let syncword_and_flags =
            ADTS_SYNCWORD_LOW_MASK | (mpeg_version_id << 3) | ADTS_PROTECTION_ABSENT;

        self.bytes_writer.write_u8(ADTS_SYNCWORD_HIGH)?;
        self.bytes_writer.write_u8(syncword_and_flags)?;

        let profile = self.mpeg4_aac.object_type;
        let sampling_frequency_index = self.mpeg4_aac.sampling_frequency_index;
        let channel_configuration = self.mpeg4_aac.channel_configuration;
        // Underflow-proof profile mapping: fail closed when no valid sequence
        // header established a legal Audio Object Type.
        let adts_profile = profile.checked_sub(1).ok_or_else(|| MpegAacError {
            value: MpegErrorValue::IntegerRange {
                value: u128::from(profile),
                target: "audio_object_type (must be >= 1)",
            },
        })?;
        let profile_frequency_channels = {
            (adts_profile << 6)
                | ((sampling_frequency_index & 0x0F) << 2)
                | ((channel_configuration >> 2) & 0x01)
        };
        let channels_and_frame_length =
            ((channel_configuration & 0x03) << 6) | (u32_to_u8(len >> 11)? & 0x03);
        let frame_length_middle = u32_to_u8(len >> 3)?;
        let frame_length_and_fullness = ((len & 0x07) as u8) << 5 | ADTS_BUFFER_FULLNESS_PARTIAL;

        self.bytes_writer.write_u8(profile_frequency_channels)?;
        self.bytes_writer.write_u8(channels_and_frame_length)?;
        self.bytes_writer.write_u8(frame_length_middle)?;
        self.bytes_writer.write_u8(frame_length_and_fullness)?;
        self.bytes_writer.write_u8(ADTS_FRAME_COUNT_ONE)?;

        self.bytes_writer
            .write(&self.bytes_reader.extract_remaining_bytes()[..])?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Mpeg4AacProcessor;
    use bytes::BytesMut;

    #[test]
    fn rejects_zero_audio_object_type_in_sequence_header() {
        // object_type = (byte_0 >> 3) & 0x1F; 0x02 puts AOT=0, freq index low
        // bits come from byte_1. This crafted ASC must fail closed.
        let mut processor = Mpeg4AacProcessor::new();
        processor
            .extend_data(&BytesMut::from(&[0x02, 0x10][..]))
            .unwrap();
        let error = processor.audio_specific_config_load().err().unwrap();
        assert!(
            error.value.to_string().contains("audio_object_type"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn adts_save_fails_closed_without_sequence_header() {
        // Default state (object_type = 0) previously underflowed profile - 1:
        // panicking in debug builds and emitting an invalid profile byte in
        // release builds. It must now return an error.
        let mut processor = Mpeg4AacProcessor::new();
        processor
            .extend_data(&BytesMut::from(&[0x12, 0x34][..]))
            .unwrap();
        let error = processor.adts_save().err().unwrap();
        assert!(
            error.value.to_string().contains("audio_object_type"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn valid_sequence_header_then_adts_save_writes_profile() {
        // 0x12 0x10: AOT=2 (AAC LC), freq index 4 (44100), channels 2.
        let mut processor = Mpeg4AacProcessor::new();
        processor
            .extend_data(&BytesMut::from(&[0x12, 0x10][..]))
            .unwrap();
        processor.audio_specific_config_load().unwrap();
        processor
            .extend_data(&BytesMut::from(&[0x00, 0x01][..]))
            .unwrap();
        processor.adts_save().unwrap();
        let out = processor.bytes_writer.extract_current_bytes();
        assert_eq!(out[0], 0xFF);
        // profile bits: (2 - 1) << 6 | (4 << 2) | (2 >> 2 & 1) = 0x50
        assert_eq!(out[2], 0x50);
    }
}
