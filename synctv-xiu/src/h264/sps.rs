use {
    super::errors::H264Error, super::utils, crate::bytesio::bits_reader::BitsReader,
    crate::bytesio::bytes_reader::BytesReader, bytes::BytesMut, std::vec::Vec,
};

#[derive(Default, Debug)]
pub struct Sps {
    pub profile_idc: u8, // u(8)
    flag: u8,

    pub level_idc: u8,         // u(8)
    seq_parameter_set_id: u32, // ue(v)

    chroma_format_idc: u32, // ue(v)

    separate_colour_plane_flag: u8,           // u(1)
    bit_depth_luma_minus8: u32,               // ue(v)
    bit_depth_chroma_minus8: u32,             // ue(v)
    qpprime_y_zero_transform_bypass_flag: u8, // u(1)

    seq_scaling_matrix_present_flag: u8, // u(1)

    seq_scaling_list_present_flag: Vec<u8>, // u(1)

    log2_max_frame_num_minus4: u32, // ue(v)
    pic_order_cnt_type: u32,        // ue(v)

    log2_max_pic_order_cnt_lsb_minus4: u32, // ue(v)

    delta_pic_order_always_zero_flag: u8,       // u(1)
    offset_for_non_ref_pic: i32,                // se(v)
    offset_for_top_to_bottom_field: i32,        // se(v)
    num_ref_frames_in_pic_order_cnt_cycle: u32, // ue(v)

    offset_for_ref_frame: Vec<i32>, // se(v)

    max_num_ref_frames: u32,                  // ue(v)
    gaps_in_frame_num_value_allowed_flag: u8, // u(1)

    pic_width_in_mbs_minus1: u32,        // ue(v)
    pic_height_in_map_units_minus1: u32, // ue(v)
    frame_mbs_only_flag: u8,             // u(1)

    mb_adaptive_frame_field_flag: u8, // u(1)

    direct_8x8_inference_flag: u8, // u(1)

    frame_cropping_flag: u8, // u(1)

    frame_crop_left_offset: u32,   // ue(v)
    frame_crop_right_offset: u32,  // ue(v)
    frame_crop_top_offset: u32,    // ue(v)
    frame_crop_bottom_offset: u32, // ue(v)

    vui_parameters_present_flag: u8, // u(1)
}

pub struct SpsParser {
    pub bytes_reader: BytesReader,
    pub bits_reader: BitsReader,
    pub sps: Sps,
}

impl SpsParser {
    #[must_use]
    pub fn new(reader: BytesReader) -> Self {
        Self {
            bytes_reader: BytesReader::new(BytesMut::new()),
            bits_reader: BitsReader::new(reader),
            sps: Sps::default(),
        }
    }

    pub fn extend_data(&mut self, data: BytesMut) -> Result<(), H264Error> {
        self.bits_reader.extend_data(data)?;
        Ok(())
    }

    pub fn parse(&mut self) -> Result<(u32, u32), H264Error> {
        self.sps.profile_idc = self.bits_reader.read_byte()?;
        tracing::info!("profile_idc: {}", self.sps.profile_idc);
        self.sps.flag = self.bits_reader.read_byte()?;
        self.sps.level_idc = self.bits_reader.read_byte()?;
        tracing::info!("level_idc: {}", self.sps.level_idc);
        self.sps.seq_parameter_set_id = utils::read_uev(&mut self.bits_reader)?;

        match self.sps.profile_idc {
            100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 => {
                self.sps.chroma_format_idc = utils::read_uev(&mut self.bits_reader)?;
                if self.sps.chroma_format_idc == 3 {
                    self.sps.separate_colour_plane_flag = self.bits_reader.read_bit()?;
                }
                self.sps.bit_depth_luma_minus8 = utils::read_uev(&mut self.bits_reader)?;
                self.sps.bit_depth_chroma_minus8 = utils::read_uev(&mut self.bits_reader)?;

                self.sps.qpprime_y_zero_transform_bypass_flag = self.bits_reader.read_bit()?;
                self.sps.seq_scaling_matrix_present_flag = self.bits_reader.read_bit()?;

                if self.sps.seq_scaling_matrix_present_flag > 0 {
                    let matrix_dim: usize = if self.sps.chroma_format_idc == 2 {
                        12
                    } else {
                        8
                    };

                    for _ in 0..matrix_dim {
                        self.sps
                            .seq_scaling_list_present_flag
                            .push(self.bits_reader.read_bit()?);
                    }
                }
            }
            _ => {}
        }

        self.sps.log2_max_frame_num_minus4 = utils::read_uev(&mut self.bits_reader)?;
        self.sps.pic_order_cnt_type = utils::read_uev(&mut self.bits_reader)?;

        match self.sps.pic_order_cnt_type {
            0 => {
                self.sps.log2_max_pic_order_cnt_lsb_minus4 =
                    utils::read_uev(&mut self.bits_reader)?;
            }
            1 => {
                self.sps.delta_pic_order_always_zero_flag = self.bits_reader.read_bit()?;
                self.sps.offset_for_non_ref_pic = utils::read_sev(&mut self.bits_reader)?;
                self.sps.offset_for_top_to_bottom_field = utils::read_sev(&mut self.bits_reader)?;
                self.sps.num_ref_frames_in_pic_order_cnt_cycle =
                    utils::read_uev(&mut self.bits_reader)?;

                // Clear and resize the vector before filling to avoid index out of bounds panic
                let count = self.sps.num_ref_frames_in_pic_order_cnt_cycle as usize;
                self.sps.offset_for_ref_frame.clear();
                self.sps.offset_for_ref_frame.reserve(count);
                for _ in 0..count {
                    self.sps
                        .offset_for_ref_frame
                        .push(utils::read_sev(&mut self.bits_reader)?);
                }
            }
            _ => {}
        }

        self.sps.max_num_ref_frames = utils::read_uev(&mut self.bits_reader)?;
        self.sps.gaps_in_frame_num_value_allowed_flag = self.bits_reader.read_bit()?;

        self.sps.pic_width_in_mbs_minus1 = utils::read_uev(&mut self.bits_reader)?;
        self.sps.pic_height_in_map_units_minus1 = utils::read_uev(&mut self.bits_reader)?;

        self.sps.frame_mbs_only_flag = self.bits_reader.read_bit()?;

        if self.sps.frame_mbs_only_flag == 0 {
            self.sps.mb_adaptive_frame_field_flag = self.bits_reader.read_bit()?;
        }
        self.sps.direct_8x8_inference_flag = self.bits_reader.read_bit()?;
        self.sps.frame_cropping_flag = self.bits_reader.read_bit()?;

        if self.sps.frame_cropping_flag > 0 {
            self.sps.frame_crop_left_offset = utils::read_uev(&mut self.bits_reader)?;
            self.sps.frame_crop_right_offset = utils::read_uev(&mut self.bits_reader)?;
            self.sps.frame_crop_top_offset = utils::read_uev(&mut self.bits_reader)?;
            self.sps.frame_crop_bottom_offset = utils::read_uev(&mut self.bits_reader)?;
        }

        self.sps.vui_parameters_present_flag = self.bits_reader.read_bit()?;

        let width = (self.sps.pic_width_in_mbs_minus1 + 1) * 16
            - self.sps.frame_crop_left_offset * 2
            - self.sps.frame_crop_right_offset * 2;
        let height = ((2 - u32::from(self.sps.frame_mbs_only_flag))
            * (self.sps.pic_height_in_map_units_minus1 + 1)
            * 16)
            - (self.sps.frame_crop_top_offset * 2)
            - (self.sps.frame_crop_bottom_offset * 2);

        tracing::trace!("parsed sps data: {:?}", self.sps);
        Ok((width, height))
    }
}
