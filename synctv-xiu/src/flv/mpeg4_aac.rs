use {
    super::errors::{MpegAacError, MpegErrorValue},
    crate::bytesio::{
        bits_reader::BitsReader, bits_writer::BitsWriter, bytes_reader::BytesReader,
        bytes_writer::BytesWriter,
    },
    bytes::BytesMut,
};

const AAC_FREQUENCE_SIZE: usize = 13;
const AAC_FREQUENCE: [u32; AAC_FREQUENCE_SIZE] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

fn bits_to_u8(value: u64) -> Result<u8, MpegAacError> {
    u8::try_from(value).map_err(|_| MpegAacError {
        value: MpegErrorValue::ShouldNotComeHere,
    })
}

fn bits_to_usize(value: u64) -> Result<usize, MpegAacError> {
    usize::try_from(value).map_err(|_| MpegAacError {
        value: MpegErrorValue::ShouldNotComeHere,
    })
}

fn usize_to_u8(value: usize) -> Result<u8, MpegAacError> {
    u8::try_from(value).map_err(|_| MpegAacError {
        value: MpegErrorValue::ShouldNotComeHere,
    })
}

fn usize_to_u32(value: usize) -> Result<u32, MpegAacError> {
    u32::try_from(value).map_err(|_| MpegAacError {
        value: MpegErrorValue::ShouldNotComeHere,
    })
}

fn u32_to_u8(value: u32) -> Result<u8, MpegAacError> {
    u8::try_from(value).map_err(|_| MpegAacError {
        value: MpegErrorValue::ShouldNotComeHere,
    })
}

#[derive(Debug, Clone, Default)]
pub struct Mpeg4Aac {
    pub object_type: u8,
    pub sampling_frequency_index: u8,
    pub channel_configuration: u8,

    pub sampling_frequency: u32,
    pub channels: u8,
    pub sbr: usize,
    pub ps: usize,
    pub pce: BytesMut,
    pub npce: usize,
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
    // 11 90
    // 00010 0011 0010 000
    // 2   3  2
    //https://wiki.multimedia.cx/index.php?title=MPEG-4_Audio#Audio_Specific_Config
    pub fn gen_audio_specific_config(&self) -> Result<BytesMut, MpegAacError> {
        let mut writer = BytesWriter::default();
        writer.write_u8(self.object_type << 3 | (self.sampling_frequency_index >> 1))?;
        writer.write_u8(
            (self.sampling_frequency_index & 0x01) << 7 | (self.channel_configuration << 3),
        )?;
        Ok(writer.extract_current_bytes())
    }
}

pub struct Mpeg4AacProcessor {
    pub bytes_reader: BytesReader,
    pub bytes_writer: BytesWriter,
    pub bits_reader: BitsReader,
    pub mpeg4_aac: Mpeg4Aac,
}

impl Default for Mpeg4AacProcessor {
    fn default() -> Self {
        Self::new()
    }
}
//https://blog.csdn.net/coloriy/article/details/90511746
impl Mpeg4AacProcessor {
    #[must_use]
    pub fn new() -> Self {
        Self {
            bytes_reader: BytesReader::new(BytesMut::new()),
            bytes_writer: BytesWriter::new(),
            bits_reader: BitsReader::new(BytesReader::new(BytesMut::new())),
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

        // tracing::info!("aac info: {:?}", self.mpeg4_aac);

        // if self.bytes_reader.len() > 2 {
        //return self.audio_specific_config_load2();
        // }

        // self.bytes_reader.read_u8()?;
        // self.bytes_reader.read_u8()?;

        self.bytes_reader.extract_remaining_bytes();

        Ok(self)
    }

    pub fn audio_specific_config_load2(&mut self) -> Result<(), MpegAacError> {
        let remain_bytes = self.bytes_reader.extract_remaining_bytes();
        // self.bits_reader.extend_from_bytesmut(remain_bytes);
        self.bits_reader.extend_data(&remain_bytes)?;

        self.mpeg4_aac.object_type = self.get_audio_object_type()?;
        self.mpeg4_aac.sampling_frequency_index = self.get_sampling_frequency()?;
        self.mpeg4_aac.channel_configuration = bits_to_u8(self.bits_reader.read_n_bits(4)?)?;

        let mut extension_audio_object_type: u8;
        let mut extension_sampling_frequency_index: u8 = 0;
        let mut extension_channel_configuration: u8 = 0;

        if self.mpeg4_aac.object_type == 5 || self.mpeg4_aac.object_type == 29 {
            extension_audio_object_type = 5;
            self.mpeg4_aac.sbr = 1;
            {
                if self.mpeg4_aac.object_type == 29 {
                    self.mpeg4_aac.ps = 1;
                }
                extension_sampling_frequency_index = self.get_sampling_frequency()?;
                self.mpeg4_aac.object_type = self.get_audio_object_type()?;

                if self.mpeg4_aac.object_type == 22 {
                    extension_channel_configuration = bits_to_u8(self.bits_reader.read_n_bits(4)?)?;
                }
            }
        } else {
            extension_audio_object_type = 0;
        }

        let ep_config: u64;

        match self.mpeg4_aac.object_type {
            1 | 2 | 3 | 4 | 5 | 6 | 7 | 17 | 19 | 20 | 21 | 22 | 23 => {
                self.ga_specific_config_load()?;
            }
            8 => {
                self.celp_specific_config_load()?;
            }
            _ => {}
        }

        match self.mpeg4_aac.object_type {
            17 | 19 | 20 | 21 | 22 | 23 | 24 | 25 | 26 | 27 | 39 => {
                ep_config = self.bits_reader.read_n_bits(2)?;

                match ep_config {
                    2 | 3 => {
                        return Err(MpegAacError {
                            value: MpegErrorValue::ShouldNotComeHere,
                        });
                    }

                    _ => {}
                }
            }
            _ => {}
        }

        let mut sync_extension_type: u64;

        if 5 != extension_audio_object_type && self.bits_reader.len() >= 16 {
            sync_extension_type = self.bits_reader.read_n_bits(11)?;

            if 0x2B7 == sync_extension_type {
                extension_audio_object_type = self.get_audio_object_type()?;

                match extension_audio_object_type {
                    5 => {
                        self.mpeg4_aac.sbr = bits_to_usize(self.bits_reader.read_n_bits(1)?)?;
                        if self.mpeg4_aac.sbr > 0 {
                            extension_sampling_frequency_index = self.get_sampling_frequency()?;
                            if self.bits_reader.len() >= 12 {
                                sync_extension_type = self.bits_reader.read_n_bits(11)?;
                                if 0x548 == sync_extension_type {
                                    self.mpeg4_aac.ps =
                                        bits_to_usize(self.bits_reader.read_n_bits(1)?)?;
                                }
                            }
                        }
                    }
                    22 => {
                        self.mpeg4_aac.sbr = bits_to_usize(self.bits_reader.read_n_bits(1)?)?;

                        if self.mpeg4_aac.sbr > 0 {
                            extension_sampling_frequency_index = self.get_sampling_frequency()?;
                        }

                        extension_channel_configuration =
                            bits_to_u8(self.bits_reader.read_n_bits(4)?)?;
                    }

                    _ => {}
                }
            }
        }

        self.bits_reader.bits_aligment_8();

        tracing::trace!(
            "remove warnings: {extension_audio_object_type} {extension_sampling_frequency_index} {extension_channel_configuration}"
        );

        Ok(())
    }

    pub fn celp_specific_config_load(&mut self) -> Result<(), MpegAacError> {
        let excitation_mode: u64;

        if self.bits_reader.read_n_bits(1)? > 0 {
            excitation_mode = self.bits_reader.read_n_bits(1)?;
            self.bits_reader.read_n_bits(1)?;
            self.bits_reader.read_n_bits(1)?;

            if excitation_mode == 1 {
                self.bits_reader.read_n_bits(3)?;
            } else if excitation_mode == 0 {
                self.bits_reader.read_n_bits(5)?;
                self.bits_reader.read_n_bits(2)?;
                self.bits_reader.read_n_bits(1)?;
            }
        } else {
            self.bits_reader.read_n_bits(1)?;
            self.bits_reader.read_n_bits(2)?;
        }

        // if self.bits_reader.read_n_bits(1)? > 0 {
        //     self.bits_reader.read_n_bits(2)?;
        // } else {
        //     self.bits_reader.read_n_bits(2)?;
        // }

        Ok(())
    }
    pub fn ga_specific_config_load(&mut self) -> Result<(), MpegAacError> {
        self.bits_reader.read_n_bits(1)?;

        if self.bits_reader.read_n_bits(1)? > 0 {
            self.bits_reader.read_n_bits(14)?;
        }
        let extension_flag: u64 = self.bits_reader.read_n_bits(1)?;

        if 0 == self.mpeg4_aac.channel_configuration {
            self.pce_load()?;
        }

        if self.mpeg4_aac.object_type == 6 || self.mpeg4_aac.object_type == 20 {
            self.bits_reader.read_n_bits(3)?;
        }

        if extension_flag > 0 {
            match self.mpeg4_aac.object_type {
                22 => {
                    self.bits_reader.read_n_bits(5)?;
                    self.bits_reader.read_n_bits(11)?;
                }
                17 | 19 | 20 | 23 => {
                    self.bits_reader.read_n_bits(1)?;
                    self.bits_reader.read_n_bits(1)?;
                    self.bits_reader.read_n_bits(1)?;
                }
                _ => {}
            }

            self.bits_reader.read_n_bits(1)?;
        }

        Ok(())
    }

    fn mpeg4_bits_copy(
        &mut self,
        writer: &mut BitsWriter,
        read_len: usize,
    ) -> Result<u64, MpegAacError> {
        let data = self.bits_reader.read_n_bits(read_len)?;
        writer.write_n_bits(data, read_len)?;
        Ok(data)
    }

    pub fn pce_load(&mut self) -> Result<u8, MpegAacError> {
        let mut cpe: u64 = 0;
        let mut tag: u64 = 0;

        let mut pce_bits_vec = BitsWriter::new(BytesWriter::new());
        pce_bits_vec.write_bytes(&self.mpeg4_aac.pce.clone())?;

        self.mpeg4_aac.channels = 0;

        let element_instance_tag: u64 = self.mpeg4_bits_copy(&mut pce_bits_vec, 4)?;
        let object_type: u64 = self.mpeg4_bits_copy(&mut pce_bits_vec, 2)?;
        let sampling_frequency_index: u64 = self.mpeg4_bits_copy(&mut pce_bits_vec, 4)?;
        let num_front_channel_elements: u64 = self.mpeg4_bits_copy(&mut pce_bits_vec, 4)?;
        let num_side_channel_elements: u64 = self.mpeg4_bits_copy(&mut pce_bits_vec, 4)?;
        let num_back_channel_elements: u64 = self.mpeg4_bits_copy(&mut pce_bits_vec, 4)?;
        let num_lfe_channel_elements: u64 = self.mpeg4_bits_copy(&mut pce_bits_vec, 2)?;
        let num_assoc_data_elements: u64 = self.mpeg4_bits_copy(&mut pce_bits_vec, 3)?;
        let num_valid_cc_elements: u64 = self.mpeg4_bits_copy(&mut pce_bits_vec, 4)?;

        for _ in 0..3 {
            if self.mpeg4_bits_copy(&mut pce_bits_vec, 1)? > 0 {
                self.mpeg4_bits_copy(&mut pce_bits_vec, 4)?;
            }
        }

        for _ in 0..num_front_channel_elements {
            cpe = self.mpeg4_bits_copy(&mut pce_bits_vec, 1)?;
            tag = self.mpeg4_bits_copy(&mut pce_bits_vec, 4)?;

            if cpe > 0 || self.mpeg4_aac.ps > 0 {
                self.mpeg4_aac.channels += 2;
            } else {
                self.mpeg4_aac.channels += 1;
            }
        }

        for _ in 0..num_side_channel_elements {
            cpe = self.mpeg4_bits_copy(&mut pce_bits_vec, 1)?;
            tag = self.mpeg4_bits_copy(&mut pce_bits_vec, 4)?;

            if cpe > 0 || self.mpeg4_aac.ps > 0 {
                self.mpeg4_aac.channels += 2;
            } else {
                self.mpeg4_aac.channels += 1;
            }
        }

        for _ in 0..num_back_channel_elements {
            cpe = self.mpeg4_bits_copy(&mut pce_bits_vec, 1)?;
            tag = self.mpeg4_bits_copy(&mut pce_bits_vec, 4)?;

            if cpe > 0 || self.mpeg4_aac.ps > 0 {
                self.mpeg4_aac.channels += 2;
            } else {
                self.mpeg4_aac.channels += 1;
            }
        }

        for _ in 0..num_lfe_channel_elements {
            tag = self.mpeg4_bits_copy(&mut pce_bits_vec, 4)?;
            self.mpeg4_aac.channels += 1;
        }

        for _ in 0..num_assoc_data_elements {
            tag = self.mpeg4_bits_copy(&mut pce_bits_vec, 4)?;
        }

        for _ in 0..num_valid_cc_elements {
            cpe = self.mpeg4_bits_copy(&mut pce_bits_vec, 1)?;
            tag = self.mpeg4_bits_copy(&mut pce_bits_vec, 4)?;
        }

        pce_bits_vec.bits_aligment_8()?;
        self.bits_reader.bits_aligment_8();

        let comment_field_bytes: u64 = self.mpeg4_bits_copy(&mut pce_bits_vec, 8)?;

        for _ in 0..comment_field_bytes {
            self.mpeg4_bits_copy(&mut pce_bits_vec, 8)?;
        }

        let rv = pce_bits_vec.len().div_ceil(8);

        tracing::trace!(
            "remove warnings: {tag} {element_instance_tag} {object_type} {sampling_frequency_index} {cpe}"
        );

        usize_to_u8(rv)
    }

    pub fn get_audio_object_type(&mut self) -> Result<u8, MpegAacError> {
        let mut audio_object_type: u64;

        audio_object_type = self.bits_reader.read_n_bits(5)?;
        if 31 == audio_object_type {
            audio_object_type = 32 + self.bits_reader.read_n_bits(6)?;
        }

        bits_to_u8(audio_object_type)
    }

    pub fn get_sampling_frequency(&mut self) -> Result<u8, MpegAacError> {
        let mut sampling_frequency_index: u64;

        sampling_frequency_index = self.bits_reader.read_n_bits(4)?;
        if sampling_frequency_index == 0x0F {
            sampling_frequency_index = self.bits_reader.read_n_bits(24)?;
        }

        bits_to_u8(sampling_frequency_index)
    }

    pub fn adts_save(&mut self) -> Result<(), MpegAacError> {
        let id = 0u8; // 0-MPEG4/1-MPEG2
        let len = usize_to_u32(self.bytes_reader.len() + 7)?;
        self.bytes_writer.write_u8(0xFF)?; //0
        self.bytes_writer.write_u8(
            0xF0 /* 12-syncword */ | (id << 3)/*1-ID*/| 0x01, /*1-protection_absent*/
        )?; //1

        let profile = self.mpeg4_aac.object_type;
        let sampling_frequency_index = self.mpeg4_aac.sampling_frequency_index;
        let channel_configuration = self.mpeg4_aac.channel_configuration;
        self.bytes_writer.write_u8(
            ((profile - 1) << 6)
                | ((sampling_frequency_index & 0x0F) << 2)
                | ((channel_configuration >> 2) & 0x01),
        )?; //2

        self.bytes_writer
            .write_u8(((channel_configuration & 0x03) << 6) | (u32_to_u8(len >> 11)? & 0x03))?; //3
        self.bytes_writer.write_u8(u32_to_u8(len >> 3)?)?; //4
        self.bytes_writer
            .write_u8(((len & 0x07) as u8) << 5 | 0x1F)?; //5
        self.bytes_writer.write_u8(0xFC)?; //6

        self.bytes_writer
            .write(&self.bytes_reader.extract_remaining_bytes()[..])?;

        Ok(())
    }
}
