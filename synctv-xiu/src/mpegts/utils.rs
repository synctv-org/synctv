use {
    super::define::epsi_stream_type,
    crate::bytesio::{bytes_errors::BytesWriteError, bytes_writer::BytesWriter},
};

pub fn pcr_write(pcr_result: &mut BytesWriter, pcr: i64) -> Result<(), BytesWriteError> {
    let pcr = u64::try_from(pcr).unwrap_or_default();
    let pcr_base = pcr / 300;
    let pcr_ext = pcr % 300;

    pcr_result.write_u8(u8::try_from((pcr_base >> 25) & 0xFF).unwrap_or_default())?;
    pcr_result.write_u8(u8::try_from((pcr_base >> 17) & 0xFF).unwrap_or_default())?;
    pcr_result.write_u8(u8::try_from((pcr_base >> 9) & 0xFF).unwrap_or_default())?;
    pcr_result.write_u8(u8::try_from((pcr_base >> 1) & 0xFF).unwrap_or_default())?;
    pcr_result.write_u8(
        u8::try_from(((pcr_base & 0x01) << 7) | 0x7E | ((pcr_ext >> 8) & 0x01)).unwrap_or_default(),
    )?;
    pcr_result.write_u8(u8::try_from(pcr_ext & 0xFF).unwrap_or_default())?;

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
