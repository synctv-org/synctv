use {
    super::errors::{H264Error, H264ErrorValue, MAX_POC_CYCLE_ENTRIES},
    super::utils,
    crate::bytesio::bits_reader::BitsReader,
    crate::bytesio::bytes_reader::BytesReader,
    std::vec::Vec,
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
    pub bits_reader: BitsReader,
    pub sps: Sps,
}

impl SpsParser {
    #[must_use]
    pub fn new(reader: BytesReader) -> Self {
        Self {
            bits_reader: BitsReader::new(reader),
            sps: Sps::default(),
        }
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
                // Validate chroma_format_idc: valid values are 0, 1, 2, 3
                if self.sps.chroma_format_idc > 3 {
                    return Err(H264Error {
                        value: H264ErrorValue::InvalidChromaFormatIdc(self.sps.chroma_format_idc),
                    });
                }
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

                // The count is publisher-controlled input; bound it before any
                // derived allocation. Each loop iteration consumes at least one
                // bit and the BitsReader errors on exhaustion, so the pushes
                // grow amortized without an eager reserve.
                if self.sps.num_ref_frames_in_pic_order_cnt_cycle > MAX_POC_CYCLE_ENTRIES {
                    return Err(H264Error {
                        value: H264ErrorValue::InvalidPocCycleCount(
                            self.sps.num_ref_frames_in_pic_order_cnt_cycle,
                        ),
                    });
                }
                let count = self.sps.num_ref_frames_in_pic_order_cnt_cycle as usize;
                self.sps.offset_for_ref_frame.clear();
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

        let width = (self.sps.pic_width_in_mbs_minus1 + 1)
            .checked_mul(16)
            .and_then(|v| v.checked_sub(self.sps.frame_crop_left_offset * 2))
            .and_then(|v| v.checked_sub(self.sps.frame_crop_right_offset * 2))
            .ok_or(H264Error {
                value: H264ErrorValue::InvalidDimensions(
                    self.sps.pic_width_in_mbs_minus1,
                    self.sps.pic_height_in_map_units_minus1,
                ),
            })?;
        let height = (2 - u32::from(self.sps.frame_mbs_only_flag))
            .checked_mul(self.sps.pic_height_in_map_units_minus1 + 1)
            .and_then(|v| v.checked_mul(16))
            .and_then(|v| v.checked_sub(self.sps.frame_crop_top_offset * 2))
            .and_then(|v| v.checked_sub(self.sps.frame_crop_bottom_offset * 2))
            .ok_or(H264Error {
                value: H264ErrorValue::InvalidDimensions(
                    self.sps.pic_width_in_mbs_minus1,
                    self.sps.pic_height_in_map_units_minus1,
                ),
            })?;

        tracing::trace!("parsed sps data: {:?}", self.sps);
        Ok((width, height))
    }
}

#[cfg(test)]
mod tests {
    use super::{H264Error, H264ErrorValue, SpsParser, MAX_POC_CYCLE_ENTRIES};
    use bytes::BytesMut;

    /// Minimal bit-level SPS builder for parser policy tests (MSB-first).
    struct SpsBits {
        bits: Vec<bool>,
    }

    impl SpsBits {
        fn new() -> Self {
            let mut bits = Self { bits: Vec::new() };
            bits.bits.extend([false; 0]);
            bits
        }

        fn bit(mut self, bit: bool) -> Self {
            self.bits.push(bit);
            self
        }

        fn ue(self, value: u32) -> Self {
            // ue(v): for n = v+1 and k = floor(log2(n)), emit k zeros, a 1,
            // then k info bits holding n - 2^k.
            let n = u64::from(value) + 1;
            let k = (n.bit_width() - 1) as usize;
            let mut bits = self;
            for _ in 0..k {
                bits = bits.bit(false);
            }
            bits = bits.bit(true);
            let remainder = n - (1u64 << k);
            for i in (0..k).rev() {
                bits = bits.bit((remainder >> i) & 1 == 1);
            }
            bits
        }

        fn bytes(self) -> BytesMut {
            let mut out = Vec::new();
            let mut byte = 0u8;
            let mut filled = 0;
            for bit in self.bits {
                byte = (byte << 1) | u8::from(bit);
                filled += 1;
                if filled == 8 {
                    out.push(byte);
                    byte = 0;
                    filled = 0;
                }
            }
            if filled > 0 {
                out.push(byte << (8 - filled));
            }
            BytesMut::from(&out[..])
        }
    }

    fn baseline_header() -> SpsBits {
        // profile_idc=66 (baseline: no high-profile extension), flag=0, level_idc=30
        let bits = SpsBits::new();
        let bits = bits
            .bit(false)
            .bit(true)
            .bit(false)
            .bit(false)
            .bit(false)
            .bit(true)
            .bit(false)
            .bit(false) // 66
            .bit(false)
            .bit(false)
            .bit(false)
            .bit(false)
            .bit(false)
            .bit(false)
            .bit(false)
            .bit(false) // 0
            .bit(false)
            .bit(false)
            .bit(false)
            .bit(true)
            .bit(true)
            .bit(true)
            .bit(true)
            .bit(false); // 30
        bits
    }

    fn parse(bits: SpsBits) -> Result<(u32, u32), H264Error> {
        SpsParser::new(crate::bytesio::bytes_reader::BytesReader::new(bits.bytes())).parse()
    }

    #[test]
    fn rejects_poc_cycle_count_above_bound() {
        // seq_parameter_set_id=0, log2_max_frame_num_minus4=0, pic_order_cnt_type=1,
        // delta flag=0, offsets=0, count=256 (bound is 255)
        let bits = baseline_header()
            .ue(0)
            .ue(0)
            .ue(1)
            .bit(false)
            .ue(0)
            .ue(0)
            .ue(MAX_POC_CYCLE_ENTRIES + 1);
        match parse(bits) {
            Err(H264Error {
                value: H264ErrorValue::InvalidPocCycleCount(256),
            }) => {}
            other => panic!("expected InvalidPocCycleCount(256), got {other:?}"),
        }
    }

    #[test]
    fn rejects_u32_max_poc_cycle_count_without_giant_allocation() {
        // count = u32::MAX encodes as 32 leading zeros + '1' + 32 zero info
        // bits (n = 2^32, code_num = 2^32 - 1).
        let bits = baseline_header().ue(0).ue(0).ue(1).bit(false).ue(0).ue(0);
        let mut bits = bits;
        for _ in 0..32 {
            bits = bits.bit(false);
        }
        bits = bits.bit(true);
        for _ in 0..32 {
            bits = bits.bit(false);
        }
        match parse(bits) {
            Err(H264Error {
                value: H264ErrorValue::InvalidPocCycleCount(u32::MAX),
            }) => {}
            other => panic!("expected InvalidPocCycleCount(u32::MAX), got {other:?}"),
        }
    }

    #[test]
    fn parses_small_poc_cycle_count() {
        // count=1 with one se(v)=0 entry, then minimal tail fields; expects 16x16.
        let bits = baseline_header()
            .ue(0)
            .ue(0)
            .ue(1)
            .bit(false)
            .ue(0)
            .ue(0)
            .ue(1)
            .ue(0) // offset_for_ref_frame[0] = se(0) == ue(0)
            .ue(0) // max_num_ref_frames
            .bit(false) // gaps_in_frame_num_value_allowed_flag
            .ue(0) // pic_width_in_mbs_minus1
            .ue(0) // pic_height_in_map_units_minus1
            .bit(true) // frame_mbs_only_flag
            .bit(true) // direct_8x8_inference_flag
            .bit(false) // frame_cropping_flag
            .bit(false); // vui_parameters_present_flag
        assert_eq!(parse(bits).expect("small count must parse"), (16, 16));
    }
}
