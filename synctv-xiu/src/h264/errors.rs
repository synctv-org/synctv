use crate::bytesio::bits_errors::BitError;

/// Maximum number of leading zeros allowed in Exp-Golomb decoding.
/// This prevents DoS attacks via malicious SPS data with excessive leading zeros.
/// A 32-bit value requires at most 32 leading zeros.
pub const MAX_EXP_GOLOMB_LEADING_ZEROS: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum H264ErrorValue {
    #[error("bit error")]
    BitError(BitError),
    #[error(
        "Exp-Golomb decoding exceeded maximum leading zeros ({0} > {MAX_EXP_GOLOMB_LEADING_ZEROS})"
    )]
    ExpGolombOverflow(usize),
    #[error("Invalid chroma_format_idc value: {0}. Valid values are 0, 1, 2, 3")]
    InvalidChromaFormatIdc(u32),
}

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct H264Error {
    pub value: H264ErrorValue,
}

impl From<BitError> for H264Error {
    fn from(error: BitError) -> Self {
        Self {
            value: H264ErrorValue::BitError(error),
        }
    }
}
