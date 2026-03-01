//! CRC32 calculation for MPEG-TS tables (PAT/PMT)
//!
//! MPEG-TS uses a specific CRC32 algorithm with the polynomial 0x04C11DB7,
//! which is different from the standard IEEE CRC32 (0xEDB88320).
//!
//! Note: We cannot use `crc32fast` here because MPEG-TS uses a different
//! polynomial. This implementation uses a pre-computed lookup table for
//! performance.

use bytes::BytesMut;

/// MPEG-TS CRC32 lookup table (polynomial: 0x04C11DB7)
const CRC32_TABLE: [u32; 256] = [
    0x0000_0000,
    0xB71D_C104,
    0x6E3B_8209,
    0xD926_430D,
    0xDC76_0413,
    0x6B6B_C517,
    0xB24D_861A,
    0x0550_471E,
    0xB8ED_0826,
    0x0FF0_C922,
    0xD6D6_8A2F,
    0x61CB_4B2B,
    0x649B_0C35,
    0xD386_CD31,
    0x0AA0_8E3C,
    0xBDBD_4F38,
    0x70DB_114C,
    0xC7C6_D048,
    0x1EE0_9345,
    0xA9FD_5241,
    0xACAD_155F,
    0x1BB0_D45B,
    0xC296_9756,
    0x758B_5652,
    0xC836_196A,
    0x7F2B_D86E,
    0xA60D_9B63,
    0x1110_5A67,
    0x1440_1D79,
    0xA35D_DC7D,
    0x7A7B_9F70,
    0xCD66_5E74,
    0xE0B6_2398,
    0x57AB_E29C,
    0x8E8D_A191,
    0x3990_6095,
    0x3CC0_278B,
    0x8BDD_E68F,
    0x52FB_A582,
    0xE5E6_6486,
    0x585B_2BBE,
    0xEF46_EABA,
    0x3660_A9B7,
    0x817D_68B3,
    0x842D_2FAD,
    0x3330_EEA9,
    0xEA16_ADA4,
    0x5D0B_6CA0,
    0x906D_32D4,
    0x2770_F3D0,
    0xFE56_B0DD,
    0x494B_71D9,
    0x4C1B_36C7,
    0xFB06_F7C3,
    0x2220_B4CE,
    0x953D_75CA,
    0x2880_3AF2,
    0x9F9D_FBF6,
    0x46BB_B8FB,
    0xF1A6_79FF,
    0xF4F6_3EE1,
    0x43EB_FFE5,
    0x9ACD_BCE8,
    0x2DD0_7DEC,
    0x7770_8634,
    0xC06D_4730,
    0x194B_043D,
    0xAE56_C539,
    0xAB06_8227,
    0x1C1B_4323,
    0xC53D_002E,
    0x7220_C12A,
    0xCF9D_8E12,
    0x7880_4F16,
    0xA1A6_0C1B,
    0x16BB_CD1F,
    0x13EB_8A01,
    0xA4F6_4B05,
    0x7DD0_0808,
    0xCACD_C90C,
    0x07AB_9778,
    0xB0B6_567C,
    0x6990_1571,
    0xDE8D_D475,
    0xDBDD_936B,
    0x6CC0_526F,
    0xB5E6_1162,
    0x02FB_D066,
    0xBF46_9F5E,
    0x085B_5E5A,
    0xD17D_1D57,
    0x6660_DC53,
    0x6330_9B4D,
    0xD42D_5A49,
    0x0D0B_1944,
    0xBA16_D840,
    0x97C6_A5AC,
    0x20DB_64A8,
    0xF9FD_27A5,
    0x4EE0_E6A1,
    0x4BB0_A1BF,
    0xFCAD_60BB,
    0x258B_23B6,
    0x9296_E2B2,
    0x2F2B_AD8A,
    0x9836_6C8E,
    0x4110_2F83,
    0xF60D_EE87,
    0xF35D_A999,
    0x4440_689D,
    0x9D66_2B90,
    0x2A7B_EA94,
    0xE71D_B4E0,
    0x5000_75E4,
    0x8926_36E9,
    0x3E3B_F7ED,
    0x3B6B_B0F3,
    0x8C76_71F7,
    0x5550_32FA,
    0xE24D_F3FE,
    0x5FF0_BCC6,
    0xE8ED_7DC2,
    0x31CB_3ECF,
    0x86D6_FFCB,
    0x8386_B8D5,
    0x349B_79D1,
    0xEDBD_3ADC,
    0x5AA0_FBD8,
    0xEEE0_0C69,
    0x59FD_CD6D,
    0x80DB_8E60,
    0x37C6_4F64,
    0x3296_087A,
    0x858B_C97E,
    0x5CAD_8A73,
    0xEBB0_4B77,
    0x560D_044F,
    0xE110_C54B,
    0x3836_8646,
    0x8F2B_4742,
    0x8A7B_005C,
    0x3D66_C158,
    0xE440_8255,
    0x535D_4351,
    0x9E3B_1D25,
    0x2926_DC21,
    0xF000_9F2C,
    0x471D_5E28,
    0x424D_1936,
    0xF550_D832,
    0x2C76_9B3F,
    0x9B6B_5A3B,
    0x26D6_1503,
    0x91CB_D407,
    0x48ED_970A,
    0xFFF0_560E,
    0xFAA0_1110,
    0x4DBD_D014,
    0x949B_9319,
    0x2386_521D,
    0x0E56_2FF1,
    0xB94B_EEF5,
    0x606D_ADF8,
    0xD770_6CFC,
    0xD220_2BE2,
    0x653D_EAE6,
    0xBC1B_A9EB,
    0x0B06_68EF,
    0xB6BB_27D7,
    0x01A6_E6D3,
    0xD880_A5DE,
    0x6F9D_64DA,
    0x6ACD_23C4,
    0xDDD0_E2C0,
    0x04F6_A1CD,
    0xB3EB_60C9,
    0x7E8D_3EBD,
    0xC990_FFB9,
    0x10B6_BCB4,
    0xA7AB_7DB0,
    0xA2FB_3AAE,
    0x15E6_FBAA,
    0xCCC0_B8A7,
    0x7BDD_79A3,
    0xC660_369B,
    0x717D_F79F,
    0xA85B_B492,
    0x1F46_7596,
    0x1A16_3288,
    0xAD0B_F38C,
    0x742D_B081,
    0xC330_7185,
    0x9990_8A5D,
    0x2E8D_4B59,
    0xF7AB_0854,
    0x40B6_C950,
    0x45E6_8E4E,
    0xF2FB_4F4A,
    0x2BDD_0C47,
    0x9CC0_CD43,
    0x217D_827B,
    0x9660_437F,
    0x4F46_0072,
    0xF85B_C176,
    0xFD0B_8668,
    0x4A16_476C,
    0x9330_0461,
    0x242D_C565,
    0xE94B_9B11,
    0x5E56_5A15,
    0x8770_1918,
    0x306D_D81C,
    0x353D_9F02,
    0x8220_5E06,
    0x5B06_1D0B,
    0xEC1B_DC0F,
    0x51A6_9337,
    0xE6BB_5233,
    0x3F9D_113E,
    0x8880_D03A,
    0x8DD0_9724,
    0x3ACD_5620,
    0xE3EB_152D,
    0x54F6_D429,
    0x7926_A9C5,
    0xCE3B_68C1,
    0x171D_2BCC,
    0xA000_EAC8,
    0xA550_ADD6,
    0x124D_6CD2,
    0xCB6B_2FDF,
    0x7C76_EEDB,
    0xC1CB_A1E3,
    0x76D6_60E7,
    0xAFF0_23EA,
    0x18ED_E2EE,
    0x1DBD_A5F0,
    0xAAA0_64F4,
    0x7386_27F9,
    0xC49B_E6FD,
    0x09FD_B889,
    0xBEE0_798D,
    0x67C6_3A80,
    0xD0DB_FB84,
    0xD58B_BC9A,
    0x6296_7D9E,
    0xBBB0_3E93,
    0x0CAD_FF97,
    0xB110_B0AF,
    0x060D_71AB,
    0xDF2B_32A6,
    0x6836_F3A2,
    0x6D66_B4BC,
    0xDA7B_75B8,
    0x035D_36B5,
    0xB440_F7B1,
];

/// Calculate CRC32 for MPEG-TS tables using the MPEG-TS polynomial
///
/// # Arguments
/// * `crc` - Initial CRC value (typically 0xFFFFFFFF for MPEG-TS)
/// * `buffer` - Data to calculate CRC over
///
/// # Returns
/// The CRC32 checksum
#[must_use]
pub fn gen_crc32(crc: u32, buffer: BytesMut) -> u32 {
    let mut result: u32 = crc;

    for i in buffer {
        let a = result ^ u32::from(i);
        let b = CRC32_TABLE[(a & 0xff) as usize];
        let c = result >> 8;
        result = b ^ c;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn test_gen_crc32() {
        // Test data from MPEG-TS specification
        let data: [u8; 12] = [
            0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00, 0x00, 0x01, 0xE1, 0x00,
        ];

        let mut payload = BytesMut::new();
        payload.extend_from_slice(&data[..]);

        let result = gen_crc32(0xffff_ffff, payload);

        let aa0 = result & 0xFF;
        let bb0 = (result >> 8) & 0xFF;
        let cc0 = (result >> 16) & 0xFF;
        let dd0 = (result >> 24) & 0xFF;

        assert_eq!(aa0, 0xE8, "CRC byte 0 mismatch");
        assert_eq!(bb0, 0xF9, "CRC byte 1 mismatch");
        assert_eq!(cc0, 0x5E, "CRC byte 2 mismatch");
        assert_eq!(dd0, 0x7D, "CRC byte 3 mismatch");
    }

    #[test]
    fn test_crc32_consistency() {
        let data = b"Hello, World!";

        let mut payload1 = BytesMut::new();
        payload1.extend_from_slice(data);
        let mut payload2 = BytesMut::new();
        payload2.extend_from_slice(data);

        // Calculate twice to ensure consistency
        let crc1 = gen_crc32(0xFFFF_FFFF, payload1);
        let crc2 = gen_crc32(0xFFFF_FFFF, payload2);

        assert_eq!(crc1, crc2, "CRC32 should be deterministic");
    }

    #[test]
    fn test_crc32_empty() {
        let payload = BytesMut::new();
        let crc = gen_crc32(0xFFFF_FFFF, payload);
        assert_eq!(
            crc, 0xFFFF_FFFF,
            "CRC32 of empty data should equal initial value"
        );
    }
}
