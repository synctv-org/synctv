use {
    super::define::epsi_stream_type,
    crate::bytesio::{bytes_errors::BytesWriteError, bytes_writer::BytesWriter},
};

pub fn pcr_write(pcr_result: &mut BytesWriter, pcr: i64) -> Result<(), BytesWriteError> {
    let pcr_base: i64 = pcr / 300;
    let pcr_ext: i64 = pcr % 300;

    pcr_result.write_u8((pcr_base >> 25) as u8)?;
    pcr_result.write_u8((pcr_base >> 17) as u8)?;
    pcr_result.write_u8((pcr_base >> 9) as u8)?;
    pcr_result.write_u8((pcr_base >> 1) as u8)?;
    pcr_result.write_u8(((pcr_base & 0x01) << 7) as u8 | 0x7E | ((pcr_ext >> 8) & 0x01) as u8)?;
    pcr_result.write_u8((pcr_ext & 0xFF) as u8)?;

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
