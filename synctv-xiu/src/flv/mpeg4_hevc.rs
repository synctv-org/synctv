use {
    super::errors::Mpeg4AvcHevcError, crate::bytesio::bytes_reader::BytesReader,
    byteorder::BigEndian,
};
#[allow(dead_code)]
#[derive(Default)]

pub struct Mpeg4Hevc {
    configuration_version: u8, // 1-only
    general_profile_space: u8, // 2bit,[0,3]
    general_tier_flag: u8,     // 1bit,[0,1]
    general_profile_idc: u8,   // 5bit,[0,31]
    general_profile_compatibility_flags: u32,
    general_constraint_indicator_flags: u64,
    general_level_idc: u8,
    min_spatial_segmentation_idc: u16,
    parallelism_type: u8,        // 2bit,[0,3]
    chroma_format: u8,           // 2bit,[0,3]
    bit_depth_luma_minus8: u8,   // 3bit,[0,7]
    bit_depth_chroma_minus8: u8, // 3bit,[0,7]
    avg_frame_rate: u16,
    constant_frame_rate: u8,   // 2bit,[0,3]
    num_temporal_layers: u8,   // 3bit,[0,7]
    temporal_id_nested: u8,    // 1bit,[0,1]
    length_size_minus_one: u8, // 2bit,[0,3]
}

#[derive(Default)]
pub struct Mpeg4HevcProcessor {
    pub mpeg4_hevc: Mpeg4Hevc,
}

impl Mpeg4HevcProcessor {
    pub fn decoder_configuration_record_load(
        &mut self,
        bytes_reader: &mut BytesReader,
    ) -> Result<&mut Self, Mpeg4AvcHevcError> {
        self.mpeg4_hevc.configuration_version = bytes_reader.read_u8()?;
        let byte_1 = bytes_reader.read_u8()?;
        self.mpeg4_hevc.general_profile_space = (byte_1 >> 6) & 0x03;
        self.mpeg4_hevc.general_tier_flag = (byte_1 >> 5) & 0x01;
        self.mpeg4_hevc.general_profile_idc = byte_1 & 0x1F;
        self.mpeg4_hevc.general_profile_compatibility_flags =
            bytes_reader.read_u32::<BigEndian>()?;
        self.mpeg4_hevc.general_constraint_indicator_flags =
            bytes_reader.read_u48::<BigEndian>()?;
        self.mpeg4_hevc.general_level_idc = bytes_reader.read_u8()?;
        self.mpeg4_hevc.min_spatial_segmentation_idc =
            bytes_reader.read_u16::<BigEndian>()? & 0x0FFF;
        self.mpeg4_hevc.parallelism_type = bytes_reader.read_u8()? & 0x03;
        self.mpeg4_hevc.chroma_format = bytes_reader.read_u8()? & 0x03;
        self.mpeg4_hevc.bit_depth_luma_minus8 = bytes_reader.read_u8()? & 0x07;

        Ok(self)
    }
}
