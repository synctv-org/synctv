//! End-to-end tests for the live media path.
//!
//! The publishers use the production RTMP building blocks and a source-built
//! libavformat. The tests do not depend on a host `ffmpeg` binary.

#![allow(clippy::unwrap_used)]

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use bytes::Bytes;
use ffmpeg_next::{codec, encoder, format, media, Rational};
use ffmpeg_sys_next as _;
use rust_h265::{parse_annex_b as parse_hevc_annex_b, Decoder as HevcDecoder, PixelData};
use rusty_aac::{
    audio_specific_config_bytes, parse_audio_specific_config, AacDecoder, AacEncoder,
    AacEncoderConfig,
};
use rusty_h264::{Decoder as AvcDecoder, Encoder as AvcEncoder, EncoderConfig, YuvFrame};
use synctv_core_testing::{aac_test_tag, avc_test_tag, RtmpMediaType, RtmpPlayer, RtmpPublisher};
use synctv_xiu::{
    hls::{
        generation_registry_key, CleanupConfig, CustomHlsRemuxer, SegmentManager, StreamRegistry,
    },
    httpflv::HttpFlvSession,
    storage::{FileStorage, HlsStorage, MemoryStorage},
    streamhub::{define::STREAM_HUB_EVENT_CHANNEL_CAPACITY, utils::Uuid, StreamsHub},
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

const APP: &str = "live";
const STREAM: &str = "e2e";

#[derive(Debug, Clone)]
struct VideoAccessUnit {
    annex_b: Vec<u8>,
    flv_tag: Vec<u8>,
    keyframe: bool,
}

#[derive(Debug, Clone)]
struct VideoFixture {
    sequence_header: Vec<u8>,
    decoder_config_annex_b: Vec<u8>,
    access_units: Vec<VideoAccessUnit>,
}

#[derive(Debug, Clone)]
struct AudioFixture {
    sequence_header: Vec<u8>,
    access_units: Vec<Vec<u8>>,
}

fn initialize_ffmpeg() -> Result<()> {
    ffmpeg_next::init()?;
    format::network::init();
    if std::env::var_os("SYNCTV_TEST_FFMPEG_DEBUG").is_some() {
        ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Debug);
    }
    Ok(())
}

fn append_flv_tag(output: &mut Vec<u8>, tag_type: u8, timestamp: u32, body: &[u8]) -> Result<()> {
    let body_size = u32::try_from(body.len())?;
    anyhow::ensure!(body_size <= 0x00ff_ffff, "FFmpeg FLV tag is too large");
    output.push(tag_type);
    output.extend_from_slice(&body_size.to_be_bytes()[1..]);
    let timestamp_bytes = timestamp.to_be_bytes();
    output.extend_from_slice(&timestamp_bytes[1..]);
    output.push(timestamp_bytes[0]);
    output.extend_from_slice(&[0, 0, 0]);
    output.extend_from_slice(body);
    output.extend_from_slice(&(11_u32 + body_size).to_be_bytes());
    Ok(())
}

fn ffmpeg_fixture_file(
    video: &VideoFixture,
    audio: Option<&AudioFixture>,
) -> Result<tempfile::NamedTempFile> {
    let mut bytes = vec![
        b'F',
        b'L',
        b'V',
        1,
        0x01 | u8::from(audio.is_some()) << 2,
        0,
        0,
        0,
        9,
        0,
        0,
        0,
        0,
    ];
    append_flv_tag(&mut bytes, 9, 0, &video.sequence_header)?;
    if let Some(audio) = audio {
        append_flv_tag(&mut bytes, 8, 0, &audio.sequence_header)?;
    }
    for (index, timestamp) in [
        0_u32, 1_001, 2_001, 3_001, 5_001, 7_001, 9_001, 11_001, 13_001, 15_001, 17_001, 19_001,
    ]
    .into_iter()
    .enumerate()
    {
        let video_access_unit = &video.access_units[index % video.access_units.len()];
        append_flv_tag(&mut bytes, 9, timestamp, &video_access_unit.flv_tag)?;
        if let Some(audio) = audio {
            let audio_access_unit = &audio.access_units[index % audio.access_units.len()];
            append_flv_tag(&mut bytes, 8, timestamp, audio_access_unit)?;
        }
    }
    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(file.path(), bytes)?;
    Ok(file)
}

fn ffmpeg_publish(
    input_path: &std::path::Path,
    url: &str,
    output_format: &str,
    ready: tokio::sync::oneshot::Sender<()>,
    release_media: tokio::sync::oneshot::Receiver<()>,
) -> Result<()> {
    initialize_ffmpeg()?;
    let mut input = format::input(input_path)?;
    let mut output = format::output_as(url, output_format)?;
    let mut stream_mapping = vec![-1_i32; input.nb_streams() as usize];
    let mut input_time_bases = vec![Rational(0, 1); input.nb_streams() as usize];
    let mut output_index = 0_i32;
    let mut needs_audio = false;
    for (input_index, input_stream) in input.streams().enumerate() {
        if !matches!(
            input_stream.parameters().medium(),
            media::Type::Audio | media::Type::Video
        ) {
            continue;
        }
        needs_audio |= input_stream.parameters().medium() == media::Type::Audio;
        stream_mapping[input_index] = output_index;
        input_time_bases[input_index] = input_stream.time_base();
        output_index += 1;
        let mut output_stream = output.add_stream(encoder::find(codec::Id::None))?;
        output_stream.set_parameters(input_stream.parameters());
    }
    anyhow::ensure!(output_index > 0, "FFmpeg fixture has no media streams");

    {
        let mut options = ffmpeg_next::Dictionary::new();
        options.set("flush_packets", "1");
        let unused = output.write_header_with(options)?;
        anyhow::ensure!(
            unused.iter().next().is_none(),
            "FFmpeg did not consume output options: {unused:?}"
        );
    }
    let mut ready = Some(ready);
    let mut release_media = Some(release_media);
    let mut media_released = false;
    let mut video_packets_written = 0_usize;
    let mut audio_started = false;
    for (input_stream, mut packet) in input.packets() {
        let input_index = input_stream.index();
        let mapped_index = stream_mapping[input_index];
        if mapped_index >= 0 {
            if input_stream.parameters().medium() == media::Type::Video {
                video_packets_written += 1;
            } else if input_stream.parameters().medium() == media::Type::Audio {
                audio_started = true;
            }
            let output_stream = output
                .stream(usize::try_from(mapped_index)?)
                .context("FFmpeg output stream disappeared")?;
            packet.rescale_ts(input_time_bases[input_index], output_stream.time_base());
            packet.set_position(-1);
            packet.set_stream(usize::try_from(mapped_index)?);
            packet.write(&mut output)?;
            if video_packets_written >= 4 && (!needs_audio || audio_started) {
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                    release_media
                        .take()
                        .context("FFmpeg media release receiver disappeared")?
                        .blocking_recv()?;
                    media_released = true;
                }
            }
            if media_released {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    anyhow::ensure!(ready.is_none(), "FFmpeg fixture produced no video packet");
    output.write_trailer()?;
    Ok(())
}

fn annex_b_nals(data: &[u8]) -> Result<Vec<&[u8]>> {
    fn start_code_len(data: &[u8], offset: usize) -> Option<usize> {
        data.get(offset..)
            .is_some_and(|tail| tail.starts_with(&[0, 0, 0, 1]))
            .then_some(4)
            .or_else(|| {
                data.get(offset..)
                    .is_some_and(|tail| tail.starts_with(&[0, 0, 1]))
                    .then_some(3)
            })
    }

    let mut nals = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        let prefix = start_code_len(data, offset).context("Annex-B start code missing")?;
        let nal_start = offset + prefix;
        let mut next = nal_start;
        while next < data.len() && start_code_len(data, next).is_none() {
            next += 1;
        }
        anyhow::ensure!(nal_start < next, "empty Annex-B NAL unit");
        nals.push(&data[nal_start..next]);
        offset = next;
    }
    Ok(nals)
}

fn patterned_yuv_frame(width: usize, height: usize, phase: usize) -> YuvFrame {
    let mut y = vec![0; width * height];
    for row in 0..height {
        for column in 0..width {
            y[row * width + column] =
                u8::try_from((column * 7 + row * 11 + phase * 29) % 256).expect("luma fits u8");
        }
    }
    let chroma_len = width * height / 4;
    YuvFrame {
        width,
        height,
        y,
        u: vec![u8::try_from(80 + phase * 7).expect("U chroma fits u8"); chroma_len],
        v: vec![u8::try_from(160 - phase * 5).expect("V chroma fits u8"); chroma_len],
    }
}

fn avc_fixture() -> Result<VideoFixture> {
    const WIDTH: usize = 32;
    const HEIGHT: usize = 32;

    let mut config = EncoderConfig::new(WIDTH, HEIGHT);
    config.gop_size = 2;
    config.qp = 24;
    config.mbtree = false;
    let mut encoder = AvcEncoder::new(config)?;
    let access_units = (0..4)
        .map(|phase| encoder.encode(&patterned_yuv_frame(WIDTH, HEIGHT, phase)))
        .collect::<Vec<_>>();

    let all_annex_b = access_units.concat();
    let decoded = AvcDecoder::new().decode_stream(&all_annex_b)?;
    anyhow::ensure!(
        decoded.len() == 4,
        "real AVC fixture decoded {} frames",
        decoded.len()
    );
    anyhow::ensure!(
        decoded
            .iter()
            .all(|frame| frame.width == WIDTH && frame.height == HEIGHT),
        "real AVC fixture dimensions changed"
    );
    anyhow::ensure!(
        decoded
            .iter()
            .any(|frame| frame.y.iter().any(|sample| *sample != 0)),
        "real AVC fixture decoded blank luma"
    );

    let first_nals = annex_b_nals(&access_units[0])?;
    let sps = first_nals
        .iter()
        .find(|nal| nal[0] & 0x1f == 7)
        .context("encoded AVC fixture has no SPS")?
        .to_vec();
    let pps = first_nals
        .iter()
        .find(|nal| nal[0] & 0x1f == 8)
        .context("encoded AVC fixture has no PPS")?
        .to_vec();
    let mut sequence_header = vec![0x17, 0, 0, 0, 0, 1, sps[1], sps[2], sps[3], 0xff, 0xe1];
    sequence_header.extend_from_slice(&u16::try_from(sps.len())?.to_be_bytes());
    sequence_header.extend_from_slice(&sps);
    sequence_header.push(1);
    sequence_header.extend_from_slice(&u16::try_from(pps.len())?.to_be_bytes());
    sequence_header.extend_from_slice(&pps);

    let access_units = access_units
        .into_iter()
        .map(|annex_b| {
            let nals = annex_b_nals(&annex_b)?;
            let keyframe = nals.iter().any(|nal| nal[0] & 0x1f == 5);
            let mut flv_tag = vec![if keyframe { 0x17 } else { 0x27 }, 1, 0, 0, 0];
            for nal in nals
                .into_iter()
                .filter(|nal| !matches!(nal[0] & 0x1f, 7..=9))
            {
                flv_tag.extend_from_slice(&u32::try_from(nal.len())?.to_be_bytes());
                flv_tag.extend_from_slice(nal);
            }
            Ok(VideoAccessUnit {
                annex_b,
                flv_tag,
                keyframe,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(access_units.iter().filter(|unit| unit.keyframe).count() == 2);

    let mut decoder_config_annex_b = Vec::new();
    for nal in [&sps, &pps] {
        decoder_config_annex_b.extend_from_slice(&[0, 0, 0, 1]);
        decoder_config_annex_b.extend_from_slice(nal);
    }
    Ok(VideoFixture {
        sequence_header,
        decoder_config_annex_b,
        access_units,
    })
}

fn aac_fixture() -> Result<AudioFixture> {
    const SAMPLE_RATE: u32 = 44_100;
    const CHANNELS: u16 = 2;
    const SAMPLE_FRAMES: usize = 4_096;

    let mut pcm = Vec::with_capacity(SAMPLE_FRAMES * usize::from(CHANNELS));
    for index in 0..SAMPLE_FRAMES {
        let sample_index = u16::try_from(index).expect("AAC fixture sample index fits u16");
        let phase = f32::from(sample_index) / 44_100.0 * 440.0 * std::f32::consts::TAU;
        pcm.extend_from_slice(&[phase.sin() * 0.35, phase.cos() * 0.25]);
    }
    let mut encoder = AacEncoder::new(AacEncoderConfig {
        bitrate_bps: 96_000,
        ..AacEncoderConfig::default()
    });
    encoder.push_pcm(&pcm, CHANNELS, SAMPLE_RATE)?;
    encoder.finish();
    let mut access_units = Vec::new();
    while let Ok(packet) = encoder.next_packet() {
        access_units.push(packet.data);
    }
    anyhow::ensure!(
        access_units.len() >= 4,
        "real AAC fixture has too few access units"
    );

    let config = audio_specific_config_bytes(SAMPLE_RATE, CHANNELS);
    let mut decoder = AacDecoder::with_config(parse_audio_specific_config(&config)?);
    let mut decoded_samples = 0;
    let mut non_silent = false;
    for access_unit in &access_units {
        let decoded = decoder.decode(access_unit, None)?;
        anyhow::ensure!(decoded.sample_rate == SAMPLE_RATE && decoded.channels == CHANNELS);
        decoded_samples += decoded.samples.len();
        non_silent |= decoded.samples.iter().any(|sample| sample.abs() > 0.000_1);
    }
    anyhow::ensure!(
        decoded_samples > 0 && non_silent,
        "real AAC fixture decoded no audio"
    );

    let mut sequence_header = vec![0xaf, 0];
    sequence_header.extend_from_slice(&config);
    Ok(AudioFixture {
        sequence_header,
        access_units: access_units
            .into_iter()
            .map(|access_unit| {
                let mut tag = vec![0xaf, 1];
                tag.extend_from_slice(&access_unit);
                tag
            })
            .collect(),
    })
}

fn hevc_fixture() -> Result<VideoFixture> {
    // x265 4.1, 16x16 Main profile, one IDR followed by one P frame. The fixed
    // Annex-B bytes keep the test runtime independent from an FFmpeg/x265 binary.
    let annex_b = hex::decode(concat!(
        "0000000140010c01ffff01600000030090000003000003001e9594090000",
        "000142010101600000030090000003000003001ea08845965655bc2f0168",
        "080000030008000003000840",
        "000000014401c0718112",
        "0000012801ac16600edf82f0",
        "000000010201d0097880cb9980",
    ))?;
    assert_decodable_hevc(&annex_b, 2)?;
    let nals = annex_b_nals(&annex_b)?;
    let parameter_sets = [32_u8, 33, 34]
        .map(|nal_type| {
            nals.iter()
                .copied()
                .find(|nal| (nal[0] >> 1) & 0x3f == nal_type)
                .with_context(|| format!("real HEVC fixture has no NAL type {nal_type}"))
        })
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    let mut sequence_header = vec![0x1c, 0, 0, 0, 0];
    sequence_header.extend_from_slice(&[
        1, 1, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 30, 0xf0, 0, 0xfc, 0xfd, 0xf8, 0xf8, 0, 0, 0x0f, 3,
    ]);
    for (nal_type, nal) in [32_u8, 33, 34]
        .into_iter()
        .zip(parameter_sets.iter().copied())
    {
        sequence_header.push(0x80 | nal_type);
        sequence_header.extend_from_slice(&1_u16.to_be_bytes());
        sequence_header.extend_from_slice(&u16::try_from(nal.len())?.to_be_bytes());
        sequence_header.extend_from_slice(nal);
    }

    let access_units = nals
        .into_iter()
        .filter(|nal| !matches!((nal[0] >> 1) & 0x3f, 32..=34))
        .map(|nal| {
            let keyframe = matches!((nal[0] >> 1) & 0x3f, 19..=21);
            let mut flv_tag = vec![if keyframe { 0x1c } else { 0x2c }, 1, 0, 0, 0];
            flv_tag.extend_from_slice(&u32::try_from(nal.len())?.to_be_bytes());
            flv_tag.extend_from_slice(nal);
            let mut annex_b = vec![0, 0, 0, 1];
            annex_b.extend_from_slice(nal);
            Ok(VideoAccessUnit {
                annex_b,
                flv_tag,
                keyframe,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(access_units.len() == 2 && access_units[0].keyframe);

    let mut decoder_config_annex_b = Vec::new();
    for nal in parameter_sets {
        decoder_config_annex_b.extend_from_slice(&[0, 0, 0, 1]);
        decoder_config_annex_b.extend_from_slice(nal);
    }
    Ok(VideoFixture {
        sequence_header,
        decoder_config_annex_b,
        access_units,
    })
}

fn assert_decodable_hevc(annex_b: &[u8], minimum_frames: usize) -> Result<()> {
    let nals = parse_hevc_annex_b(annex_b);
    let mut decoder = HevcDecoder::new();
    let mut frames = Vec::new();
    for nal in &nals {
        if let Some(frame) = decoder.decode_nal(nal)? {
            frames.push(frame);
        }
    }
    while let Some(frame) = decoder.flush() {
        frames.push(frame);
    }
    anyhow::ensure!(
        frames.len() >= minimum_frames,
        "HEVC decoded {} frames",
        frames.len()
    );
    anyhow::ensure!(
        frames
            .iter()
            .all(|frame| frame.width == 16 && frame.height == 16),
        "HEVC dimensions changed"
    );
    anyhow::ensure!(frames.iter().all(|frame| match &frame.y {
        PixelData::U8(samples) => !samples.is_empty(),
        PixelData::U16(samples) => !samples.is_empty(),
    }));
    Ok(())
}

#[derive(Debug)]
struct PesInspection {
    stream_id: u8,
    pts: Vec<u64>,
    random_access_seen: bool,
}

#[derive(Debug)]
struct TsInspection {
    pmt_pid: u16,
    pmt_version: u8,
    pcr_pid: u16,
    streams: BTreeMap<u16, u8>,
    pes: BTreeMap<u16, PesInspection>,
}

fn ts_payload(packet: &[u8]) -> Result<Option<(&[u8], bool)>> {
    anyhow::ensure!(packet.len() == 188, "invalid TS packet size");
    anyhow::ensure!(packet[0] == 0x47, "invalid TS sync byte");
    let adaptation_field_control = (packet[3] >> 4) & 0x03;
    let (offset, random_access) = match adaptation_field_control {
        1 => (4, false),
        2 => return Ok(None),
        3 => {
            let adaptation_length = usize::from(packet[4]);
            let offset = 5 + adaptation_length;
            anyhow::ensure!(offset <= packet.len(), "invalid TS adaptation field");
            let random_access = adaptation_length > 0 && packet[5] & 0x40 != 0;
            (offset, random_access)
        }
        _ => anyhow::bail!("TS packet has no adaptation field or payload"),
    };
    Ok((offset < packet.len()).then_some((&packet[offset..], random_access)))
}

fn psi_section(payload: &[u8]) -> Result<&[u8]> {
    let pointer = usize::from(*payload.first().context("PSI pointer field missing")?);
    let section = payload
        .get(1 + pointer..)
        .context("PSI pointer exceeds packet payload")?;
    anyhow::ensure!(section.len() >= 3, "PSI section header missing");
    let section_length = (usize::from(section[1] & 0x0f) << 8) | usize::from(section[2]);
    section
        .get(..3 + section_length)
        .context("truncated PSI section")
}

fn decode_pts(bytes: &[u8]) -> Result<u64> {
    anyhow::ensure!(bytes.len() >= 5, "truncated PES PTS");
    anyhow::ensure!(
        bytes[0] & 1 == 1 && bytes[2] & 1 == 1 && bytes[4] & 1 == 1,
        "invalid PES PTS markers"
    );
    Ok((u64::from((bytes[0] >> 1) & 0x07) << 30)
        | (u64::from(bytes[1]) << 22)
        | (u64::from((bytes[2] >> 1) & 0x7f) << 15)
        | (u64::from(bytes[3]) << 7)
        | u64::from((bytes[4] >> 1) & 0x7f))
}

fn inspect_ts(data: &[u8]) -> Result<TsInspection> {
    anyhow::ensure!(!data.is_empty(), "empty TS segment");
    anyhow::ensure!(data.len().is_multiple_of(188), "unaligned TS segment");

    let mut pmt_pid = None;
    let mut pmt_version = None;
    let mut pcr_pid = None;
    let mut streams = BTreeMap::new();
    let mut pes = BTreeMap::<u16, PesInspection>::new();
    let mut continuity_counters = BTreeMap::<u16, u8>::new();

    for packet in data.as_chunks::<188>().0 {
        let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
        let payload_unit_start = packet[1] & 0x40 != 0;
        let Some((payload, random_access)) = ts_payload(packet)? else {
            continue;
        };
        let continuity_counter = packet[3] & 0x0f;
        if let Some(previous) = continuity_counters.insert(pid, continuity_counter) {
            anyhow::ensure!(
                continuity_counter == (previous + 1) % 16,
                "TS continuity counter jumped for PID {pid:#x}: {previous} -> {continuity_counter}"
            );
        }

        if pid == 0 && payload_unit_start {
            let section = psi_section(payload)?;
            anyhow::ensure!(section[0] == 0x00, "PAT table ID mismatch");
            let end = section.len().checked_sub(4).context("PAT CRC missing")?;
            for entry in section[8..end].as_chunks::<4>().0 {
                if u16::from_be_bytes([entry[0], entry[1]]) != 0 {
                    pmt_pid = Some((u16::from(entry[2] & 0x1f) << 8) | u16::from(entry[3]));
                    break;
                }
            }
            continue;
        }

        if Some(pid) == pmt_pid && payload_unit_start {
            let section = psi_section(payload)?;
            anyhow::ensure!(section[0] == 0x02, "PMT table ID mismatch");
            pmt_version = Some((section[5] >> 1) & 0x1f);
            pcr_pid = Some((u16::from(section[8] & 0x1f) << 8) | u16::from(section[9]));
            let program_info_length =
                (usize::from(section[10] & 0x0f) << 8) | usize::from(section[11]);
            let mut offset = 12 + program_info_length;
            let end = section.len().checked_sub(4).context("PMT CRC missing")?;
            streams.clear();
            while offset < end {
                anyhow::ensure!(offset + 5 <= end, "truncated PMT stream entry");
                let stream_type = section[offset];
                let elementary_pid =
                    (u16::from(section[offset + 1] & 0x1f) << 8) | u16::from(section[offset + 2]);
                let es_info_length = (usize::from(section[offset + 3] & 0x0f) << 8)
                    | usize::from(section[offset + 4]);
                streams.insert(elementary_pid, stream_type);
                offset += 5 + es_info_length;
            }
            continue;
        }

        if payload_unit_start && streams.contains_key(&pid) {
            anyhow::ensure!(payload.len() >= 14, "truncated PES header");
            anyhow::ensure!(&payload[..3] == b"\0\0\x01", "invalid PES start code");
            let pts_dts_flags = (payload[7] >> 6) & 0x03;
            anyhow::ensure!(pts_dts_flags == 2 || pts_dts_flags == 3, "PES PTS missing");
            let entry = pes.entry(pid).or_insert_with(|| PesInspection {
                stream_id: payload[3],
                pts: Vec::new(),
                random_access_seen: false,
            });
            anyhow::ensure!(entry.stream_id == payload[3], "PES stream ID changed");
            entry.pts.push(decode_pts(&payload[9..14])?);
            entry.random_access_seen |= random_access;
        }
    }

    Ok(TsInspection {
        pmt_pid: pmt_pid.context("PAT did not declare a PMT")?,
        pmt_version: pmt_version.context("PMT version missing")?,
        pcr_pid: pcr_pid.context("PMT PCR PID missing")?,
        streams,
        pes,
    })
}

fn extract_ts_elementary_stream(data: &[u8], target_pid: u16) -> Result<Vec<u8>> {
    anyhow::ensure!(data.len().is_multiple_of(188), "unaligned TS segment");
    let mut elementary = Vec::new();
    for packet in data.as_chunks::<188>().0 {
        let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
        if pid != target_pid {
            continue;
        }
        let payload_unit_start = packet[1] & 0x40 != 0;
        let Some((payload, _)) = ts_payload(packet)? else {
            continue;
        };
        if payload_unit_start {
            anyhow::ensure!(payload.len() >= 9, "truncated PES header");
            anyhow::ensure!(&payload[..3] == b"\0\0\x01", "invalid PES start code");
            let header_len = 9 + usize::from(payload[8]);
            elementary.extend_from_slice(
                payload
                    .get(header_len..)
                    .context("truncated PES optional header")?,
            );
        } else {
            elementary.extend_from_slice(payload);
        }
    }
    anyhow::ensure!(
        !elementary.is_empty(),
        "TS PID {target_pid:#x} has no payload"
    );
    Ok(elementary)
}

fn flv_tag_body(tag: &[u8], expected_type: u8) -> Result<&[u8]> {
    anyhow::ensure!(tag.len() >= 15, "truncated FLV tag");
    anyhow::ensure!(tag[0] == expected_type, "unexpected FLV tag type");
    let data_size = (usize::from(tag[1]) << 16) | (usize::from(tag[2]) << 8) | usize::from(tag[3]);
    let body_end = 11 + data_size;
    anyhow::ensure!(tag.len() == body_end + 4, "FLV tag size mismatch");
    let previous_size = u32::from_be_bytes(tag[body_end..].try_into()?);
    anyhow::ensure!(
        usize::try_from(previous_size)? == body_end,
        "FLV previous tag size mismatch"
    );
    Ok(&tag[11..body_end])
}

fn avcc_payload_to_annex_b(payload: &[u8]) -> Result<Vec<u8>> {
    anyhow::ensure!(
        payload.len() >= 5 && payload[1] == 1,
        "AVC/HEVC media tag missing"
    );
    let mut annex_b = Vec::new();
    let mut offset = 5;
    while offset < payload.len() {
        anyhow::ensure!(offset + 4 <= payload.len(), "truncated length-prefixed NAL");
        let nal_len = usize::try_from(u32::from_be_bytes(payload[offset..offset + 4].try_into()?))?;
        offset += 4;
        let nal = payload
            .get(offset..offset + nal_len)
            .context("truncated NAL payload")?;
        annex_b.extend_from_slice(&[0, 0, 0, 1]);
        annex_b.extend_from_slice(nal);
        offset += nal_len;
    }
    Ok(annex_b)
}

fn decode_adts_stream(data: &[u8]) -> Result<(usize, bool)> {
    let mut decoder = AacDecoder::new();
    let mut offset = 0;
    let mut sample_count = 0;
    let mut non_silent = false;
    while offset < data.len() {
        let header = rusty_aac::parse_adts(&data[offset..])?;
        let end = offset + header.frame_length;
        let frame = decoder.decode(data.get(offset..end).context("truncated ADTS frame")?, None)?;
        sample_count += frame.samples.len();
        non_silent |= frame.samples.iter().any(|sample| sample.abs() > 0.000_1);
        offset = end;
    }
    Ok((sample_count, non_silent))
}

fn assert_pts_monotonic(inspection: &TsInspection) {
    for (pid, stream) in &inspection.pes {
        assert!(
            stream.pts.windows(2).all(|pair| pair[0] <= pair[1]),
            "PTS regressed for PID {pid:#x}: {:?}",
            stream.pts
        );
    }
}

fn assert_pts_continue(previous: &mut BTreeMap<u16, u64>, inspection: &TsInspection) {
    for (pid, stream) in &inspection.pes {
        if let (Some(previous_pts), Some(first_pts)) = (previous.get(pid), stream.pts.first()) {
            assert!(
                previous_pts <= first_pts,
                "PTS regressed across segments for PID {pid:#x}: {previous_pts} -> {first_pts}"
            );
        }
        if let Some(last_pts) = stream.pts.last() {
            previous.insert(*pid, *last_pts);
        }
    }
}

fn assert_ts_tracks(inspection: &TsInspection, expected: &[(u16, u8, u8)]) {
    assert_eq!(inspection.pmt_pid, 0x100);
    assert_eq!(
        inspection.streams.len(),
        expected.len(),
        "unexpected PMT streams: {:?}",
        inspection.streams
    );
    assert_eq!(
        inspection.pes.len(),
        expected.len(),
        "unexpected PES streams: {:?}",
        inspection.pes.keys().collect::<Vec<_>>()
    );
    for &(pid, stream_type, stream_id) in expected {
        assert_eq!(inspection.streams.get(&pid), Some(&stream_type));
        let pes = inspection.pes.get(&pid).expect("declared PID has no PES");
        assert_eq!(pes.stream_id, stream_id);
        assert!(!pes.pts.is_empty());
    }
    assert!(inspection.streams.contains_key(&inspection.pcr_pid));
    assert_pts_monotonic(inspection);
}

struct FixedModeAuth {
    mode: synctv_xiu::rtmp::auth::RtmpStreamMode,
    unpublish_count: Arc<AtomicUsize>,
}

struct RejectFirstPublishAuth {
    publish_attempts: Arc<AtomicUsize>,
    rollback_count: Arc<AtomicUsize>,
    unpublish_count: Arc<AtomicUsize>,
}

#[async_trait]
impl synctv_xiu::rtmp::auth::AuthCallback for RejectFirstPublishAuth {
    async fn on_publish(
        &self,
        _generation_id: Uuid,
        _app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) -> std::result::Result<
        Option<synctv_xiu::rtmp::auth::AuthPublishRewrite>,
        Box<dyn std::error::Error + Send + Sync>,
    > {
        if self.publish_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "test publish rejected",
            )
            .into());
        }
        Ok(None)
    }

    async fn on_play(
        &self,
        _app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }

    async fn on_unpublish(
        &self,
        _generation_id: Uuid,
        _app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) {
        self.unpublish_count.fetch_add(1, Ordering::SeqCst);
    }

    async fn on_publish_rollback(
        &self,
        _generation_id: Uuid,
        _app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) {
        self.rollback_count.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl synctv_xiu::rtmp::auth::AuthCallback for FixedModeAuth {
    async fn on_publish(
        &self,
        _generation_id: Uuid,
        app_name: &str,
        stream_name: &str,
        _query: Option<&str>,
    ) -> std::result::Result<
        Option<synctv_xiu::rtmp::auth::AuthPublishRewrite>,
        Box<dyn std::error::Error + Send + Sync>,
    > {
        Ok(Some(synctv_xiu::rtmp::auth::AuthPublishRewrite {
            app_name: app_name.to_string(),
            stream_name: stream_name.to_string(),
            media_mode: self.mode,
        }))
    }

    async fn on_play(
        &self,
        _app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }

    async fn on_unpublish(
        &self,
        _generation_id: Uuid,
        _app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) {
        self.unpublish_count.fetch_add(1, Ordering::SeqCst);
    }
}

struct TestServer {
    address: std::net::SocketAddr,
    shutdown: CancellationToken,
    server_task: tokio::task::JoinHandle<()>,
    hub_task: tokio::task::JoinHandle<()>,
    hls_task: tokio::task::JoinHandle<()>,
    cleanup_shutdown: CancellationToken,
    cleanup_task: tokio::task::JoinHandle<()>,
    event_sender: mpsc::Sender<synctv_xiu::streamhub::define::StreamHubEvent>,
    registry: StreamRegistry,
    storage: Arc<dyn HlsStorage>,
    segment_manager: Arc<SegmentManager>,
}

async fn start_server() -> Result<TestServer> {
    start_server_with_options(Arc::new(MemoryStorage::unlimited()), None).await
}

async fn start_rtmp_hub_server() -> Result<TestServer> {
    start_rtmp_hub_server_with_auth(None).await
}

async fn start_rtmp_hub_server_with_auth(
    auth: Option<Arc<dyn synctv_xiu::rtmp::auth::AuthCallback>>,
) -> Result<TestServer> {
    let (event_tx, event_rx) = mpsc::channel(STREAM_HUB_EVENT_CHANNEL_CAPACITY);
    let server_event_sender = event_tx.clone();
    let mut hub = StreamsHub::new(event_tx.clone(), event_rx);
    let hub_task = tokio::spawn(async move {
        let _ = hub.run().await;
    });

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server_cancel = CancellationToken::new();
    let server_cancel_for_task = server_cancel.clone();
    let mut rtmp_server =
        synctv_xiu::rtmp::server::RtmpServer::new(address.to_string(), event_tx, 4, auth, None)
            .with_listener(listener)
            .with_cancellation_token(&server_cancel_for_task)
            .with_shutdown_grace_period(Duration::from_millis(200));
    let server_task = tokio::spawn(async move {
        let _ = rtmp_server.run().await;
    });
    let hls_task = tokio::spawn(std::future::pending());
    let storage: Arc<dyn HlsStorage> = Arc::new(MemoryStorage::unlimited());
    let segment_manager = Arc::new(SegmentManager::new(
        Arc::clone(&storage),
        CleanupConfig {
            interval: Duration::from_secs(3600),
            retention: Duration::from_secs(3600),
            final_playlist_grace: Duration::from_secs(60),
            ended_segment_grace: Duration::from_secs(90),
            max_segments_per_stream: 0,
        },
    ));
    let cleanup_shutdown = CancellationToken::new();
    let cleanup_task = Arc::clone(&segment_manager).start_cleanup_task(cleanup_shutdown.clone());

    Ok(TestServer {
        address,
        shutdown: server_cancel,
        server_task,
        hub_task,
        hls_task,
        cleanup_shutdown,
        cleanup_task,
        event_sender: server_event_sender,
        registry: Arc::new(dashmap::DashMap::new()),
        storage,
        segment_manager,
    })
}

async fn start_server_with_storage(storage: Arc<dyn HlsStorage>) -> Result<TestServer> {
    start_server_with_options(storage, None).await
}

async fn start_server_with_storage_and_cleanup(
    storage: Arc<dyn HlsStorage>,
    cleanup: CleanupConfig,
) -> Result<TestServer> {
    start_server_with_options_and_cleanup(storage, None, cleanup).await
}

async fn start_server_with_auth(
    auth: Arc<dyn synctv_xiu::rtmp::auth::AuthCallback>,
) -> Result<TestServer> {
    start_server_with_options(Arc::new(MemoryStorage::unlimited()), Some(auth)).await
}

async fn start_server_with_options(
    storage: Arc<dyn HlsStorage>,
    auth: Option<Arc<dyn synctv_xiu::rtmp::auth::AuthCallback>>,
) -> Result<TestServer> {
    start_server_with_options_and_cleanup(
        storage,
        auth,
        CleanupConfig {
            interval: Duration::from_secs(3600),
            retention: Duration::from_secs(3600),
            final_playlist_grace: Duration::from_secs(60),
            ended_segment_grace: Duration::from_secs(90),
            max_segments_per_stream: 0,
        },
    )
    .await
}

async fn start_server_with_options_and_cleanup(
    storage: Arc<dyn HlsStorage>,
    auth: Option<Arc<dyn synctv_xiu::rtmp::auth::AuthCallback>>,
    cleanup: CleanupConfig,
) -> Result<TestServer> {
    let (event_tx, event_rx) = mpsc::channel(STREAM_HUB_EVENT_CHANNEL_CAPACITY);
    let server_event_sender = event_tx.clone();
    let mut hub = StreamsHub::new(event_tx.clone(), event_rx);
    let broadcast = hub.get_client_event_consumer();
    let hub_sender = hub.get_hub_event_sender();
    let hub_task = tokio::spawn(async move {
        let _ = hub.run().await;
    });

    let segment_manager = Arc::new(SegmentManager::new(Arc::clone(&storage), cleanup));
    let cleanup_shutdown = CancellationToken::new();
    let cleanup_task = Arc::clone(&segment_manager).start_cleanup_task(cleanup_shutdown.clone());
    let registry = Arc::new(dashmap::DashMap::new());
    let hls_cancel = CancellationToken::new();
    let hls_registry = Arc::clone(&registry);
    let hls_manager = Arc::clone(&segment_manager);
    let hls_cancel_for_task = hls_cancel.clone();
    let hls_task = tokio::spawn(async move {
        let mut remuxer = CustomHlsRemuxer::new(
            broadcast,
            hub_sender,
            hls_manager,
            hls_registry,
            hls_cancel_for_task,
        );
        let _ = remuxer.run().await;
    });

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server_cancel = CancellationToken::new();
    let server_cancel_for_task = server_cancel.clone();
    let mut server =
        synctv_xiu::rtmp::server::RtmpServer::new(address.to_string(), event_tx, 4, auth, None)
            .with_listener(listener)
            .with_cancellation_token(&server_cancel_for_task)
            .with_shutdown_grace_period(Duration::from_millis(200));
    let server_task = tokio::spawn(async move {
        let _ = server.run().await;
    });

    Ok(TestServer {
        address,
        shutdown: server_cancel,
        server_task,
        hub_task,
        hls_task,
        cleanup_shutdown,
        cleanup_task,
        event_sender: server_event_sender,
        registry,
        storage,
        segment_manager,
    })
}

impl TestServer {
    async fn stop(self) {
        self.shutdown.cancel();
        self.cleanup_shutdown.cancel();
        let _ = self.cleanup_task.await;
        self.server_task.abort();
        self.hls_task.abort();
        self.hub_task.abort();
        let _ = self.server_task.await;
        let _ = self.hls_task.await;
        let _ = self.hub_task.await;
    }
}

async fn wait_for_playlist_matching<F>(
    server: &TestServer,
    matches: F,
    timeout_message: &str,
) -> Arc<parking_lot::RwLock<synctv_xiu::hls::StreamProcessorState>>
where
    F: Fn(&synctv_xiu::hls::StreamProcessorState) -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(state) = server.registry.iter().find_map(|entry| {
            let state = entry.value().read();
            matches(&state).then(|| Arc::clone(entry.value()))
        }) {
            return state;
        }
        assert!(Instant::now() < deadline, "{timeout_message}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_playlist(
    server: &TestServer,
) -> Arc<parking_lot::RwLock<synctv_xiu::hls::StreamProcessorState>> {
    wait_for_playlist_matching(
        server,
        |state| state.app_name == APP && state.stream_name == STREAM,
        "HLS handler did not register",
    )
    .await
}

async fn subscribe_frames(
    server: &TestServer,
) -> Result<synctv_xiu::streamhub::define::FrameDataReceiver> {
    subscribe_stream_frames(server, STREAM).await
}

async fn subscribe_stream_frames(
    server: &TestServer,
    stream_name: &str,
) -> Result<synctv_xiu::streamhub::define::FrameDataReceiver> {
    use synctv_xiu::streamhub::{
        define::{NotifyInfo, StreamHubEvent, SubDataType, SubscribeType, SubscriberInfo},
        stream::StreamIdentifier,
        utils::Uuid,
    };

    let (result_sender, result_receiver) = tokio::sync::oneshot::channel();
    server
        .event_sender
        .send(StreamHubEvent::Subscribe {
            identifier: StreamIdentifier::Rtmp {
                app_name: APP.to_string(),
                stream_name: stream_name.to_string(),
            },
            info: SubscriberInfo {
                id: Uuid::new(),
                sub_type: SubscribeType::RtmpPull,
                sub_data_type: SubDataType::Frame,
                notify_info: NotifyInfo {
                    request_url: "test://direct-subscriber".to_string(),
                    remote_addr: "127.0.0.1".to_string(),
                },
            },
            result_sender,
        })
        .await?;
    let receiver = result_receiver
        .await??
        .0
        .frame_receiver
        .context("StreamHub direct frame subscription did not return a frame receiver")?;
    Ok(receiver)
}

async fn read_rtsp_request(stream: &mut TcpStream) -> Result<String> {
    let mut request = Vec::new();
    let mut byte = [0_u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        anyhow::ensure!(stream.read(&mut byte).await? == 1, "RTSP client closed");
        request.push(byte[0]);
        anyhow::ensure!(request.len() <= 16 * 1024, "RTSP request is too large");
    }
    Ok(String::from_utf8(request)?)
}

fn rtsp_cseq(request: &str) -> Result<&str> {
    request
        .lines()
        .find_map(|line| line.strip_prefix("CSeq: "))
        .context("RTSP request is missing CSeq")
}

fn rtsp_client_interleaved_channel(setup: &str) -> Result<u8> {
    setup
        .lines()
        .find_map(|line| line.strip_prefix("Transport: "))
        .context("RTSP SETUP is missing Transport")?
        .split(';')
        .find_map(|part| part.strip_prefix("interleaved="))
        .and_then(|channels| channels.split_once('-').map(|(rtp, _)| rtp))
        .context("RTSP SETUP is missing interleaved channels")?
        .parse()
        .context("invalid RTSP interleaved RTP channel")
}

async fn write_interleaved_rtp(
    stream: &mut TcpStream,
    channel: u8,
    payload_type: u8,
    sequence: u16,
    timestamp: u32,
    payload: &[u8],
) -> Result<()> {
    write_interleaved_rtp_with_marker(
        stream,
        channel,
        payload_type,
        sequence,
        timestamp,
        payload,
        true,
    )
    .await
}

async fn write_interleaved_rtp_with_marker(
    stream: &mut TcpStream,
    channel: u8,
    payload_type: u8,
    sequence: u16,
    timestamp: u32,
    payload: &[u8],
    marker: bool,
) -> Result<()> {
    let mut rtp = Vec::with_capacity(12 + payload.len());
    rtp.extend_from_slice(&[0x80, (u8::from(marker) << 7) | payload_type]);
    rtp.extend_from_slice(&sequence.to_be_bytes());
    rtp.extend_from_slice(&timestamp.to_be_bytes());
    rtp.extend_from_slice(&[1, 2, 3, 4]);
    rtp.extend_from_slice(payload);
    stream.write_all(&[b'$', channel]).await?;
    stream
        .write_all(&u16::try_from(rtp.len())?.to_be_bytes())
        .await?;
    stream.write_all(&rtp).await?;
    Ok(())
}

fn hevc_fu_payload(nal: &[u8], start: bool, end: bool, fragment: &[u8]) -> Result<Vec<u8>> {
    anyhow::ensure!(nal.len() >= 3, "HEVC NAL too short for FU packetization");
    let nal_type = (nal[0] >> 1) & 0x3f;
    let mut payload = Vec::with_capacity(3 + fragment.len());
    payload.push((nal[0] & 0x81) | (49 << 1));
    payload.push(nal[1]);
    payload.push((u8::from(start) << 7) | (u8::from(end) << 6) | nal_type);
    payload.extend_from_slice(fragment);
    Ok(payload)
}

async fn spawn_interleaved_rtsp_source(
    video: &VideoFixture,
    audio: &AudioFixture,
) -> Result<(
    std::net::SocketAddr,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<()>>,
)> {
    let parameter_sets = annex_b_nals(&video.decoder_config_annex_b)?;
    let sps = parameter_sets
        .iter()
        .find(|nal| nal[0] & 0x1f == 7)
        .context("real RTSP AVC fixture has no SPS")?;
    let pps = parameter_sets
        .iter()
        .find(|nal| nal[0] & 0x1f == 8)
        .context("real RTSP AVC fixture has no PPS")?;
    let sprop_parameter_sets = format!("{},{}", BASE64.encode(sps), BASE64.encode(pps));
    let audio_config = hex::encode(
        audio
            .sequence_header
            .get(2..)
            .context("real RTSP AAC fixture has no AudioSpecificConfig")?,
    );
    let video_access_units = video
        .access_units
        .iter()
        .map(|unit| {
            annex_b_nals(&unit.annex_b)?
                .into_iter()
                .find(|nal| matches!(nal[0] & 0x1f, 1..=5))
                .map(<[u8]>::to_vec)
                .context("real RTSP AVC access unit has no VCL NAL")
        })
        .collect::<Result<Vec<_>>>()?;
    let audio_access_units = audio
        .access_units
        .iter()
        .map(|tag| {
            tag.get(2..)
                .map(<[u8]>::to_vec)
                .context("real RTSP AAC access unit has no raw payload")
        })
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(video_access_units.len() >= 4 && audio_access_units.len() >= 4);

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let describe = read_rtsp_request(&mut stream).await?;
        let sdp = format!(
            concat!(
                "v=0\r\n",
                "o=- 1 1 IN IP4 127.0.0.1\r\n",
                "s=SyncTV RTSP integration source\r\n",
                "t=0 0\r\n",
                "a=control:*\r\n",
                "m=video 0 RTP/AVP 96\r\n",
                "c=IN IP4 0.0.0.0\r\n",
                "a=rtpmap:96 H264/90000\r\n",
                "a=fmtp:96 packetization-mode=1;sprop-parameter-sets={}\r\n",
                "a=control:trackID=1\r\n",
                "m=audio 0 RTP/AVP 97\r\n",
                "c=IN IP4 0.0.0.0\r\n",
                "a=rtpmap:97 MPEG4-GENERIC/44100/2\r\n",
                "a=fmtp:97 profile-level-id=1;mode=AAC-hbr;config={};sizelength=13;indexlength=3;indexdeltalength=3\r\n",
                "a=control:trackID=2\r\n",
            ),
            sprop_parameter_sets, audio_config
        );
        let response = format!(
            "RTSP/1.0 200 OK\r\nCSeq: {}\r\nContent-Type: application/sdp\r\nContent-Base: rtsp://{address}/live/\r\nContent-Length: {}\r\n\r\n{sdp}",
            rtsp_cseq(&describe)?,
            sdp.len()
        );
        stream.write_all(response.as_bytes()).await?;

        let mut video_channel = None;
        let mut audio_channel = None;
        loop {
            let request = read_rtsp_request(&mut stream).await?;
            if request.starts_with("PLAY ") {
                let response = format!(
                    "RTSP/1.0 200 OK\r\nCSeq: {}\r\nSession: synctv-e2e\r\nRTP-Info: url=rtsp://{address}/live/trackID=1;seq=1;rtptime=0,url=rtsp://{address}/live/trackID=2;seq=1;rtptime=0\r\n\r\n",
                    rtsp_cseq(&request)?
                );
                stream.write_all(response.as_bytes()).await?;
                break;
            }

            anyhow::ensure!(
                request.starts_with("SETUP "),
                "unexpected RTSP request: {request}"
            );
            let channel = rtsp_client_interleaved_channel(&request)?;
            let track_id = if request.contains("trackID=1") {
                video_channel = Some(channel);
                1
            } else if request.contains("trackID=2") {
                audio_channel = Some(channel);
                2
            } else {
                anyhow::bail!("unknown RTSP track in SETUP request: {request}");
            };
            let response = format!(
                "RTSP/1.0 200 OK\r\nCSeq: {}\r\nSession: synctv-e2e;timeout=60\r\nTransport: RTP/AVP/TCP;unicast;interleaved={channel}-{};ssrc=01020304;mode=\"play\"\r\n\r\n",
                rtsp_cseq(&request)?,
                channel + 1
            );
            tracing::debug!(track_id, channel, "RTSP fixture accepted SETUP");
            stream.write_all(response.as_bytes()).await?;
        }
        let _ = release_rx.await;

        for (index, (video_timestamp, audio_timestamp)) in [
            (0_u32, 0_u32),
            (450_000, 220_500),
            (900_000, 441_000),
            (990_000, 485_100),
        ]
        .into_iter()
        .enumerate()
        {
            let sequence = u16::try_from(index + 1)?;
            if let Some(video_channel) = video_channel {
                write_interleaved_rtp(
                    &mut stream,
                    video_channel,
                    96,
                    sequence,
                    video_timestamp,
                    &video_access_units[index],
                )
                .await?;
            }
            if let Some(audio_channel) = audio_channel {
                let audio = &audio_access_units[index];
                let au_size_bits = u16::try_from(audio.len())?
                    .checked_shl(3)
                    .context("real RTSP AAC access unit is too large")?;
                let mut audio_payload = vec![0, 16];
                audio_payload.extend_from_slice(&au_size_bits.to_be_bytes());
                audio_payload.extend_from_slice(audio);
                write_interleaved_rtp(
                    &mut stream,
                    audio_channel,
                    97,
                    sequence,
                    audio_timestamp,
                    &audio_payload,
                )
                .await?;
            }
        }
        stream.shutdown().await?;
        Ok(())
    });
    Ok((address, release_tx, task))
}

async fn spawn_interleaved_hevc_rtsp_source() -> Result<(
    std::net::SocketAddr,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<()>>,
)> {
    let fixture = hevc_fixture()?;
    let parameter_sets = annex_b_nals(&fixture.decoder_config_annex_b)?;
    let parameter_set = |nal_type| {
        parameter_sets
            .iter()
            .find(|nal| (nal[0] >> 1) & 0x3f == nal_type)
            .map(|nal| BASE64.encode(nal))
            .with_context(|| format!("HEVC RTSP fixture has no NAL type {nal_type}"))
    };
    let vps = parameter_set(32)?;
    let sps = parameter_set(33)?;
    let pps = parameter_set(34)?;
    let idr = annex_b_nals(&fixture.access_units[0].annex_b)?[0].to_vec();
    let inter = annex_b_nals(&fixture.access_units[1].annex_b)?[0].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let describe = read_rtsp_request(&mut stream).await?;
        let sdp = format!(
            concat!(
                "v=0\r\n",
                "o=- 1 1 IN IP4 127.0.0.1\r\n",
                "s=SyncTV HEVC RTSP integration source\r\n",
                "t=0 0\r\n",
                "a=control:*\r\n",
                "m=video 0 RTP/AVP 96\r\n",
                "c=IN IP4 0.0.0.0\r\n",
                "a=rtpmap:96 H265/90000\r\n",
                "a=fmtp:96 profile-id=1;sprop-vps={vps};sprop-sps={sps};sprop-pps={pps}\r\n",
                "a=control:trackID=1\r\n",
            ),
            vps = vps,
            sps = sps,
            pps = pps
        );
        let response = format!(
            "RTSP/1.0 200 OK\r\nCSeq: {}\r\nContent-Type: application/sdp\r\nContent-Base: rtsp://{address}/live/\r\nContent-Length: {}\r\n\r\n{sdp}",
            rtsp_cseq(&describe)?,
            sdp.len()
        );
        stream.write_all(response.as_bytes()).await?;

        let setup = read_rtsp_request(&mut stream).await?;
        let response = format!(
            "RTSP/1.0 200 OK\r\nCSeq: {}\r\nSession: synctv-hevc;timeout=60\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1;ssrc=01020304;mode=\"play\"\r\n\r\n",
            rtsp_cseq(&setup)?
        );
        stream.write_all(response.as_bytes()).await?;

        let play = read_rtsp_request(&mut stream).await?;
        let response = format!(
            "RTSP/1.0 200 OK\r\nCSeq: {}\r\nSession: synctv-hevc\r\nRTP-Info: url=rtsp://{address}/live/trackID=1;seq=1;rtptime=0\r\n\r\n",
            rtsp_cseq(&play)?
        );
        stream.write_all(response.as_bytes()).await?;
        let _ = release_rx.await;

        let split = (idr.len() - 2) / 2;
        let idr_start = hevc_fu_payload(&idr, true, false, &idr[2..2 + split])?;
        let idr_end = hevc_fu_payload(&idr, false, true, &idr[2 + split..])?;
        for (sequence, timestamp, payload, marker) in [
            (1_u16, 0_u32, idr_start.as_slice(), false),
            (2, 0, idr_end.as_slice(), true),
            (3, 450_000, inter.as_slice(), true),
        ] {
            write_interleaved_rtp_with_marker(
                &mut stream,
                0,
                96,
                sequence,
                timestamp,
                payload,
                marker,
            )
            .await?;
        }
        stream.shutdown().await?;
        Ok(())
    });
    Ok((address, release_tx, task))
}

fn rtsp_client_rtp_port(setup: &str) -> Result<u16> {
    let transport = setup
        .lines()
        .find_map(|line| line.strip_prefix("Transport: "))
        .context("UDP RTSP SETUP is missing Transport")?;
    transport
        .split(';')
        .find_map(|part| part.strip_prefix("client_port="))
        .and_then(|ports| ports.split_once('-').map(|(rtp, _)| rtp))
        .context("UDP RTSP SETUP is missing client_port")?
        .parse()
        .context("invalid RTSP client RTP port")
}

async fn spawn_udp_rtsp_source() -> Result<(
    std::net::SocketAddr,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<()>>,
)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut control, peer) = listener.accept().await?;
        let describe = read_rtsp_request(&mut control).await?;
        let sdp = concat!(
            "v=0\r\n",
            "o=- 1 1 IN IP4 127.0.0.1\r\n",
            "s=SyncTV UDP RTSP source\r\n",
            "t=0 0\r\n",
            "a=control:*\r\n",
            "m=video 0 RTP/AVP 96\r\n",
            "c=IN IP4 127.0.0.1\r\n",
            "a=rtpmap:96 H264/90000\r\n",
            "a=fmtp:96 packetization-mode=1;sprop-parameter-sets=Z0IAH5WoFAFuQA==,aM4G4g==\r\n",
            "a=control:trackID=1\r\n",
        );
        let response = format!(
            "RTSP/1.0 200 OK\r\nCSeq: {}\r\nContent-Type: application/sdp\r\nContent-Base: rtsp://{address}/live/\r\nContent-Length: {}\r\n\r\n{sdp}",
            rtsp_cseq(&describe)?,
            sdp.len()
        );
        control.write_all(response.as_bytes()).await?;

        let setup = read_rtsp_request(&mut control).await?;
        let client_rtp_port = rtsp_client_rtp_port(&setup)?;
        let rtp_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await?;
        let server_rtp_port = rtp_socket.local_addr()?.port();
        let response = format!(
            "RTSP/1.0 200 OK\r\nCSeq: {}\r\nSession: synctv-udp;timeout=60\r\nTransport: RTP/AVP;unicast;client_port={client_rtp_port}-{};server_port={server_rtp_port}-{};ssrc=01020304;mode=\"play\"\r\n\r\n",
            rtsp_cseq(&setup)?,
            client_rtp_port.saturating_add(1),
            server_rtp_port.saturating_add(1),
        );
        control.write_all(response.as_bytes()).await?;

        let play = read_rtsp_request(&mut control).await?;
        let response = format!(
            "RTSP/1.0 200 OK\r\nCSeq: {}\r\nSession: synctv-udp\r\nRTP-Info: url=rtsp://{address}/live/trackID=1;seq=1;rtptime=0\r\n\r\n",
            rtsp_cseq(&play)?
        );
        control.write_all(response.as_bytes()).await?;
        let _ = release_rx.await;

        let client = std::net::SocketAddr::new(peer.ip(), client_rtp_port);
        for (sequence, timestamp, nal) in [
            (1_u16, 0_u32, &[0x65, 0x88, 0x84][..]),
            (2, 450_000, &[0x41, 0x9a, 0x22][..]),
            (3, 900_000, &[0x65, 0x88, 0x85][..]),
            (4, 990_000, &[0x41, 0x9a, 0x23][..]),
        ] {
            let mut rtp = Vec::with_capacity(12 + nal.len());
            rtp.extend_from_slice(&[0x80, 0xe0]);
            rtp.extend_from_slice(&sequence.to_be_bytes());
            rtp.extend_from_slice(&timestamp.to_be_bytes());
            rtp.extend_from_slice(&[1, 2, 3, 4]);
            rtp.extend_from_slice(nal);
            rtp_socket.send_to(&rtp, client).await?;
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        control.shutdown().await?;
        Ok(())
    });
    Ok((address, release_tx, task))
}

async fn publish_external_frames(
    server: &TestServer,
    request_url: &str,
) -> Result<(
    synctv_xiu::streamhub::define::FrameDataSender,
    synctv_xiu::streamhub::utils::Uuid,
)> {
    use synctv_xiu::{
        rtmp::session::common::RtmpStreamHandler,
        streamhub::{
            define::{NotifyInfo, PubDataType, PublishType, PublisherInfo, StreamHubEvent},
            stream::StreamIdentifier,
            utils::Uuid,
        },
    };

    let (result_sender, result_receiver) = tokio::sync::oneshot::channel();
    let generation_id = Uuid::new();
    server
        .event_sender
        .send(StreamHubEvent::Publish {
            identifier: StreamIdentifier::Rtmp {
                app_name: APP.to_string(),
                stream_name: STREAM.to_string(),
            },
            info: PublisherInfo {
                id: generation_id,
                pub_type: PublishType::ExternalPull,
                pub_data_type: PubDataType::Frame,
                notify_info: NotifyInfo {
                    request_url: request_url.to_string(),
                    remote_addr: "127.0.0.1".to_string(),
                },
            },
            result_sender,
            stream_handler: Arc::new(RtmpStreamHandler::new()),
        })
        .await?;
    let frame_sender = result_receiver
        .await??
        .0
        .context("external publication did not return a frame sender")?;
    Ok((frame_sender, generation_id))
}

async fn unpublish_test_stream(
    server: &TestServer,
    generation_id: synctv_xiu::streamhub::utils::Uuid,
) -> Result<()> {
    use synctv_xiu::streamhub::{define::StreamHubEvent, stream::StreamIdentifier};
    server
        .event_sender
        .send(StreamHubEvent::UnPublish {
            identifier: StreamIdentifier::Rtmp {
                app_name: APP.to_string(),
                stream_name: STREAM.to_string(),
            },
            generation_id,
        })
        .await?;
    Ok(())
}

async fn wait_for_playlist_generation(
    server: &TestServer,
    previous_generation_id: Option<synctv_xiu::streamhub::utils::Uuid>,
) -> Arc<parking_lot::RwLock<synctv_xiu::hls::StreamProcessorState>> {
    wait_for_playlist_matching(
        server,
        |state| {
            state.app_name == APP
                && state.stream_name == STREAM
                && previous_generation_id != Some(state.generation_id)
        },
        "HLS generation did not register a new publisher owner",
    )
    .await
}

async fn send_hls_gops(
    publisher: &mut RtmpPublisher,
    state: &Arc<parking_lot::RwLock<synctv_xiu::hls::StreamProcessorState>>,
    gop_count: u32,
) -> Result<Option<String>> {
    publisher.send_video(0, true).await?;
    publisher.send_audio(0).await?;
    publisher.send_video(1, true).await?;

    let mut first_segment_name = None;
    for segment in 0..gop_count {
        let start = segment * 10_000 + 1;
        if segment > 0 {
            publisher.send_video(start, true).await?;
            if segment == 1 {
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    if let Some(name) = state
                        .read()
                        .playlist
                        .segments
                        .front()
                        .map(|segment| segment.ts_name.clone())
                    {
                        first_segment_name = Some(name);
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "first HLS segment was not written"
                    );
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
        for offset in (1..10).map(|second| second * 1_000) {
            publisher.send_video(start + offset, false).await?;
        }
        publisher.send_audio(start + 9_000).await?;
    }
    Ok(first_segment_name)
}

async fn wait_for_generation_end(
    state: &Arc<parking_lot::RwLock<synctv_xiu::hls::StreamProcessorState>>,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if state.read().marked_for_cleanup {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "HLS generation did not reach cleanup-marked state"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_ended_segments(
    server: &TestServer,
    state: &Arc<parking_lot::RwLock<synctv_xiu::hls::StreamProcessorState>>,
) -> Result<Vec<Vec<u8>>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (ended, names) = {
            let state = state.read();
            (
                state.marked_for_cleanup,
                state
                    .playlist
                    .segments
                    .iter()
                    .map(|segment| segment.ts_name.clone())
                    .collect::<Vec<_>>(),
            )
        };
        if ended {
            anyhow::ensure!(!names.is_empty(), "ended HLS playlist has no segments");
            let mut segments = Vec::with_capacity(names.len());
            for name in names {
                segments.push(server.storage.read(APP, STREAM, &name).await?.to_vec());
            }
            return Ok(segments);
        }
        anyhow::ensure!(Instant::now() < deadline, "HLS playlist did not end");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn start_flv_capture(
    server: &TestServer,
) -> Result<(
    tokio::sync::mpsc::Receiver<std::io::Result<Bytes>>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
)> {
    let (flv_tx, flv_rx) = mpsc::channel(32);
    let mut session = HttpFlvSession::new(
        APP.to_string(),
        STREAM.to_string(),
        server.event_sender.clone(),
        flv_tx,
    );
    session.start().await?;
    let task = tokio::spawn(async move { session.run_after_start().await });
    Ok((flv_rx, task))
}

async fn finish_flv_capture(
    mut receiver: tokio::sync::mpsc::Receiver<std::io::Result<Bytes>>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
) -> Result<Vec<Bytes>> {
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .context("HTTP-FLV capture did not finish")???;
    let header = receiver.recv().await.context("HTTP-FLV header missing")??;
    anyhow::ensure!(header.starts_with(b"FLV"), "HTTP-FLV header invalid");
    let mut tags = Vec::new();
    while let Some(tag) = receiver.recv().await {
        tags.push(tag?);
    }
    Ok(tags)
}

fn assert_decodable_avc_aac_flv(
    tags: &[Bytes],
    video: &VideoFixture,
    audio: &AudioFixture,
) -> Result<()> {
    let mut flv_video = video.decoder_config_annex_b.clone();
    let mut flv_audio_decoder = None;
    let mut flv_video_frames = 0;
    let mut flv_audio_samples = 0;
    let mut flv_audio_non_silent = false;
    for tag in tags {
        match tag.first().copied() {
            Some(9) => {
                let body = flv_tag_body(tag, 9)?;
                if body.get(1) == Some(&0) {
                    anyhow::ensure!(body == video.sequence_header);
                } else if body.get(1) == Some(&1) {
                    flv_video.extend_from_slice(&avcc_payload_to_annex_b(body)?);
                    flv_video_frames += 1;
                }
            }
            Some(8) => {
                let body = flv_tag_body(tag, 8)?;
                if body.get(1) == Some(&0) {
                    anyhow::ensure!(body == audio.sequence_header);
                    flv_audio_decoder = Some(AacDecoder::with_config(parse_audio_specific_config(
                        body.get(2..).context("FLV AAC config missing")?,
                    )?));
                } else if body.get(1) == Some(&1) {
                    let decoded = flv_audio_decoder
                        .as_mut()
                        .context("FLV AAC frame arrived before config")?
                        .decode(body.get(2..).context("FLV AAC frame missing")?, None)?;
                    flv_audio_samples += decoded.samples.len();
                    flv_audio_non_silent |=
                        decoded.samples.iter().any(|sample| sample.abs() > 0.000_1);
                }
            }
            _ => {}
        }
    }
    let decoded_flv = AvcDecoder::new().decode_stream(&flv_video)?;
    anyhow::ensure!(flv_video_frames >= 4 && decoded_flv.len() >= 4);
    anyhow::ensure!(flv_audio_samples > 0 && flv_audio_non_silent);
    Ok(())
}

#[tokio::test]
async fn rtmp_publish_fans_out_to_hls_and_http_flv_and_finishes_playlist() -> Result<()> {
    let server = start_server().await?;
    exercise_rtmp_hls_flv_pipeline(server).await
}

#[tokio::test]
async fn ended_hls_generation_remains_routable_during_playlist_grace() -> Result<()> {
    let server = start_server().await?;
    let mut publisher = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    publisher.send_video(0, true).await?;
    publisher.send_audio(0).await?;
    publisher.send_video(1, true).await?;
    let state = wait_for_playlist(&server).await;

    publisher.close();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if state
            .read()
            .playlist
            .generate_m3u8(str::to_string)
            .contains("#EXT-X-ENDLIST")
        {
            break;
        }
        assert!(Instant::now() < deadline, "HLS playlist did not end");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let generation_id = state.read().generation_id;
    let key = generation_registry_key(APP, STREAM, &generation_id.to_string());
    assert!(
        server.registry.contains_key(&key),
        "ended HLS generation must remain addressable during final playlist grace"
    );

    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn rtmp_play_late_join_receives_sequence_headers_keyframe_and_live_media() -> Result<()> {
    let server = start_rtmp_hub_server().await?;
    let mut publisher = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    publisher.send_video(0, true).await?;
    publisher.send_audio(0).await?;
    publisher.send_video(1, true).await?;
    publisher.send_audio(1).await?;

    let mut player = RtmpPlayer::connect(server.address, APP, STREAM).await?;
    tokio::time::sleep(Duration::from_millis(50)).await;
    for timestamp in [2_001, 3_001, 4_001, 5_001] {
        publisher.send_video(timestamp, false).await?;
        publisher.send_audio(timestamp + 1).await?;
    }

    let media = player.receive_media(10, Duration::from_secs(3)).await?;
    assert!(media.iter().any(|message| {
        message.media_type == RtmpMediaType::Video && message.payload.get(1) == Some(&0)
    }));
    assert!(media.iter().any(|message| {
        message.media_type == RtmpMediaType::Audio && message.payload.get(1) == Some(&0)
    }));
    assert!(media.iter().any(|message| {
        message.media_type == RtmpMediaType::Video
            && message.payload.first().is_some_and(|flags| flags >> 4 == 1)
            && message.payload.get(1) == Some(&1)
    }));
    assert!(media.iter().any(|message| {
        message.media_type == RtmpMediaType::Video && message.timestamp == 2_001
    }));
    assert!(media.iter().any(|message| {
        message.media_type == RtmpMediaType::Audio && message.timestamp == 2_002
    }));

    publisher.close();
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn real_avc_aac_rtmp_pipeline_is_decodable_through_play_flv_and_hls() -> Result<()> {
    let video = avc_fixture()?;
    let audio = aac_fixture()?;
    let server = start_server().await?;
    let mut publisher = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    let state = wait_for_playlist(&server).await;
    let (flv_rx, flv_task) = start_flv_capture(&server).await?;

    publisher.send_raw_video(0, &video.sequence_header).await?;
    publisher.send_raw_audio(0, &audio.sequence_header).await?;
    publisher
        .send_raw_video(1, &video.access_units[0].flv_tag)
        .await?;
    publisher.send_raw_audio(1, &audio.access_units[0]).await?;

    let mut player = RtmpPlayer::connect(server.address, APP, STREAM).await?;
    let player_task =
        tokio::spawn(async move { player.receive_media(8, Duration::from_secs(3)).await });
    for (index, timestamp) in [5_001_u32, 10_001, 12_001].into_iter().enumerate() {
        publisher
            .send_raw_video(timestamp, &video.access_units[index + 1].flv_tag)
            .await?;
        publisher
            .send_raw_audio(timestamp, &audio.access_units[index + 1])
            .await?;
    }
    let played = player_task.await??;
    anyhow::ensure!(played.iter().any(|message| {
        message.media_type == RtmpMediaType::Video && message.payload == video.sequence_header
    }));
    anyhow::ensure!(played.iter().any(|message| {
        message.media_type == RtmpMediaType::Audio && message.payload == audio.sequence_header
    }));
    anyhow::ensure!(played.iter().any(|message| {
        message.media_type == RtmpMediaType::Video
            && message.payload == video.access_units[0].flv_tag
    }));
    publisher.close();

    let segments = wait_for_ended_segments(&server, &state).await?;
    let mut hls_video = Vec::new();
    let mut hls_audio = Vec::new();
    for segment in &segments {
        let inspection = inspect_ts(segment)?;
        assert_ts_tracks(&inspection, &[(0x101, 0x1b, 0xe0), (0x102, 0x0f, 0xc0)]);
        hls_video.extend_from_slice(&extract_ts_elementary_stream(segment, 0x101)?);
        hls_audio.extend_from_slice(&extract_ts_elementary_stream(segment, 0x102)?);
    }
    let decoded_video = AvcDecoder::new().decode_stream(&hls_video)?;
    anyhow::ensure!(
        decoded_video.len() >= 4,
        "HLS AVC decoded {} frames",
        decoded_video.len()
    );
    anyhow::ensure!(decoded_video
        .iter()
        .all(|frame| frame.width == 32 && frame.height == 32));
    let (audio_samples, non_silent) = decode_adts_stream(&hls_audio)?;
    anyhow::ensure!(audio_samples > 0 && non_silent, "HLS AAC decoded no audio");

    let flv_tags = finish_flv_capture(flv_rx, flv_task).await?;
    assert_decodable_avc_aac_flv(&flv_tags, &video, &audio)?;

    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn ffmpeg_libavformat_rtmp_avc_aac_is_decodable_through_flv_and_hls() -> Result<()> {
    let video = avc_fixture()?;
    let audio = aac_fixture()?;
    let input = ffmpeg_fixture_file(&video, Some(&audio))?;
    let server = start_server().await?;
    let url = format!("rtmp://{}/{APP}/{STREAM}", server.address);
    let input_path = input.path().to_path_buf();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let publisher = tokio::task::spawn_blocking(move || {
        ffmpeg_publish(&input_path, &url, "flv", ready_tx, release_rx)
    });
    ready_rx
        .await
        .context("FFmpeg RTMP publisher closed before writing its header")?;

    let state = wait_for_playlist(&server).await;
    let (flv_rx, flv_task) = start_flv_capture(&server).await?;
    let _ = release_tx.send(());
    publisher.await??;

    let segments = wait_for_ended_segments(&server, &state).await?;
    let mut hls_video = Vec::new();
    let mut hls_audio = Vec::new();
    for segment in &segments {
        let inspection = inspect_ts(segment)?;
        assert_ts_tracks(&inspection, &[(0x101, 0x1b, 0xe0), (0x102, 0x0f, 0xc0)]);
        hls_video.extend_from_slice(&extract_ts_elementary_stream(segment, 0x101)?);
        hls_audio.extend_from_slice(&extract_ts_elementary_stream(segment, 0x102)?);
    }
    anyhow::ensure!(
        AvcDecoder::new().decode_stream(&hls_video)?.len() >= 4,
        "FFmpeg RTMP HLS AVC decoded too few frames"
    );
    let (audio_samples, non_silent) = decode_adts_stream(&hls_audio)?;
    anyhow::ensure!(
        audio_samples > 0 && non_silent,
        "FFmpeg RTMP HLS AAC decoded no audio"
    );

    let flv_tags = finish_flv_capture(flv_rx, flv_task).await?;
    assert_decodable_avc_aac_flv(&flv_tags, &video, &audio)?;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn ffmpeg_libavformat_rtmp_hevc_is_decodable_through_flv_and_hls() -> Result<()> {
    let video = hevc_fixture()?;
    let input = ffmpeg_fixture_file(&video, None)?;
    let server = start_server().await?;
    let url = format!("rtmp://{}/{APP}/{STREAM}", server.address);
    let input_path = input.path().to_path_buf();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let publisher = tokio::task::spawn_blocking(move || {
        ffmpeg_publish(&input_path, &url, "flv", ready_tx, release_rx)
    });
    ready_rx
        .await
        .context("FFmpeg HEVC RTMP publisher closed before writing its header")?;

    let state = wait_for_playlist(&server).await;
    let (flv_rx, flv_task) = start_flv_capture(&server).await?;
    let _ = release_tx.send(());
    publisher.await??;

    let segments = wait_for_ended_segments(&server, &state).await?;
    let mut hls_video = Vec::new();
    for segment in &segments {
        let inspection = inspect_ts(segment)?;
        assert_ts_tracks(&inspection, &[(0x101, 0x24, 0xe0)]);
        hls_video.extend_from_slice(&extract_ts_elementary_stream(segment, 0x101)?);
    }
    assert_decodable_hevc(&hls_video, 4)?;

    let flv_tags = finish_flv_capture(flv_rx, flv_task).await?;
    let mut flv_video = video.decoder_config_annex_b.clone();
    let mut frames = 0;
    for tag in flv_tags {
        if tag.first() != Some(&9) {
            continue;
        }
        let body = flv_tag_body(&tag, 9)?;
        if body.get(1) == Some(&1) {
            flv_video.extend_from_slice(&avcc_payload_to_annex_b(body)?);
            frames += 1;
        }
    }
    anyhow::ensure!(frames >= 4, "FFmpeg RTMP FLV delivered too few HEVC frames");
    assert_decodable_hevc(&flv_video, 4)?;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn real_hevc_rtmp_pipeline_is_decodable_through_play_flv_and_hls() -> Result<()> {
    let video = hevc_fixture()?;
    let server = start_server().await?;
    let mut publisher = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    let state = wait_for_playlist(&server).await;
    let (flv_rx, flv_task) = start_flv_capture(&server).await?;

    publisher.send_raw_video(0, &video.sequence_header).await?;
    publisher
        .send_raw_video(1, &video.access_units[0].flv_tag)
        .await?;
    let mut player = RtmpPlayer::connect(server.address, APP, STREAM).await?;
    let player_task =
        tokio::spawn(async move { player.receive_media(4, Duration::from_secs(3)).await });
    for (timestamp, unit) in [
        (5_001_u32, &video.access_units[1]),
        (10_001, &video.access_units[0]),
        (12_001, &video.access_units[1]),
    ] {
        publisher.send_raw_video(timestamp, &unit.flv_tag).await?;
    }
    let played = player_task.await??;
    anyhow::ensure!(played
        .iter()
        .any(|message| message.payload == video.sequence_header));
    anyhow::ensure!(played
        .iter()
        .any(|message| message.payload == video.access_units[0].flv_tag));
    publisher.close();

    let segments = wait_for_ended_segments(&server, &state).await?;
    let mut hls_video = Vec::new();
    for segment in &segments {
        let inspection = inspect_ts(segment)?;
        assert_ts_tracks(&inspection, &[(0x101, 0x24, 0xe0)]);
        hls_video.extend_from_slice(&extract_ts_elementary_stream(segment, 0x101)?);
    }
    assert_decodable_hevc(&hls_video, 4)?;

    let flv_tags = finish_flv_capture(flv_rx, flv_task).await?;
    let mut flv_video = video.decoder_config_annex_b.clone();
    let mut frame_count = 0;
    for tag in flv_tags {
        if tag.first() != Some(&9) {
            continue;
        }
        let body = flv_tag_body(&tag, 9)?;
        if body.get(1) == Some(&0) {
            anyhow::ensure!(body == video.sequence_header);
        } else if body.get(1) == Some(&1) {
            flv_video.extend_from_slice(&avcc_payload_to_annex_b(body)?);
            frame_count += 1;
        }
    }
    anyhow::ensure!(frame_count >= 4);
    assert_decodable_hevc(&flv_video, 4)?;

    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn rtmp_hls_flv_pipeline_runs_on_file_storage() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let server = start_server_with_storage(Arc::new(FileStorage::new(directory.path()))).await?;
    exercise_rtmp_hls_flv_pipeline(server).await
}

async fn exercise_rtmp_hls_flv_pipeline(server: TestServer) -> Result<()> {
    let mut publisher = RtmpPublisher::connect(server.address, APP, STREAM).await?;

    // The first timestamp carries the AVC decoder configuration record.  The
    // sequence then spans seven ten-second GOPs, enough to roll the six-entry
    // playlist window while the storage backend keeps every TS object.
    publisher.send_video(0, true).await?;
    publisher.send_audio(0).await?;
    publisher.send_video(1, true).await?;

    let (flv_tx, mut flv_rx) = mpsc::channel(32);
    let flv_session = HttpFlvSession::new(
        APP.to_string(),
        STREAM.to_string(),
        server.event_sender.clone(),
        flv_tx,
    );
    let (flv_tx_2, mut flv_rx_2) = mpsc::channel(32);
    let flv_session_2 = HttpFlvSession::new(
        APP.to_string(),
        STREAM.to_string(),
        server.event_sender.clone(),
        flv_tx_2,
    );
    let state = wait_for_playlist(&server).await;
    let flv_task = tokio::spawn(async move {
        let mut session = flv_session;
        let _ = session.run().await;
    });
    let flv_task_2 = tokio::spawn(async move {
        let mut session = flv_session_2;
        let _ = session.run().await;
    });

    let mut first_segment_name = None;
    for segment in 0..7 {
        let start = segment * 10_000 + 1;
        if segment > 0 {
            publisher.send_video(start, true).await?;
            if segment == 1 {
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    if let Some(name) = state
                        .read()
                        .playlist
                        .segments
                        .front()
                        .map(|segment| segment.ts_name.clone())
                    {
                        first_segment_name = Some(name);
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "first HLS segment was not written"
                    );
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
        for offset in (1..10).map(|second| second * 1_000) {
            publisher.send_video(start + offset, false).await?;
        }
        publisher.send_audio(start + 9_000).await?;
    }
    publisher.close();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let playlist = state.read().generate_m3u8(|name| format!("/hls/{name}"));
        if playlist.contains("#EXT-X-ENDLIST") {
            assert!(
                playlist.contains("#EXT-X-MEDIA-SEQUENCE:1"),
                "unexpected playlist:\n{playlist}"
            );
            assert!(!playlist.contains("#EXT-X-MEDIA-SEQUENCE:0"));
            break;
        }
        assert!(Instant::now() < deadline, "HLS playlist did not finish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let segment_names: Vec<String> = state
        .read()
        .playlist
        .segments
        .iter()
        .map(|segment| segment.ts_name.clone())
        .collect();
    assert_eq!(segment_names.len(), 6);
    let mut previous_pts = BTreeMap::new();
    for name in &segment_names {
        let segment = server.storage.read(APP, STREAM, name).await?;
        let inspection = inspect_ts(&segment)?;
        assert_ts_tracks(&inspection, &[(0x101, 0x1b, 0xe0), (0x102, 0x0f, 0xc0)]);
        assert_eq!(inspection.pmt_version, 2);
        assert_eq!(inspection.pcr_pid, 0x101);
        assert!(inspection.pes[&0x101].random_access_seen);
        assert_pts_continue(&mut previous_pts, &inspection);
    }
    assert!(
        server
            .storage
            .exists(APP, STREAM, first_segment_name.as_deref().unwrap())
            .await?
    );
    assert!(server.storage.count_stream_segments(APP, STREAM).await? >= 7);

    let flv_chunk = tokio::time::timeout(Duration::from_secs(2), flv_rx.recv())
        .await
        .context("HTTP-FLV session did not produce a response")?
        .context("HTTP-FLV response channel closed")??;
    assert!(flv_chunk.starts_with(b"FLV"));
    assert_eq!(flv_chunk.get(4).copied().unwrap_or_default() & 0x05, 0x05);
    for _ in 0..2 {
        let media_chunk = tokio::time::timeout(Duration::from_secs(2), flv_rx.recv())
            .await
            .context("HTTP-FLV media tag was not produced")?
            .context("HTTP-FLV response channel closed before media")??;
        assert!(matches!(media_chunk.first(), Some(8 | 9 | 18)));
    }
    let flv_chunk_2 = tokio::time::timeout(Duration::from_secs(2), flv_rx_2.recv())
        .await
        .context("second HTTP-FLV session did not produce a response")?
        .context("second HTTP-FLV response channel closed")??;
    assert!(flv_chunk_2.starts_with(b"FLV"));
    assert_eq!(flv_chunk_2.get(4).copied().unwrap_or_default() & 0x05, 0x05);
    flv_task.abort();
    flv_task_2.abort();
    let _ = flv_task.await;
    let _ = flv_task_2.await;
    server.stop().await;
    Ok(())
}

#[cfg(all(feature = "s3", any(feature = "tls-aws-lc", feature = "tls-ring")))]
#[tokio::test]
#[ignore = "requires Docker and starts the shared RustFS testcontainer"]
async fn rtmp_hls_flv_pipeline_runs_on_real_rustfs_object_storage() -> Result<()> {
    use synctv_core_testing::{start_rustfs, test_rustfs_base_path};
    use synctv_xiu::storage::{S3Config, S3Storage};

    let (_rustfs, s3) = start_rustfs().await;
    let storage = Arc::new(S3Storage::new(S3Config {
        endpoint: s3.endpoint,
        access_key_id: s3.access_key_id,
        secret_access_key: s3.secret_access_key,
        bucket: s3.bucket,
        region: Some(s3.region),
        // Deliberately omit the trailing slash to exercise constructor normalization.
        base_path: test_rustfs_base_path("xiu-streaming-e2e"),
        public_url_prefix: String::new(),
        presign_expires_in: 60,
    })?);

    let server = start_server_with_storage(storage).await?;
    exercise_rtmp_hls_flv_pipeline(server).await
}

async fn exercise_hls_retention_lifecycle(storage: Arc<dyn HlsStorage>) -> Result<()> {
    const RETENTION: Duration = Duration::from_secs(1);
    const CLIENT_REFRESH_GRACE: Duration = Duration::from_millis(100);

    let server = start_server_with_storage_and_cleanup(
        storage,
        CleanupConfig {
            interval: Duration::from_secs(3600),
            retention: RETENTION,
            final_playlist_grace: Duration::ZERO,
            ended_segment_grace: Duration::ZERO,
            max_segments_per_stream: 0,
        },
    )
    .await?;
    let mut publisher = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    let state = wait_for_playlist_generation(&server, None).await;
    let first_segment = send_hls_gops(&mut publisher, &state, 8)
        .await?
        .context("eight GOPs did not produce a first HLS segment")?;
    publisher.close();
    wait_for_generation_end(&state).await;

    let public_segments = state
        .read()
        .playlist
        .segments
        .iter()
        .map(|segment| segment.ts_name.clone())
        .collect::<Vec<_>>();
    assert_eq!(public_segments.len(), 6);
    assert!(!public_segments.contains(&first_segment));
    assert!(server.storage.exists(APP, STREAM, &first_segment).await?);

    tokio::time::sleep(CLIENT_REFRESH_GRACE).await;
    let stale_segment = server.storage.read(APP, STREAM, &first_segment).await?;
    assert_eq!(stale_segment.first(), Some(&0x47));
    assert_eq!(stale_segment.len() % 188, 0);

    tokio::time::sleep(RETENTION + Duration::from_millis(100)).await;
    let deleted = server.segment_manager.cleanup_expired().await?;
    assert!(deleted >= 8, "retention cleanup deleted {deleted} segments");
    assert!(!server.storage.exists(APP, STREAM, &first_segment).await?);
    assert_eq!(server.storage.count_stream_segments(APP, STREAM).await?, 0);

    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn memory_hls_retains_slid_segment_for_refresh_then_expires_it() -> Result<()> {
    exercise_hls_retention_lifecycle(Arc::new(MemoryStorage::unlimited())).await
}

#[tokio::test]
async fn file_hls_retains_slid_segment_for_refresh_then_expires_it() -> Result<()> {
    let directory = tempfile::tempdir()?;
    exercise_hls_retention_lifecycle(Arc::new(FileStorage::new(directory.path()))).await
}

async fn exercise_same_key_generation_cleanup(storage: Arc<dyn HlsStorage>) -> Result<()> {
    let server = start_server_with_storage_and_cleanup(
        storage,
        CleanupConfig {
            interval: Duration::from_millis(25),
            retention: Duration::from_secs(3600),
            final_playlist_grace: Duration::from_millis(250),
            ended_segment_grace: Duration::from_millis(500),
            max_segments_per_stream: 0,
        },
    )
    .await?;

    let mut first_publisher = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    let first_state = wait_for_playlist_generation(&server, None).await;
    send_hls_gops(&mut first_publisher, &first_state, 2).await?;
    first_publisher.close();
    wait_for_generation_end(&first_state).await;

    let (first_generation_id, old_segments) = {
        let state = first_state.read();
        (state.generation_id, state.cleanup_segment_names.clone())
    };
    assert!(!old_segments.is_empty());
    for segment in &old_segments {
        assert!(server.storage.exists(APP, STREAM, segment).await?);
    }

    let mut replacement = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    let replacement_state = wait_for_playlist_generation(&server, Some(first_generation_id)).await;
    send_hls_gops(&mut replacement, &replacement_state, 2).await?;
    let replacement_generation_id = replacement_state.read().generation_id;
    let new_segments = replacement_state
        .read()
        .playlist
        .segments
        .iter()
        .map(|segment| segment.ts_name.clone())
        .collect::<Vec<_>>();
    assert!(!new_segments.is_empty());

    let cleanup_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let mut old_generation_exists = false;
        for segment in &old_segments {
            old_generation_exists |= server.storage.exists(APP, STREAM, segment).await?;
        }
        for segment in &new_segments {
            assert!(server.storage.exists(APP, STREAM, segment).await?);
        }
        let registry_owner = server
            .registry
            .get(&synctv_xiu::hls::generation_registry_key(
                APP,
                STREAM,
                &replacement_generation_id.to_string(),
            ))
            .context("replacement HLS generation disappeared during old cleanup")?
            .read()
            .generation_id;
        assert_eq!(registry_owner, replacement_generation_id);
        if !old_generation_exists {
            break;
        }
        assert!(
            Instant::now() < cleanup_deadline,
            "scheduled old generation cleanup did not run"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let previous_segment_count = replacement_state.read().playlist.segments.len();
    replacement.send_video(20_001, true).await?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if replacement_state.read().playlist.segments.len() > previous_segment_count {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "replacement generation stopped remuxing after old cleanup"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    replacement.close();
    wait_for_generation_end(&replacement_state).await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn memory_old_generation_cleanup_preserves_same_key_republish() -> Result<()> {
    exercise_same_key_generation_cleanup(Arc::new(MemoryStorage::unlimited())).await
}

#[tokio::test]
async fn file_old_generation_cleanup_preserves_same_key_republish() -> Result<()> {
    let directory = tempfile::tempdir()?;
    exercise_same_key_generation_cleanup(Arc::new(FileStorage::new(directory.path()))).await
}

#[tokio::test]
async fn file_storage_persists_and_cleans_real_ts_objects() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let storage = FileStorage::new(directory.path());
    let name = format!("{}_test-segment", chrono::Utc::now().timestamp() / 60);
    storage
        .write(APP, STREAM, &name, Bytes::from_static(b"transport-stream"))
        .await?;
    assert!(storage.exists(APP, STREAM, &name).await?);
    assert_eq!(
        storage.read(APP, STREAM, &name).await?,
        Bytes::from_static(b"transport-stream")
    );
    assert_eq!(storage.delete_app_stream(APP, STREAM).await?, 1);
    assert!(!storage.exists(APP, STREAM, &name).await?);
    Ok(())
}

#[tokio::test]
async fn interleaved_audio_does_not_corrupt_video_format3_extended_timestamp() -> Result<()> {
    let server = start_server().await?;
    let mut publisher = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    let mut frames = subscribe_frames(&server).await?;

    publisher.send_video(0, true).await?;
    publisher.send_video(0x0100_0000, true).await?;
    publisher.send_audio(1).await?;
    publisher.send_video(0x0200_0000, true).await?;

    let expected = avc_test_tag(0x0200_0000, true).freeze();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let frame = tokio::time::timeout(Duration::from_millis(500), frames.recv())
            .await
            .context("timed out waiting for interleaved RTMP frame")?
            .context("direct RTMP frame subscription closed")?;
        if let synctv_xiu::streamhub::define::FrameData::Video { timestamp, data } = frame {
            if timestamp == 0x0200_0000 {
                assert_eq!(data, expected);
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "format-3 extended timestamp frame was not delivered"
        );
    }

    publisher.close();
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn malformed_avc_publisher_is_reclaimed_and_same_key_can_republish() -> Result<()> {
    let server = start_server().await?;
    let mut malformed = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    let nalu_without_sequence_header = [0x17, 0x01, 0, 0, 0, 0, 0, 0, 4, 0x65, 0xde, 0xad, 0xbe];
    malformed
        .send_raw_video(0, &nalu_without_sequence_header)
        .await?;
    // The first frame closes the transceiver; the next write makes the RTMP
    // session observe the closed publisher channel and run its teardown path.
    let _ = malformed.send_raw_video(1, &[0x17]).await;
    malformed.close();

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match subscribe_frames(&server).await {
            Ok(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            Err(_) => break,
        }
        assert!(
            Instant::now() < deadline,
            "malformed publisher left a zombie StreamHub key"
        );
    }

    let mut replacement = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    let mut frames = subscribe_frames(&server).await?;
    replacement.send_video(0, true).await?;
    replacement.send_video(1, true).await?;
    let delivered = tokio::time::timeout(Duration::from_secs(2), frames.recv())
        .await
        .context("replacement publisher did not deliver media")?
        .context("replacement publisher frame channel closed")?;
    assert!(matches!(
        delivered,
        synctv_xiu::streamhub::define::FrameData::Video { .. }
    ));

    replacement.close();
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn malformed_aac_publisher_runs_cleanup_and_same_key_can_republish() -> Result<()> {
    let unpublish_count = Arc::new(AtomicUsize::new(0));
    let auth: Arc<dyn synctv_xiu::rtmp::auth::AuthCallback> = Arc::new(FixedModeAuth {
        mode: synctv_xiu::rtmp::auth::RtmpStreamMode::Default,
        unpublish_count: Arc::clone(&unpublish_count),
    });
    let server = start_server_with_auth(auth).await?;
    let mut malformed = RtmpPublisher::connect(server.address, APP, STREAM).await?;

    // AAC requires a second FLV audio-header byte. The first frame terminates
    // the stream transceiver; the next frame makes the RTMP session observe
    // the closed publisher channel and execute its lifecycle teardown.
    malformed.send_raw_audio(0, &[0xaf]).await?;
    tokio::time::sleep(Duration::from_millis(30)).await;
    let _ = malformed.send_raw_audio(1, &[0xaf]).await;
    malformed.close();

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let stream_absent = subscribe_frames(&server).await.is_err();
        if stream_absent && unpublish_count.load(Ordering::SeqCst) == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "malformed AAC publisher did not complete StreamHub and auth cleanup"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let mut replacement = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    let mut frames = subscribe_frames(&server).await?;
    replacement.send_audio(0).await?;
    replacement.send_audio(1).await?;
    let delivered = tokio::time::timeout(Duration::from_secs(2), frames.recv())
        .await
        .context("replacement AAC publisher did not deliver media")?
        .context("replacement AAC publisher frame channel closed")?;
    assert!(matches!(
        delivered,
        synctv_xiu::streamhub::define::FrameData::Audio { .. }
    ));

    replacement.close();
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn abrupt_rtmp_disconnect_finishes_hls_and_allows_fenced_republish() -> Result<()> {
    let server = start_server().await?;
    let mut first = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    let first_state = wait_for_playlist_generation(&server, None).await;
    let mut first_frames = subscribe_frames(&server).await?;
    first.send_video(0, true).await?;
    first.send_audio(0).await?;
    first.send_video(1, true).await?;
    first.send_audio(1).await?;
    first.send_video(5_001, false).await?;
    first.send_audio(5_001).await?;

    // Dropping the network publisher exercises the same teardown path as a
    // client process being killed while HLS and direct subscribers are active.
    first.abort().await?;
    wait_for_generation_end(&first_state).await;
    assert!(first_state
        .read()
        .generate_m3u8(|name| format!("/hls/{name}.ts"))
        .contains("#EXT-X-ENDLIST"));

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let next = tokio::time::timeout(remaining, first_frames.recv())
            .await
            .context("publisher subscriber did not close after disconnect")?;
        if next.is_none() {
            break;
        }
    }
    let mut replacement = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    let first_generation_id = first_state.read().generation_id;
    let replacement_state = wait_for_playlist_generation(&server, Some(first_generation_id)).await;
    let mut replacement_frames = subscribe_frames(&server).await?;
    let marker = [0x17, 0x01, 0, 0, 0, 0, 0, 0, 4, 0x65, 0xfa, 0xce, 0xb0];
    replacement
        .send_raw_video(0, &avc_test_tag(0, true))
        .await?;
    replacement.send_raw_video(1, &marker).await?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let frame = tokio::time::timeout(remaining, replacement_frames.recv())
            .await
            .context("replacement publisher media timed out")?
            .context("replacement publisher subscriber closed")?;
        if let synctv_xiu::streamhub::define::FrameData::Video { data, .. } = frame {
            if data.windows(3).any(|window| window == [0xfa, 0xce, 0xb0]) {
                break;
            }
        }
    }
    assert_ne!(replacement_state.read().generation_id, first_generation_id);

    replacement.close();
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn duplicate_rtmp_publisher_cannot_replace_active_stream() -> Result<()> {
    let server = start_server().await?;
    let mut first = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    let mut frames = subscribe_frames(&server).await?;
    first.send_video(0, true).await?;
    first.send_video(1, true).await?;

    let mut duplicate = RtmpPublisher::connect_unconfirmed(server.address, APP, STREAM).await?;
    let duplicate_marker = [0x17, 0x01, 0, 0, 0, 0, 0, 0, 4, 0x65, 0xaa, 0xbb, 0xcc];
    let _ = duplicate.send_raw_video(2, &duplicate_marker).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let first_marker = [0x17, 0x01, 0, 0, 0, 0, 0, 0, 4, 0x65, 0xde, 0xad, 0xbe];
    first.send_raw_video(3, &first_marker).await?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut received_first_marker = false;
    loop {
        let frame = tokio::time::timeout(Duration::from_millis(500), frames.recv())
            .await
            .context("active publisher subscriber timed out")?
            .context("active publisher subscriber closed")?;
        if let synctv_xiu::streamhub::define::FrameData::Video { data, .. } = frame {
            assert!(
                !data.windows(3).any(|window| window == [0xaa, 0xbb, 0xcc]),
                "duplicate publisher media reached active stream subscribers"
            );
            received_first_marker |= data.windows(3).any(|window| window == [0xde, 0xad, 0xbe]);
        }
        if received_first_marker {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "active publisher stopped delivering after duplicate publish"
        );
    }

    duplicate.close();
    first.close();
    server.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_publish_releases_session_and_same_key_can_publish_next() -> Result<()> {
    let publish_attempts = Arc::new(AtomicUsize::new(0));
    let rollback_count = Arc::new(AtomicUsize::new(0));
    let unpublish_count = Arc::new(AtomicUsize::new(0));
    let auth: Arc<dyn synctv_xiu::rtmp::auth::AuthCallback> = Arc::new(RejectFirstPublishAuth {
        publish_attempts: Arc::clone(&publish_attempts),
        rollback_count: Arc::clone(&rollback_count),
        unpublish_count: Arc::clone(&unpublish_count),
    });
    let server = start_rtmp_hub_server_with_auth(Some(auth)).await?;

    let rejected = RtmpPublisher::connect_unconfirmed(server.address, APP, STREAM).await?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while publish_attempts.load(Ordering::SeqCst) < 1 {
        anyhow::ensure!(
            Instant::now() < deadline,
            "authentication callback did not receive the rejected publish"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    rejected.close();
    assert!(subscribe_frames(&server).await.is_err());
    assert_eq!(rollback_count.load(Ordering::SeqCst), 0);
    assert_eq!(unpublish_count.load(Ordering::SeqCst), 0);

    let mut accepted = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    let mut frames = subscribe_frames(&server).await?;
    let marker = [0x17, 0x01, 0, 0, 0, 0, 0, 0, 4, 0x65, 0xde, 0xad, 0xbe];
    accepted.send_raw_video(1, &marker).await?;
    let frame = tokio::time::timeout(Duration::from_secs(2), frames.recv())
        .await
        .context("accepted publisher did not deliver media")?
        .context("accepted publisher channel closed")?;
    let synctv_xiu::streamhub::define::FrameData::Video { data, .. } = frame else {
        anyhow::bail!("accepted publisher delivered a non-video frame")
    };
    assert!(data.windows(3).any(|window| window == [0xde, 0xad, 0xbe]));
    assert_eq!(publish_attempts.load(Ordering::SeqCst), 2);

    accepted.close();
    server.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_distinct_rtmp_stream_keys_keep_media_isolated() -> Result<()> {
    const SECOND_STREAM: &str = "e2e-secondary";
    const FIRST_MARKER: [u8; 13] = [0x17, 0x01, 0, 0, 0, 0, 0, 0, 4, 0x65, 0x11, 0x22, 0x33];
    const SECOND_MARKER: [u8; 13] = [0x17, 0x01, 0, 0, 0, 0, 0, 0, 4, 0x65, 0xaa, 0xbb, 0xcc];

    let server = start_rtmp_hub_server().await?;
    let mut first = tokio::time::timeout(
        Duration::from_secs(5),
        RtmpPublisher::connect(server.address, APP, STREAM),
    )
    .await
    .context("first RTMP source connection timed out")??;
    let mut second = tokio::time::timeout(
        Duration::from_secs(5),
        RtmpPublisher::connect(server.address, APP, SECOND_STREAM),
    )
    .await
    .context("second RTMP source connection timed out")??;
    let mut first_frames = tokio::time::timeout(
        Duration::from_secs(5),
        subscribe_stream_frames(&server, STREAM),
    )
    .await
    .context("first RTMP source subscription timed out")??;
    let mut second_frames = tokio::time::timeout(
        Duration::from_secs(5),
        subscribe_stream_frames(&server, SECOND_STREAM),
    )
    .await
    .context("second RTMP source subscription timed out")??;

    tokio::try_join!(
        first.send_raw_video(1, &FIRST_MARKER),
        second.send_raw_video(1, &SECOND_MARKER),
    )?;

    let first_frame = tokio::time::timeout(Duration::from_secs(2), first_frames.recv())
        .await
        .context("first RTMP source did not deliver media")?
        .context("first RTMP source channel closed")?;
    let second_frame = tokio::time::timeout(Duration::from_secs(2), second_frames.recv())
        .await
        .context("second RTMP source did not deliver media")?
        .context("second RTMP source channel closed")?;

    let synctv_xiu::streamhub::define::FrameData::Video {
        data: first_data, ..
    } = first_frame
    else {
        anyhow::bail!("first RTMP source delivered a non-video frame")
    };
    let synctv_xiu::streamhub::define::FrameData::Video {
        data: second_data, ..
    } = second_frame
    else {
        anyhow::bail!("second RTMP source delivered a non-video frame")
    };

    assert!(first_data
        .windows(3)
        .any(|window| window == [0x11, 0x22, 0x33]));
    assert!(!first_data
        .windows(3)
        .any(|window| window == [0xaa, 0xbb, 0xcc]));
    assert!(second_data
        .windows(3)
        .any(|window| window == [0xaa, 0xbb, 0xcc]));
    assert!(!second_data
        .windows(3)
        .any(|window| window == [0x11, 0x22, 0x33]));

    first.close();
    second.close();
    server.stop().await;
    Ok(())
}

async fn exercise_single_track_rtmp_source(video_only: bool) -> Result<()> {
    let server = start_server().await?;
    let mut publisher = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    let state = wait_for_playlist(&server).await;
    let (flv_tx, mut flv_rx) = mpsc::channel(32);
    let mut flv_session = HttpFlvSession::new(
        APP.to_string(),
        STREAM.to_string(),
        server.event_sender.clone(),
        flv_tx,
    );
    flv_session.start().await?;
    let flv_task = tokio::spawn(async move { flv_session.run_after_start().await });

    if video_only {
        publisher.send_video(0, true).await?;
        publisher.send_video(1, true).await?;
        publisher.send_video(5_001, false).await?;
        publisher.send_video(10_001, true).await?;
        publisher.send_video(12_001, false).await?;
    } else {
        publisher.send_audio(0).await?;
        publisher.send_audio(1).await?;
        publisher.send_audio(5_001).await?;
        publisher.send_audio(10_001).await?;
        publisher.send_audio(12_001).await?;
    }
    publisher.close();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let playlist = state.read().generate_m3u8(|name| format!("/hls/{name}"));
        if playlist.contains("#EXT-X-ENDLIST") {
            let names = state
                .read()
                .playlist
                .segments
                .iter()
                .map(|segment| segment.ts_name.clone())
                .collect::<Vec<_>>();
            assert!(!names.is_empty());
            let mut previous_pts = BTreeMap::new();
            for name in names {
                let segment = server.storage.read(APP, STREAM, &name).await?;
                let inspection = inspect_ts(&segment)?;
                let expected = if video_only {
                    [(0x101, 0x1b, 0xe0)]
                } else {
                    [(0x101, 0x0f, 0xc0)]
                };
                assert_ts_tracks(&inspection, &expected);
                assert_eq!(inspection.pmt_version, 1);
                assert_eq!(inspection.pcr_pid, 0x101);
                if video_only {
                    assert!(inspection.pes[&0x101].random_access_seen);
                }
                assert_pts_continue(&mut previous_pts, &inspection);
            }
            break;
        }
        assert!(
            Instant::now() < deadline,
            "single-track HLS playlist did not end"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let task_result = tokio::time::timeout(Duration::from_secs(2), flv_task)
        .await
        .context("single-track HTTP-FLV task did not end")??;
    task_result?;
    let header = flv_rx
        .recv()
        .await
        .context("single-track HTTP-FLV header missing")??;
    assert!(header.starts_with(b"FLV"));
    let expected_flag = if video_only { 0x01 } else { 0x04 };
    let expected_tag = if video_only { 9 } else { 8 };
    assert_eq!(header[4] & 0x05, expected_flag);
    while let Some(tag) = flv_rx.recv().await {
        assert_eq!(tag?.first(), Some(&expected_tag));
    }

    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn rtmp_video_only_generates_video_flv_and_hls() -> Result<()> {
    exercise_single_track_rtmp_source(true).await
}

#[tokio::test]
async fn rtmp_audio_only_generates_audio_flv_and_hls() -> Result<()> {
    exercise_single_track_rtmp_source(false).await
}

async fn exercise_auth_filtered_rtmp_source(
    mode: synctv_xiu::rtmp::auth::RtmpStreamMode,
) -> Result<()> {
    let unpublish_count = Arc::new(AtomicUsize::new(0));
    let auth: Arc<dyn synctv_xiu::rtmp::auth::AuthCallback> = Arc::new(FixedModeAuth {
        mode,
        unpublish_count: Arc::clone(&unpublish_count),
    });
    let server = start_server_with_auth(auth).await?;
    let mut publisher = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    let state = wait_for_playlist(&server).await;
    let mut direct_frames = subscribe_frames(&server).await?;

    let (flv_tx, mut flv_rx) = mpsc::channel(32);
    let mut flv_session = HttpFlvSession::new(
        APP.to_string(),
        STREAM.to_string(),
        server.event_sender.clone(),
        flv_tx,
    );
    flv_session.start().await?;
    let flv_task = tokio::spawn(async move { flv_session.run_after_start().await });

    publisher.send_video(0, true).await?;
    publisher.send_audio(0).await?;
    publisher.send_video(1, true).await?;
    publisher.send_audio(1).await?;

    let mut rtmp_player = RtmpPlayer::connect(server.address, APP, STREAM).await?;
    let rtmp_play_task =
        tokio::spawn(async move { rtmp_player.receive_media(4, Duration::from_secs(3)).await });
    publisher.send_video(5_001, false).await?;
    publisher.send_audio(5_001).await?;
    publisher.send_video(10_001, true).await?;
    publisher.send_audio(10_001).await?;
    publisher.send_video(12_001, false).await?;
    publisher.send_audio(12_001).await?;
    let rtmp_media = rtmp_play_task.await??;
    publisher.close();

    let expected_video = mode == synctv_xiu::rtmp::auth::RtmpStreamMode::VideoOnly;
    assert!(rtmp_media.iter().all(|message| {
        message.media_type
            == if expected_video {
                RtmpMediaType::Video
            } else {
                RtmpMediaType::Audio
            }
    }));
    let mut audio_frames = 0;
    let mut video_frames = 0;
    loop {
        match tokio::time::timeout(Duration::from_secs(2), direct_frames.recv()).await {
            Ok(Some(synctv_xiu::streamhub::define::FrameData::Audio { .. })) => {
                audio_frames += 1;
            }
            Ok(Some(synctv_xiu::streamhub::define::FrameData::Video { .. })) => {
                video_frames += 1;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(error) => return Err(error).context("filtered direct subscriber did not end"),
        }
    }
    if expected_video {
        assert!(video_frames > 0);
        assert_eq!(audio_frames, 0);
    } else {
        assert!(audio_frames > 0);
        assert_eq!(video_frames, 0);
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let playlist = state.read().generate_m3u8(|name| format!("/hls/{name}"));
        if playlist.contains("#EXT-X-ENDLIST") {
            let segment_names = state
                .read()
                .playlist
                .segments
                .iter()
                .map(|segment| segment.ts_name.clone())
                .collect::<Vec<_>>();
            assert!(!segment_names.is_empty());
            let mut previous_pts = BTreeMap::new();
            for name in segment_names {
                let segment = server.storage.read(APP, STREAM, &name).await?;
                let inspection = inspect_ts(&segment)?;
                let expected = if expected_video {
                    [(0x101, 0x1b, 0xe0)]
                } else {
                    [(0x101, 0x0f, 0xc0)]
                };
                assert_ts_tracks(&inspection, &expected);
                assert_eq!(inspection.pmt_version, 1);
                assert_eq!(inspection.pcr_pid, 0x101);
                if expected_video {
                    assert!(inspection.pes[&0x101].random_access_seen);
                }
                assert_pts_continue(&mut previous_pts, &inspection);
            }
            break;
        }
        assert!(
            Instant::now() < deadline,
            "filtered RTMP HLS playlist did not end"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    tokio::time::timeout(Duration::from_secs(2), flv_task)
        .await
        .context("filtered HTTP-FLV task did not end")???;
    let header = flv_rx
        .recv()
        .await
        .context("filtered HTTP-FLV header missing")??;
    assert!(header.starts_with(b"FLV"));
    let expected_flag = if expected_video { 0x01 } else { 0x04 };
    let expected_tag = if expected_video { 9 } else { 8 };
    assert_eq!(header[4] & 0x05, expected_flag);
    while let Some(tag) = flv_rx.recv().await {
        assert_eq!(tag?.first(), Some(&expected_tag));
    }
    assert_eq!(unpublish_count.load(Ordering::SeqCst), 1);

    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn auth_video_only_mode_filters_audio_from_rtmp_hls_and_flv() -> Result<()> {
    exercise_auth_filtered_rtmp_source(synctv_xiu::rtmp::auth::RtmpStreamMode::VideoOnly).await
}

#[tokio::test]
async fn auth_audio_only_mode_filters_video_from_rtmp_hls_and_flv() -> Result<()> {
    exercise_auth_filtered_rtmp_source(synctv_xiu::rtmp::auth::RtmpStreamMode::AudioOnly).await
}

async fn exercise_late_video_track(
    sequence_header: &[u8],
    keyframe: &[u8],
    expected_stream_type: u8,
) -> Result<()> {
    let server = start_server().await?;
    let mut publisher = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    let state = wait_for_playlist(&server).await;

    publisher.send_audio(0).await?;
    for timestamp in [1, 5_001, 10_001] {
        publisher.send_audio(timestamp).await?;
    }
    publisher.send_raw_video(11_001, sequence_header).await?;
    publisher.send_raw_video(12_001, keyframe).await?;
    publisher.send_audio(12_002).await?;
    publisher.close();

    wait_for_generation_end(&state).await;
    let (playlist, segments) = {
        let state = state.read();
        (
            state.generate_m3u8(|name| format!("/hls/{name}")),
            state.playlist.segments.iter().cloned().collect::<Vec<_>>(),
        )
    };
    assert!(playlist.contains("#EXT-X-ENDLIST"));
    assert!(playlist.contains("#EXT-X-DISCONTINUITY"));
    assert!(
        segments.len() >= 3,
        "late-track stream produced {segments:?}"
    );

    let mut previous_pts = BTreeMap::new();
    for segment in &segments[..segments.len() - 1] {
        assert!(!segment.discontinuity);
        let data = server.storage.read(APP, STREAM, &segment.ts_name).await?;
        let inspection = inspect_ts(&data)?;
        assert_ts_tracks(&inspection, &[(0x101, 0x0f, 0xc0)]);
        assert_eq!(inspection.pmt_version, 1);
        assert_eq!(inspection.pcr_pid, 0x101);
        assert_pts_continue(&mut previous_pts, &inspection);
    }

    let dual_track_segment = segments.last().expect("dual-track segment missing");
    assert!(dual_track_segment.discontinuity);
    let data = server
        .storage
        .read(APP, STREAM, &dual_track_segment.ts_name)
        .await?;
    let inspection = inspect_ts(&data)?;
    assert_ts_tracks(
        &inspection,
        &[(0x101, 0x0f, 0xc0), (0x102, expected_stream_type, 0xe0)],
    );
    assert_eq!(inspection.pmt_version, 2);
    assert_eq!(inspection.pcr_pid, 0x102);
    assert!(inspection.pes[&0x102].random_access_seen);
    assert_pts_continue(&mut previous_pts, &inspection);

    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn late_h264_track_starts_a_discontinuous_dual_track_segment() -> Result<()> {
    exercise_late_video_track(&avc_test_tag(0, true), &avc_test_tag(1, true), 0x1b).await
}

#[tokio::test]
async fn late_hevc_track_preserves_audio_and_writes_nonempty_hevc_segment() -> Result<()> {
    let fixture = hevc_fixture()?;
    exercise_late_video_track(
        &fixture.sequence_header,
        &fixture.access_units[0].flv_tag,
        0x24,
    )
    .await
}

#[tokio::test]
async fn late_audio_track_starts_discontinuity_segment_at_video_keyframe() -> Result<()> {
    let server = start_server().await?;
    let mut publisher = RtmpPublisher::connect(server.address, APP, STREAM).await?;
    let state = wait_for_playlist(&server).await;

    publisher.send_raw_video(0, &avc_test_tag(0, true)).await?;
    tokio::time::sleep(Duration::from_millis(2)).await;
    publisher.send_raw_video(1, &avc_test_tag(1, true)).await?;
    tokio::time::sleep(Duration::from_millis(2)).await;
    for timestamp in [5_001, 10_001, 15_001] {
        publisher
            .send_raw_video(timestamp, &avc_test_tag(timestamp, false))
            .await?;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    // The audio sequence header and raw frame arrive while a video segment is
    // in progress. The muxer must hold the track change until the next IDR.
    publisher.send_raw_audio(15_002, &aac_test_tag(0)).await?;
    tokio::time::sleep(Duration::from_millis(2)).await;
    publisher
        .send_raw_audio(15_003, &aac_test_tag(15_003))
        .await?;
    tokio::time::sleep(Duration::from_millis(2)).await;
    publisher
        .send_raw_video(20_001, &avc_test_tag(20_001, true))
        .await?;
    tokio::time::sleep(Duration::from_millis(2)).await;
    publisher
        .send_raw_audio(20_002, &aac_test_tag(20_002))
        .await?;
    publisher.close();

    wait_for_generation_end(&state).await;
    let segments = state
        .read()
        .playlist
        .segments
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    let discontinuity = segments
        .iter()
        .find(|segment| segment.discontinuity)
        .with_context(|| format!("late-audio discontinuity segment missing: {segments:?}"))?;
    let data = server
        .storage
        .read(APP, STREAM, &discontinuity.ts_name)
        .await?;
    let inspection = inspect_ts(&data)?;
    // Video arrived first, so it owns the first PID; the late AAC track is
    // added to the discontinuity segment afterwards.
    assert_ts_tracks(&inspection, &[(0x101, 0x1b, 0xe0), (0x102, 0x0f, 0xc0)]);
    assert!(
        inspection.pes[&0x101].random_access_seen,
        "late-audio discontinuity segment must begin with an IDR"
    );

    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn slow_http_flv_subscriber_isolated_from_healthy_subscriber() -> Result<()> {
    let server = start_server().await?;
    let mut publisher = RtmpPublisher::connect(server.address, APP, STREAM).await?;

    let (slow_tx, _slow_rx) = mpsc::channel(1);
    let mut slow_session = HttpFlvSession::new(
        APP.to_string(),
        STREAM.to_string(),
        server.event_sender.clone(),
        slow_tx,
    );
    slow_session.start().await?;
    let slow_task = tokio::spawn(async move { slow_session.run_after_start().await });

    let (healthy_tx, mut healthy_rx) = mpsc::channel(512);
    let mut healthy_session = HttpFlvSession::new(
        APP.to_string(),
        STREAM.to_string(),
        server.event_sender.clone(),
        healthy_tx,
    );
    healthy_session.start().await?;
    let healthy_task = tokio::spawn(async move { healthy_session.run_after_start().await });

    publisher.send_video(0, true).await?;
    publisher.send_audio(0).await?;
    publisher.send_video(1, true).await?;
    for frame in 2..340_u32 {
        publisher.send_video(frame * 33, frame % 60 == 0).await?;
    }

    let slow_error = tokio::time::timeout(Duration::from_secs(5), slow_task)
        .await
        .context("slow HTTP-FLV subscriber was not disconnected")??
        .expect_err("slow HTTP-FLV subscriber should exceed its drop threshold");
    assert!(slow_error
        .to_string()
        .contains("Slow subscriber disconnected"));

    let marker = [0x17, 0x01, 0, 0, 0, 0, 0, 0, 4, 0x65, 0xde, 0xad, 0xbe];
    publisher.send_raw_video(12_000, &marker).await?;
    publisher.close();
    tokio::time::timeout(Duration::from_secs(3), healthy_task)
        .await
        .context("healthy HTTP-FLV subscriber did not finish")???;

    let mut received_marker = false;
    while let Some(chunk) = healthy_rx.recv().await {
        let chunk = chunk?;
        received_marker |= chunk
            .windows(4)
            .any(|window| window == [0xde, 0xad, 0xbe, 0]);
        received_marker |= chunk.windows(3).any(|window| window == [0xde, 0xad, 0xbe]);
    }
    assert!(
        received_marker,
        "healthy HTTP-FLV subscriber did not receive the post-disconnect marker frame"
    );

    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn rtsp_audio_only_selection_generates_audio_flv_and_hls() -> Result<()> {
    use synctv_xiu::rtsp::{RtspPullConfig, RtspPullSession, RtspTrackSelection};

    let video = avc_fixture()?;
    let audio = aac_fixture()?;
    let server = start_server().await?;
    let (frame_sender, generation_id) =
        publish_external_frames(&server, "rtsp://audio-only-integration-source/live").await?;
    let state = wait_for_playlist(&server).await;
    let (flv_rx, flv_task) = start_flv_capture(&server).await?;
    let (rtsp_address, release_media, source_task) =
        spawn_interleaved_rtsp_source(&video, &audio).await?;

    let mut config = RtspPullConfig::from_url(&format!("rtsp://{rtsp_address}/live"))?;
    config.video_track = RtspTrackSelection::Disabled;
    config.audio_track = RtspTrackSelection::FirstCompatible;
    let mut session = RtspPullSession::connect(config).await?;
    assert_eq!(session.selected_tracks(), (None, Some(1)));

    let _ = release_media.send(());
    let mut audio_frames = 0_usize;
    while let Some(frame) = session.next_frame().await? {
        anyhow::ensure!(
            matches!(
                &frame,
                synctv_xiu::streamhub::define::FrameData::Audio { .. }
            ),
            "audio-only RTSP selection emitted a non-audio frame"
        );
        audio_frames += 1;
        frame_sender
            .send(frame)
            .await
            .map_err(|_| anyhow::anyhow!("audio-only RTSP publication channel closed"))?;
    }
    anyhow::ensure!(
        audio_frames >= 5,
        "audio-only RTSP delivered too few frames"
    );
    source_task.await??;
    unpublish_test_stream(&server, generation_id).await?;

    let segments = wait_for_ended_segments(&server, &state).await?;
    let mut hls_audio = Vec::new();
    for segment in &segments {
        let inspection = inspect_ts(segment)?;
        assert_ts_tracks(&inspection, &[(0x101, 0x0f, 0xc0)]);
        hls_audio.extend_from_slice(&extract_ts_elementary_stream(segment, 0x101)?);
    }
    let (audio_samples, non_silent) = decode_adts_stream(&hls_audio)?;
    anyhow::ensure!(
        audio_samples > 0 && non_silent,
        "audio-only RTSP HLS decoded no audio"
    );

    let flv_tags = finish_flv_capture(flv_rx, flv_task).await?;
    anyhow::ensure!(!flv_tags.is_empty(), "audio-only RTSP HTTP-FLV had no tags");
    anyhow::ensure!(
        flv_tags.iter().all(|tag| tag.first() == Some(&8)),
        "audio-only RTSP HTTP-FLV contained a non-audio tag"
    );

    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn rtsp_rejects_selection_with_both_media_types_disabled() -> Result<()> {
    use synctv_xiu::rtsp::{RtspPullConfig, RtspPullSession, RtspTrackSelection};

    let video = avc_fixture()?;
    let audio = aac_fixture()?;
    let (rtsp_address, release_media, source_task) =
        spawn_interleaved_rtsp_source(&video, &audio).await?;
    let mut config = RtspPullConfig::from_url(&format!("rtsp://{rtsp_address}/live"))?;
    config.video_track = RtspTrackSelection::Disabled;
    config.audio_track = RtspTrackSelection::Disabled;

    let Err(error) = RtspPullSession::connect(config).await else {
        anyhow::bail!("disabling every RTSP media type accepted the source");
    };
    assert!(
        error
            .to_string()
            .contains("no compatible H.264/H.265 video or AAC audio track"),
        "unexpected disabled-track error: {error:#}"
    );

    drop(release_media);
    source_task.abort();
    let _ = source_task.await;
    Ok(())
}

#[tokio::test]
async fn rtsp_explicit_video_index_rejects_an_audio_track() -> Result<()> {
    use synctv_xiu::rtsp::{RtspPullConfig, RtspPullSession, RtspTrackSelection};

    let video = avc_fixture()?;
    let audio = aac_fixture()?;
    let (rtsp_address, release_media, source_task) =
        spawn_interleaved_rtsp_source(&video, &audio).await?;
    let mut config = RtspPullConfig::from_url(&format!("rtsp://{rtsp_address}/live"))?;
    config.video_track = RtspTrackSelection::Index(1);
    config.audio_track = RtspTrackSelection::Disabled;

    let Err(error) = RtspPullSession::connect(config).await else {
        anyhow::bail!("selecting the AAC track as video accepted the source");
    };
    assert!(
        error.to_string().contains("cannot be used as video"),
        "unexpected explicit-index error: {error:#}"
    );

    drop(release_media);
    source_task.abort();
    let _ = source_task.await;
    Ok(())
}

#[tokio::test]
async fn real_avc_aac_rtsp_rtp_pipeline_is_decodable_through_flv_and_hls() -> Result<()> {
    use synctv_xiu::rtsp::{RtspPullConfig, RtspPullSession};

    let video = avc_fixture()?;
    let audio = aac_fixture()?;
    let server = start_server().await?;
    let (frame_sender, generation_id) =
        publish_external_frames(&server, "rtsp://integration-source/live").await?;
    let state = wait_for_playlist(&server).await;
    let (flv_rx, flv_task) = start_flv_capture(&server).await?;

    let (rtsp_address, release_media, source_task) =
        spawn_interleaved_rtsp_source(&video, &audio).await?;
    let mut session = RtspPullSession::connect(RtspPullConfig::from_url(&format!(
        "rtsp://{rtsp_address}/live"
    ))?)
    .await?;
    let _ = release_media.send(());
    while let Some(frame) = session.next_frame().await? {
        frame_sender
            .send(frame)
            .await
            .map_err(|_| anyhow::anyhow!("RTSP frame publisher channel closed"))?;
    }
    source_task.await??;

    let mut late_frames = subscribe_frames(&server).await?;
    let mut received_sequence_header = false;
    let mut received_audio_sequence_header = false;
    let mut received_keyframe = false;
    for _ in 0..10 {
        let Some(frame) = tokio::time::timeout(Duration::from_secs(1), late_frames.recv()).await?
        else {
            break;
        };
        if let synctv_xiu::streamhub::define::FrameData::Video { data, .. } = frame {
            received_sequence_header |= data.get(1) == Some(&0);
            received_keyframe |=
                data.first().is_some_and(|flags| flags >> 4 == 1) && data.get(1) == Some(&1);
        } else if let synctv_xiu::streamhub::define::FrameData::Audio { data, .. } = frame {
            received_audio_sequence_header |= data.get(1) == Some(&0);
        }
        if received_sequence_header && received_audio_sequence_header && received_keyframe {
            break;
        }
    }
    assert!(received_sequence_header);
    assert!(received_audio_sequence_header);
    assert!(received_keyframe);

    unpublish_test_stream(&server, generation_id).await?;

    let mut hls_video = Vec::new();
    let mut hls_audio = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let playlist = state.read().generate_m3u8(|name| format!("/hls/{name}"));
        if playlist.contains("#EXT-X-ENDLIST") {
            assert!(playlist.contains("#EXT-X-DISCONTINUITY"));
            let segments = state
                .read()
                .playlist
                .segments
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            assert!(segments.len() >= 2);

            let first_data = server
                .storage
                .read(APP, STREAM, &segments[0].ts_name)
                .await?;
            let first = inspect_ts(&first_data)?;
            assert_ts_tracks(&first, &[(0x101, 0x1b, 0xe0)]);
            assert_eq!(first.pmt_version, 1);
            assert_eq!(first.pcr_pid, 0x101);
            assert!(first.pes[&0x101].random_access_seen);
            hls_video.extend_from_slice(&extract_ts_elementary_stream(&first_data, 0x101)?);

            assert!(segments[1].discontinuity);
            let mut previous_pts = BTreeMap::new();
            assert_pts_continue(&mut previous_pts, &first);
            for segment in &segments[1..] {
                let data = server.storage.read(APP, STREAM, &segment.ts_name).await?;
                let inspection = inspect_ts(&data)?;
                assert_ts_tracks(&inspection, &[(0x101, 0x1b, 0xe0), (0x102, 0x0f, 0xc0)]);
                assert_eq!(inspection.pmt_version, 2);
                assert_eq!(inspection.pcr_pid, 0x101);
                assert_pts_continue(&mut previous_pts, &inspection);
                hls_video.extend_from_slice(&extract_ts_elementary_stream(&data, 0x101)?);
                hls_audio.extend_from_slice(&extract_ts_elementary_stream(&data, 0x102)?);
            }
            let final_data = server
                .storage
                .read(
                    APP,
                    STREAM,
                    &segments.last().expect("final segment").ts_name,
                )
                .await?;
            assert!(inspect_ts(&final_data)?.pes[&0x101].random_access_seen);
            break;
        }
        assert!(Instant::now() < deadline, "RTSP HLS playlist did not end");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let decoded_video = AvcDecoder::new().decode_stream(&hls_video)?;
    anyhow::ensure!(
        decoded_video.len() >= 4,
        "RTSP HLS AVC decoded too few frames"
    );
    let (audio_samples, non_silent) = decode_adts_stream(&hls_audio)?;
    anyhow::ensure!(
        audio_samples > 0 && non_silent,
        "RTSP HLS AAC decoded no audio"
    );
    let flv_tags = finish_flv_capture(flv_rx, flv_task).await?;
    assert_decodable_avc_aac_flv(&flv_tags, &video, &audio)?;

    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn real_hevc_rtsp_rtp_pipeline_is_decodable_through_flv_and_hls() -> Result<()> {
    use synctv_xiu::rtsp::{RtspPullConfig, RtspPullSession};

    let fixture = hevc_fixture()?;
    let server = start_server().await?;
    let (frame_sender, generation_id) =
        publish_external_frames(&server, "rtsp://hevc-integration-source/live").await?;
    let state = wait_for_playlist(&server).await;
    let (flv_rx, flv_task) = start_flv_capture(&server).await?;
    let (rtsp_address, release_media, source_task) = spawn_interleaved_hevc_rtsp_source().await?;
    let mut session = RtspPullSession::connect(RtspPullConfig::from_url(&format!(
        "rtsp://{rtsp_address}/live"
    ))?)
    .await?;
    anyhow::ensure!(session.selected_tracks() == (Some(0), None));
    let _ = release_media.send(());
    let mut rtsp_video = fixture.decoder_config_annex_b.clone();
    let mut rtsp_frame_count = 0_usize;
    while let Some(frame) = session.next_frame().await? {
        if let synctv_xiu::streamhub::define::FrameData::Video { data, .. } = &frame {
            if data.get(1) == Some(&1) {
                rtsp_video.extend_from_slice(&avcc_payload_to_annex_b(data)?);
                rtsp_frame_count += 1;
            }
        }
        frame_sender
            .send(frame)
            .await
            .map_err(|_| anyhow::anyhow!("HEVC RTSP frame publication closed"))?;
    }
    anyhow::ensure!(rtsp_frame_count == 2);
    let mut expected_rtsp_video = fixture.decoder_config_annex_b.clone();
    for index in [0_usize, 1] {
        expected_rtsp_video.extend_from_slice(&fixture.access_units[index].annex_b);
    }
    anyhow::ensure!(
        rtsp_video == expected_rtsp_video,
        "Retina changed HEVC access units\nactual={}\nexpected={}",
        hex::encode(&rtsp_video),
        hex::encode(&expected_rtsp_video)
    );
    assert_decodable_hevc(&rtsp_video, 2)
        .map_err(|error| anyhow::anyhow!("Retina HEVC output is invalid: {error:#}"))?;
    source_task.await??;
    unpublish_test_stream(&server, generation_id).await?;

    let segments = wait_for_ended_segments(&server, &state).await?;
    let mut hls_video = Vec::new();
    for segment in &segments {
        let inspection = inspect_ts(segment)?;
        assert_ts_tracks(&inspection, &[(0x101, 0x24, 0xe0)]);
        hls_video.extend_from_slice(&extract_ts_elementary_stream(segment, 0x101)?);
    }
    anyhow::ensure!(
        hls_video == rtsp_video,
        "HLS changed Retina HEVC bytes\nactual={}\nexpected={}",
        hex::encode(&hls_video),
        hex::encode(&rtsp_video)
    );
    assert_decodable_hevc(&hls_video, 2)
        .map_err(|error| anyhow::anyhow!("RTSP HLS HEVC output is invalid: {error:#}"))?;

    let flv_tags = finish_flv_capture(flv_rx, flv_task).await?;
    let mut flv_video = fixture.decoder_config_annex_b;
    let mut frame_count = 0;
    let mut saw_sequence_header = false;
    for tag in flv_tags {
        if tag.first() != Some(&9) {
            continue;
        }
        let body = flv_tag_body(&tag, 9)?;
        anyhow::ensure!(body[0] & 0x0f == 12, "RTSP HEVC changed FLV codec ID");
        if body.get(1) == Some(&0) {
            saw_sequence_header = true;
        } else if body.get(1) == Some(&1) {
            flv_video.extend_from_slice(&avcc_payload_to_annex_b(body)?);
            frame_count += 1;
        }
    }
    anyhow::ensure!(saw_sequence_header && frame_count == 2);
    assert_decodable_hevc(&flv_video, 2)
        .map_err(|error| anyhow::anyhow!("RTSP HTTP-FLV HEVC output is invalid: {error:#}"))?;

    server.stop().await;
    Ok(())
}

async fn exercise_ffmpeg_mediamtx_rtsp_pipeline(
    mediamtx: &synctv_core_testing::ExternalServiceContainer,
    path: &str,
    video: &VideoFixture,
    audio: Option<&AudioFixture>,
) -> Result<()> {
    use synctv_xiu::rtsp::{RtspPullConfig, RtspPullSession, RtspTransport};

    let input = ffmpeg_fixture_file(video, audio)?;
    let server = start_server().await?;
    let (frame_sender, generation_id) =
        publish_external_frames(&server, &format!("rtsp://mediamtx/{path}")).await?;
    let state = wait_for_playlist(&server).await;
    let (flv_rx, flv_task) = start_flv_capture(&server).await?;

    let rtmp_port = mediamtx
        .mapped_port(1935)
        .context("MediaMTX RTMP port was not mapped")?;
    let mediamtx_path = format!("live/{path}");
    let publish_url = format!("rtmp://{}:{rtmp_port}/{mediamtx_path}", mediamtx.host());
    let input_path = input.path().to_path_buf();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let ffmpeg_publisher = tokio::task::spawn_blocking(move || {
        ffmpeg_publish(&input_path, &publish_url, "flv", ready_tx, release_rx)
    });
    ready_rx
        .await
        .context("FFmpeg MediaMTX publisher closed before writing its header")?;

    let api_port = mediamtx
        .mapped_port(9997)
        .context("MediaMTX API port was not mapped")?;
    let api_url = format!("http://{}:{api_port}/v3/paths/list", mediamtx.host());
    let path_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let api_response = match read_http_body(&api_url).await {
            Ok(response) => response,
            Err(error) => format!("probe error: {error:#}"),
        };
        let registered = serde_json::from_str::<serde_json::Value>(&api_response)
            .ok()
            .and_then(|body| {
                body.get("items")
                    .and_then(serde_json::Value::as_array)
                    .cloned()
            })
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("name").and_then(serde_json::Value::as_str)
                        == Some(mediamtx_path.as_str())
                })
            });
        if registered {
            break;
        }
        if Instant::now() >= path_deadline {
            let rtmp_connections = read_http_body(&format!(
                "http://{}:{api_port}/v3/rtmpconns/list",
                mediamtx.host()
            ))
            .await
            .unwrap_or_else(|error| format!("failed to read RTMP connections: {error:#}"));
            let logs = mediamtx
                .logs()
                .await
                .unwrap_or_else(|error| format!("failed to read MediaMTX logs: {error:#}"));
            anyhow::bail!(
                "MediaMTX path {mediamtx_path} was not registered; API response: {api_response}; RTMP connections: {rtmp_connections}; logs:\n{logs}"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let rtsp_url = format!(
        "rtsp://{}:{}/{mediamtx_path}",
        mediamtx.host(),
        mediamtx.port()
    );
    let mut config = RtspPullConfig::from_url(&rtsp_url)?;
    config.transport = RtspTransport::Tcp;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut rtsp = loop {
        match RtspPullSession::connect(config.clone()).await {
            Ok(session) => break session,
            Err(error) if Instant::now() < deadline => {
                tracing::debug!(%error, %rtsp_url, "waiting for MediaMTX RTSP path");
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(error) => return Err(error),
        }
    };
    if audio.is_some() {
        anyhow::ensure!(rtsp.selected_tracks() == (Some(0), Some(1)));
    } else {
        anyhow::ensure!(rtsp.selected_tracks() == (Some(0), None));
    }
    let _ = release_tx.send(());
    let mut rtsp_video_frames = 0_usize;
    let mut rtsp_audio_frames = 0_usize;
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), rtsp.next_frame())
            .await
            .context("MediaMTX RTSP source did not finish")??;
        let Some(frame) = frame else {
            break;
        };
        match &frame {
            synctv_xiu::streamhub::define::FrameData::Video { .. } => rtsp_video_frames += 1,
            synctv_xiu::streamhub::define::FrameData::Audio { .. } => rtsp_audio_frames += 1,
            _ => {}
        }
        frame_sender
            .send(frame)
            .await
            .map_err(|_| anyhow::anyhow!("MediaMTX RTSP publication channel closed"))?;
    }
    anyhow::ensure!(
        rtsp_video_frames >= 5,
        "MediaMTX RTSP delivered only {rtsp_video_frames} video frames for {path}"
    );
    anyhow::ensure!(
        audio.is_none() || rtsp_audio_frames >= 5,
        "MediaMTX RTSP delivered only {rtsp_audio_frames} audio frames for {path}"
    );
    ffmpeg_publisher.await??;
    unpublish_test_stream(&server, generation_id).await?;

    let segments = wait_for_ended_segments(&server, &state).await?;
    let flv_tags = finish_flv_capture(flv_rx, flv_task).await?;
    if let Some(audio) = audio {
        let mut hls_video = Vec::new();
        let mut hls_audio = Vec::new();
        let mut saw_dual_track_segment = false;
        let mut saw_audio_only_segment = false;
        for segment in &segments {
            let inspection = inspect_ts(segment)?;
            anyhow::ensure!(inspection.pmt_pid == 0x100);
            anyhow::ensure!(
                inspection.streams.len() == inspection.pes.len(),
                "MediaMTX HLS PMT/PES mismatch: {:?}",
                inspection.streams
            );
            let video_pid = inspection
                .streams
                .iter()
                .find_map(|(&pid, &stream_type)| (stream_type == 0x1b).then_some(pid));
            let audio_pid = inspection
                .streams
                .iter()
                .find_map(|(&pid, &stream_type)| (stream_type == 0x0f).then_some(pid));
            anyhow::ensure!(
                inspection
                    .streams
                    .values()
                    .all(|stream_type| matches!(stream_type, 0x1b | 0x0f)),
                "MediaMTX HLS produced an unexpected stream type: {:?}",
                inspection.streams
            );
            if let Some(pid) = video_pid {
                anyhow::ensure!(inspection.pes[&pid].stream_id == 0xe0);
                hls_video.extend_from_slice(&extract_ts_elementary_stream(segment, pid)?);
            }
            if let Some(pid) = audio_pid {
                anyhow::ensure!(inspection.pes[&pid].stream_id == 0xc0);
                hls_audio.extend_from_slice(&extract_ts_elementary_stream(segment, pid)?);
            }
            anyhow::ensure!(audio_pid.is_some(), "MediaMTX HLS segment lost AAC");
            saw_dual_track_segment |= video_pid.is_some();
            saw_audio_only_segment |= video_pid.is_none();
            assert!(inspection.streams.contains_key(&inspection.pcr_pid));
            assert_pts_monotonic(&inspection);
        }
        anyhow::ensure!(saw_dual_track_segment, "MediaMTX HLS never added AVC");
        if saw_audio_only_segment {
            let playlist = state.read();
            anyhow::ensure!(
                playlist
                    .playlist
                    .segments
                    .iter()
                    .any(|segment| segment.discontinuity),
                "MediaMTX HLS track-set change lacked a discontinuity"
            );
        }
        anyhow::ensure!(
            AvcDecoder::new().decode_stream(&hls_video)?.len() >= 4,
            "MediaMTX RTSP HLS AVC decoded too few frames"
        );
        let (audio_samples, non_silent) = decode_adts_stream(&hls_audio)?;
        anyhow::ensure!(
            audio_samples > 0 && non_silent,
            "MediaMTX RTSP HLS AAC decoded no audio"
        );
        assert_decodable_avc_aac_flv(&flv_tags, video, audio)?;
    } else {
        let mut hls_video = Vec::new();
        for segment in &segments {
            let inspection = inspect_ts(segment)?;
            assert_ts_tracks(&inspection, &[(0x101, 0x24, 0xe0)]);
            hls_video.extend_from_slice(&extract_ts_elementary_stream(segment, 0x101)?);
        }
        assert_decodable_hevc(&hls_video, 4)?;

        let mut flv_video = video.decoder_config_annex_b.clone();
        let mut frames = 0;
        for tag in flv_tags {
            if tag.first() != Some(&9) {
                continue;
            }
            let body = flv_tag_body(&tag, 9)?;
            if body.get(1) == Some(&1) {
                flv_video.extend_from_slice(&avcc_payload_to_annex_b(body)?);
                frames += 1;
            }
        }
        anyhow::ensure!(
            frames >= 4,
            "MediaMTX RTSP FLV delivered too few HEVC frames"
        );
        assert_decodable_hevc(&flv_video, 4)?;
    }

    server.stop().await;
    Ok(())
}

async fn read_http_body(url: &str) -> Result<String> {
    let url = url::Url::parse(url)?;
    let host = url.host_str().context("HTTP probe URL has no host")?;
    let port = url
        .port_or_known_default()
        .context("HTTP probe URL has no port")?;
    let mut stream = TcpStream::connect((host, port)).await?;
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n",
        url.path()
    );
    stream.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    let response = String::from_utf8(response)?;
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .context("HTTP probe response has no header terminator")?;
    anyhow::ensure!(
        headers.starts_with("HTTP/1.1 200") || headers.starts_with("HTTP/1.0 200"),
        "HTTP probe failed: {headers}"
    );
    Ok(body.to_string())
}

#[tokio::test]
#[ignore = "requires Docker and starts a MediaMTX interoperability container"]
async fn ffmpeg_libavformat_mediamtx_rtsp_avc_aac_and_hevc_are_decodable() -> Result<()> {
    const MEDIAMTX_CONFIG: &str = r"api: yes

authInternalUsers:
  - user: any
    pass:
    ips: []
    permissions:
      - action: publish
      - action: read
      - action: playback
      - action: api

paths:
  all_others:
";

    let mediamtx = synctv_core_testing::start_external_service(
        synctv_core_testing::ExternalServiceRequest::new(
            "mediamtx",
            "synctv-mediamtx-",
            "bluenviron/mediamtx",
            "1.19.3",
            8554,
        )
        .with_exposed_port(1935)
        .with_exposed_port(9997)
        .with_copy_to("/mediamtx.yml", MEDIAMTX_CONFIG.as_bytes())
        .with_stdout_ready_message("[RTSP] started with listeners"),
    )
    .await;

    let avc = avc_fixture()?;
    let aac = aac_fixture()?;
    exercise_ffmpeg_mediamtx_rtsp_pipeline(&mediamtx, "avc-aac", &avc, Some(&aac)).await?;

    let hevc = hevc_fixture()?;
    exercise_ffmpeg_mediamtx_rtsp_pipeline(&mediamtx, "hevc", &hevc, None).await?;
    Ok(())
}

#[tokio::test]
async fn rtsp_udp_video_only_fans_out_to_hls_and_http_flv() -> Result<()> {
    use synctv_xiu::rtsp::{RtspPullConfig, RtspPullSession, RtspTrackSelection, RtspTransport};

    let server = start_server().await?;
    let (frame_sender, generation_id) =
        publish_external_frames(&server, "rtsp://udp-source/live").await?;
    let state = wait_for_playlist(&server).await;

    let (flv_tx, mut flv_rx) = mpsc::channel(32);
    let mut flv_session = HttpFlvSession::new(
        APP.to_string(),
        STREAM.to_string(),
        server.event_sender.clone(),
        flv_tx,
    );
    flv_session.start().await?;
    let flv_task = tokio::spawn(async move { flv_session.run_after_start().await });

    let (rtsp_address, release_media, source_task) = spawn_udp_rtsp_source().await?;
    let mut config = RtspPullConfig::from_url(&format!("rtsp://{rtsp_address}/live"))?;
    config.transport = RtspTransport::Udp;
    config.audio_track = RtspTrackSelection::Disabled;
    let mut session = RtspPullSession::connect(config).await?;
    assert_eq!(session.selected_tracks(), (Some(0), None));
    let _ = release_media.send(());
    while let Some(frame) = session.next_frame().await? {
        frame_sender
            .send(frame)
            .await
            .map_err(|_| anyhow::anyhow!("UDP RTSP frame publisher channel closed"))?;
    }
    source_task.await??;
    unpublish_test_stream(&server, generation_id).await?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let playlist = state.read().generate_m3u8(|name| format!("/hls/{name}"));
        if playlist.contains("#EXT-X-ENDLIST") {
            let name = state
                .read()
                .playlist
                .segments
                .front()
                .context("UDP RTSP HLS segment missing")?
                .ts_name
                .clone();
            let segment = server.storage.read(APP, STREAM, &name).await?;
            let inspection = inspect_ts(&segment)?;
            assert_ts_tracks(&inspection, &[(0x101, 0x1b, 0xe0)]);
            assert_eq!(inspection.pmt_version, 1);
            assert_eq!(inspection.pcr_pid, 0x101);
            assert!(inspection.pes[&0x101].random_access_seen);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "UDP RTSP HLS playlist did not end"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let header = tokio::time::timeout(Duration::from_secs(2), flv_rx.recv())
        .await
        .context("UDP RTSP HTTP-FLV header timeout")?
        .context("UDP RTSP HTTP-FLV response closed")??;
    assert!(header.starts_with(b"FLV"));
    assert_eq!(header[4] & 0x05, 0x01);
    let video_tag = flv_rx
        .recv()
        .await
        .context("UDP RTSP HTTP-FLV video tag missing")??;
    assert_eq!(video_tag.first(), Some(&9));

    flv_task.abort();
    let _ = flv_task.await;
    server.stop().await;
    Ok(())
}
