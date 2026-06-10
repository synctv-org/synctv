use {
    super::define::epsi_stream_type,
    crate::bytesio::{
        bytes_errors::{BytesWriteError, BytesWriteErrorValue},
        bytes_writer::BytesWriter,
    },
    std::io,
};

fn invalid_pcr(message: &str) -> BytesWriteError {
    BytesWriteError {
        value: BytesWriteErrorValue::IO(io::Error::new(io::ErrorKind::InvalidInput, message)),
    }
}

fn pcr_byte(value: u64) -> u8 {
    (value & 0xFF) as u8
}

pub fn pcr_write(pcr_result: &mut BytesWriter, pcr: i64) -> Result<(), BytesWriteError> {
    let pcr = u64::try_from(pcr).map_err(|_| invalid_pcr("PCR must be non-negative"))?;
    let pcr_base = pcr / 300;
    let pcr_ext = pcr % 300;

    pcr_result.write_u8(pcr_byte(pcr_base >> 25))?;
    pcr_result.write_u8(pcr_byte(pcr_base >> 17))?;
    pcr_result.write_u8(pcr_byte(pcr_base >> 9))?;
    pcr_result.write_u8(pcr_byte(pcr_base >> 1))?;
    pcr_result.write_u8(pcr_byte(
        ((pcr_base & 0x01) << 7) | 0x7E | ((pcr_ext >> 8) & 0x01),
    ))?;
    pcr_result.write_u8(pcr_byte(pcr_ext))?;

    Ok(())
}

#[must_use]
pub const fn is_steam_type_video(stream_type: u8) -> bool {
    matches!(
        stream_type,
        epsi_stream_type::PSI_STREAM_H264 | epsi_stream_type::PSI_STREAM_HEVC
    )
}

#[must_use]
pub const fn is_steam_type_audio(stream_type: u8) -> bool {
    matches!(
        stream_type,
        epsi_stream_type::PSI_STREAM_AUDIO_OPUS
            | epsi_stream_type::PSI_STREAM_AAC
            | epsi_stream_type::PSI_STREAM_MP3
            | epsi_stream_type::PSI_STREAM_MPEG4_AAC
    )
}
