//! RTSP client ingest and FLV-compatible frame conversion.
//!
//! RTSP/RTP session handling and codec depacketization are provided by Retina.
//! This module owns SyncTV's track policy and converts H.264, H.265, and AAC
//! access units into the FLV tag bodies consumed by `StreamHub`.

use std::{collections::VecDeque, fmt, net::SocketAddr, time::Duration};

use anyhow::{Context, Result};
use bytes::{Bytes, BytesMut};
use futures::StreamExt as _;
use percent_encoding::percent_decode_str;
use retina::{
    client::{
        Credentials, Demuxed, InitialTimestampPolicy, PlayOptions, Session, SessionOptions,
        SetupOptions, Transport,
    },
    codec::{CodecItem, FrameFormat, ParametersRef, VideoParametersCodec},
};
use url::Url;

use crate::{
    flv::define::{self, AvcCodecId},
    streamhub::define::FrameData,
};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// RTP transport used by an RTSP source.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RtspTransport {
    /// RTP and RTCP interleaved over the RTSP TCP connection.
    #[default]
    Tcp,
    /// RTP and RTCP over UDP.
    Udp,
}

impl From<RtspTransport> for Transport {
    fn from(value: RtspTransport) -> Self {
        match value {
            RtspTransport::Tcp => Self::Tcp(Default::default()),
            RtspTransport::Udp => Self::Udp(Default::default()),
        }
    }
}

/// Selects one media track from a multi-track RTSP presentation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "index")]
pub enum RtspTrackSelection {
    /// Select the first track compatible with SyncTV's live outputs.
    #[default]
    FirstCompatible,
    /// Select an exact zero-based SDP media index.
    Index(usize),
    /// Disable this media type.
    Disabled,
}

/// Credentials used for RTSP Basic or Digest authentication.
#[derive(Clone, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct RtspCredentials {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for RtspCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RtspCredentials")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

/// Final RTSP pull configuration used by the streaming layer.
#[derive(Clone, Debug)]
pub struct RtspPullConfig {
    pub url: Url,
    pub credentials: Option<RtspCredentials>,
    pub transport: RtspTransport,
    pub video_track: RtspTrackSelection,
    pub audio_track: RtspTrackSelection,
    pub request_timeout: Duration,
}

impl RtspPullConfig {
    /// Parses an RTSP URL and extracts URL userinfo into explicit credentials.
    pub fn from_url(source_url: &str) -> Result<Self> {
        let mut url = Url::parse(source_url).context("invalid RTSP source URL")?;
        anyhow::ensure!(url.scheme() == "rtsp", "RTSP source URL must use rtsp://");
        anyhow::ensure!(
            url.host_str().is_some(),
            "RTSP source URL is missing a host"
        );

        let credentials = if url.username().is_empty() {
            None
        } else {
            let username = decode_userinfo(url.username())?;
            let password = decode_userinfo(url.password().unwrap_or_default())?;
            Some(RtspCredentials { username, password })
        };
        url.set_password(None)
            .map_err(|()| anyhow::anyhow!("failed to remove RTSP URL password"))?;
        url.set_username("")
            .map_err(|()| anyhow::anyhow!("failed to remove RTSP URL username"))?;

        Ok(Self {
            url,
            credentials,
            transport: RtspTransport::Tcp,
            video_track: RtspTrackSelection::FirstCompatible,
            audio_track: RtspTrackSelection::FirstCompatible,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        })
    }

    /// Replaces the URL host and port with an address validated by the caller.
    /// This pins the connection target and closes the DNS-rebinding window.
    pub fn pin_address(&mut self, address: SocketAddr) -> Result<()> {
        self.url
            .set_host(Some(&address.ip().to_string()))
            .context("failed to pin RTSP source address")?;
        self.url
            .set_port(Some(address.port()))
            .map_err(|()| anyhow::anyhow!("failed to pin RTSP source port"))?;
        Ok(())
    }
}

fn decode_userinfo(value: &str) -> Result<String> {
    percent_decode_str(value)
        .decode_utf8()
        .map(std::borrow::Cow::into_owned)
        .context("RTSP credentials contain invalid UTF-8")
}

/// Connected RTSP session yielding FLV-compatible `FrameData` values.
pub struct RtspPullSession {
    demuxed: Demuxed,
    video_track: Option<usize>,
    audio_track: Option<usize>,
    video_sequence_header: Option<Bytes>,
    audio_sequence_header: Option<Bytes>,
    pending: VecDeque<FrameData>,
}

impl RtspPullSession {
    /// DESCRIBE, SETUP, and PLAY an RTSP presentation.
    pub async fn connect(config: RtspPullConfig) -> Result<Self> {
        let credentials = config.credentials.map(|credentials| Credentials {
            username: credentials.username,
            password: credentials.password,
        });
        let options = SessionOptions::default()
            .creds(credentials)
            .user_agent(format!("SyncTV/{}", env!("CARGO_PKG_VERSION")));

        let mut session = tokio::time::timeout(
            config.request_timeout,
            Session::describe(config.url, options),
        )
        .await
        .context("RTSP DESCRIBE timed out")?
        .context("RTSP DESCRIBE failed")?;

        let video_track = select_track(
            session.streams(),
            "video",
            config.video_track,
            is_supported_video,
        )?;
        let audio_track = select_track(
            session.streams(),
            "audio",
            config.audio_track,
            is_supported_audio,
        )?;
        anyhow::ensure!(
            video_track.is_some() || audio_track.is_some(),
            "RTSP presentation has no compatible H.264/H.265 video or AAC audio track"
        );

        for track in [video_track, audio_track].into_iter().flatten() {
            let setup = SetupOptions::default()
                .transport(config.transport.into())
                .frame_format(FrameFormat::MP4);
            tokio::time::timeout(config.request_timeout, session.setup(track, setup))
                .await
                .with_context(|| format!("RTSP SETUP timed out for track {track}"))?
                .with_context(|| format!("RTSP SETUP failed for track {track}"))?;
        }

        let play_options =
            PlayOptions::default().initial_timestamp(InitialTimestampPolicy::Permissive);
        let playing = tokio::time::timeout(config.request_timeout, session.play(play_options))
            .await
            .context("RTSP PLAY timed out")?
            .context("RTSP PLAY failed")?;
        let demuxed = playing
            .demuxed()
            .context("RTSP presentation contains an unsupported selected track")?;

        Ok(Self {
            demuxed,
            video_track,
            audio_track,
            video_sequence_header: None,
            audio_sequence_header: None,
            pending: VecDeque::new(),
        })
    }

    /// Returns the selected SDP media indices as `(video, audio)`.
    #[must_use]
    pub const fn selected_tracks(&self) -> (Option<usize>, Option<usize>) {
        (self.video_track, self.audio_track)
    }

    /// Returns the next FLV-compatible frame, including codec sequence headers.
    pub async fn next_frame(&mut self) -> Result<Option<FrameData>> {
        loop {
            if let Some(frame) = self.pending.pop_front() {
                return Ok(Some(frame));
            }

            let Some(item) = self.demuxed.next().await else {
                return Ok(None);
            };
            let item = item.context("RTSP/RTP stream failed")?;
            match item {
                CodecItem::VideoFrame(frame) if Some(frame.stream_id()) == self.video_track => {
                    let timestamp = timestamp_millis(frame.timestamp());
                    let stream_id = frame.stream_id();
                    let (codec_id, extra_data) = video_parameters(&self.demuxed, stream_id)?;
                    let sequence_header = video_tag_body(
                        codec_id,
                        true,
                        define::avc_packet_type::AVC_SEQHDR,
                        &extra_data,
                    );
                    if self.video_sequence_header.as_ref() != Some(&sequence_header) {
                        self.video_sequence_header = Some(sequence_header.clone());
                        self.pending.push_back(FrameData::Video {
                            timestamp,
                            data: sequence_header,
                        });
                    }
                    self.pending.push_back(FrameData::Video {
                        timestamp,
                        data: video_tag_body(
                            codec_id,
                            frame.is_random_access_point(),
                            define::avc_packet_type::AVC_NALU,
                            frame.data(),
                        ),
                    });
                }
                CodecItem::AudioFrame(frame) if Some(frame.stream_id()) == self.audio_track => {
                    let timestamp = timestamp_millis(frame.timestamp());
                    let stream_id = frame.stream_id();
                    let (extra_data, stereo) = audio_parameters(&self.demuxed, stream_id)?;
                    let sequence_header =
                        audio_tag_body(stereo, define::aac_packet_type::AAC_SEQHDR, &extra_data);
                    if self.audio_sequence_header.as_ref() != Some(&sequence_header) {
                        self.audio_sequence_header = Some(sequence_header.clone());
                        self.pending.push_back(FrameData::Audio {
                            timestamp,
                            data: sequence_header,
                        });
                    }
                    self.pending.push_back(FrameData::Audio {
                        timestamp,
                        data: audio_tag_body(
                            stereo,
                            define::aac_packet_type::AAC_RAW,
                            frame.data(),
                        ),
                    });
                }
                _ => {}
            }
        }
    }
}

fn select_track(
    streams: &[retina::client::Stream],
    media: &str,
    selection: RtspTrackSelection,
    supported: fn(&retina::client::Stream) -> bool,
) -> Result<Option<usize>> {
    match selection {
        RtspTrackSelection::Disabled => Ok(None),
        RtspTrackSelection::FirstCompatible => Ok(streams.iter().position(supported)),
        RtspTrackSelection::Index(index) => {
            let stream = streams
                .get(index)
                .with_context(|| format!("RTSP {media} track index {index} is out of range"))?;
            anyhow::ensure!(
                supported(stream),
                "RTSP track {index} is {}, encoded as {}, and cannot be used as {media}",
                stream.media(),
                stream.encoding_name()
            );
            Ok(Some(index))
        }
    }
}

fn is_supported_video(stream: &retina::client::Stream) -> bool {
    stream.media() == "video" && matches!(stream.encoding_name(), "h264" | "h265")
}

fn is_supported_audio(stream: &retina::client::Stream) -> bool {
    stream.media() == "audio" && stream.encoding_name() == "mpeg4-generic"
}

fn video_parameters(demuxed: &Demuxed, stream_id: usize) -> Result<(u8, Bytes)> {
    let parameters = demuxed
        .streams()
        .get(stream_id)
        .and_then(retina::client::Stream::parameters)
        .with_context(|| format!("RTSP video track {stream_id} has no codec parameters"))?;
    let ParametersRef::Video(parameters) = parameters else {
        anyhow::bail!("RTSP track {stream_id} returned non-video parameters");
    };
    let codec_id = match parameters.codec_params() {
        VideoParametersCodec::H264 { .. } => AvcCodecId::H264 as u8,
        VideoParametersCodec::H265 { .. } => AvcCodecId::HEVC as u8,
        _ => anyhow::bail!("RTSP video track {stream_id} uses an unsupported codec"),
    };
    anyhow::ensure!(
        !parameters.extra_data().is_empty(),
        "RTSP video track {stream_id} has empty codec configuration"
    );
    Ok((codec_id, Bytes::copy_from_slice(parameters.extra_data())))
}

fn audio_parameters(demuxed: &Demuxed, stream_id: usize) -> Result<(Bytes, bool)> {
    let parameters = demuxed
        .streams()
        .get(stream_id)
        .and_then(retina::client::Stream::parameters)
        .with_context(|| format!("RTSP audio track {stream_id} has no codec parameters"))?;
    let ParametersRef::Audio(parameters) = parameters else {
        anyhow::bail!("RTSP track {stream_id} returned non-audio parameters");
    };
    anyhow::ensure!(
        !parameters.extra_data().is_empty(),
        "RTSP AAC track {stream_id} has empty AudioSpecificConfig"
    );
    Ok((
        Bytes::copy_from_slice(parameters.extra_data()),
        parameters.channels().get() > 1,
    ))
}

fn timestamp_millis(timestamp: retina::Timestamp) -> u32 {
    let elapsed = timestamp.elapsed();
    if elapsed <= 0 {
        return 0;
    }
    let millis = u128::from(elapsed.cast_unsigned())
        .saturating_mul(1_000)
        .checked_div(u128::from(timestamp.clock_rate().get()))
        .unwrap_or_default();
    u32::try_from(millis).unwrap_or(u32::MAX)
}

fn video_tag_body(codec_id: u8, keyframe: bool, packet_type: u8, payload: &[u8]) -> Bytes {
    let mut body = BytesMut::with_capacity(5 + payload.len());
    let frame_type = if keyframe {
        define::frame_type::KEY_FRAME
    } else {
        define::frame_type::INTER_FRAME
    };
    body.extend_from_slice(&[(frame_type << 4) | codec_id, packet_type, 0, 0, 0]);
    body.extend_from_slice(payload);
    body.freeze()
}

fn audio_tag_body(stereo: bool, packet_type: u8, payload: &[u8]) -> Bytes {
    let mut body = BytesMut::with_capacity(2 + payload.len());
    let sound_type = u8::from(stereo);
    let flags = ((define::SoundFormat::AAC as u8) << 4) | (3 << 2) | (1 << 1) | sound_type;
    body.extend_from_slice(&[flags, packet_type]);
    body.extend_from_slice(payload);
    body.freeze()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    async fn read_rtsp_request(stream: &mut tokio::net::TcpStream) -> Result<String> {
        let mut request = Vec::new();
        let mut byte = [0_u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            let read = stream.read(&mut byte).await?;
            anyhow::ensure!(read == 1, "RTSP client closed during request");
            request.push(byte[0]);
            anyhow::ensure!(request.len() <= 16 * 1024, "RTSP test request is too large");
        }
        String::from_utf8(request).context("RTSP test request is not UTF-8")
    }

    fn request_cseq(request: &str) -> Result<&str> {
        request
            .lines()
            .find_map(|line| line.strip_prefix("CSeq: "))
            .context("RTSP test request is missing CSeq")
    }

    async fn spawn_interleaved_h264_server(
    ) -> Result<(SocketAddr, tokio::task::JoinHandle<Result<()>>)> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;

            let describe = read_rtsp_request(&mut stream).await?;
            anyhow::ensure!(
                describe.starts_with("DESCRIBE "),
                "expected DESCRIBE request"
            );
            let sdp = concat!(
                "v=0\r\n",
                "o=- 1 1 IN IP4 127.0.0.1\r\n",
                "s=SyncTV RTSP test\r\n",
                "t=0 0\r\n",
                "a=control:*\r\n",
                "m=video 0 RTP/AVP 96\r\n",
                "c=IN IP4 0.0.0.0\r\n",
                "a=rtpmap:96 H264/90000\r\n",
                "a=fmtp:96 packetization-mode=1;sprop-parameter-sets=Z0IAH5WoFAFuQA==,aM4G4g==\r\n",
                "a=control:trackID=1\r\n",
            );
            let response = format!(
                "RTSP/1.0 200 OK\r\nCSeq: {}\r\nContent-Type: application/sdp\r\nContent-Base: rtsp://{address}/live/\r\nContent-Length: {}\r\n\r\n{sdp}",
                request_cseq(&describe)?,
                sdp.len()
            );
            stream.write_all(response.as_bytes()).await?;

            let setup = read_rtsp_request(&mut stream).await?;
            anyhow::ensure!(setup.starts_with("SETUP "), "expected SETUP request");
            let response = format!(
                "RTSP/1.0 200 OK\r\nCSeq: {}\r\nSession: synctv-test;timeout=60\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1;ssrc=01020304;mode=\"play\"\r\n\r\n",
                request_cseq(&setup)?
            );
            stream.write_all(response.as_bytes()).await?;

            let play = read_rtsp_request(&mut stream).await?;
            anyhow::ensure!(play.starts_with("PLAY "), "expected PLAY request");
            let response = format!(
                "RTSP/1.0 200 OK\r\nCSeq: {}\r\nSession: synctv-test\r\nRTP-Info: url=rtsp://{address}/live/trackID=1;seq=1;rtptime=0\r\n\r\n",
                request_cseq(&play)?
            );
            stream.write_all(response.as_bytes()).await?;

            let rtp = [
                0x80, 0xe0, // RTP v2, marker, payload type 96.
                0x00, 0x01, // Sequence number.
                0x00, 0x00, 0x00, 0x00, // RTP timestamp.
                0x01, 0x02, 0x03, 0x04, // SSRC.
                0x65, 0x88, 0x84, // H.264 IDR NAL.
            ];
            let mut interleaved = Vec::with_capacity(4 + rtp.len());
            interleaved.extend_from_slice(&[b'$', 0]);
            interleaved.extend_from_slice(&u16::try_from(rtp.len())?.to_be_bytes());
            interleaved.extend_from_slice(&rtp);
            stream.write_all(&interleaved).await?;
            stream.shutdown().await?;
            Ok(())
        });
        Ok((address, handle))
    }

    #[test]
    fn url_credentials_are_extracted_and_redacted() -> Result<()> {
        let config = RtspPullConfig::from_url("rtsp://camera%40user:p%40ss@example.com/live")?;
        assert_eq!(config.url.as_str(), "rtsp://example.com/live");
        assert_eq!(
            config.credentials,
            Some(RtspCredentials {
                username: "camera@user".to_string(),
                password: "p@ss".to_string(),
            })
        );
        Ok(())
    }

    #[test]
    fn video_tags_use_legacy_flv_avc_and_hevc_layout() {
        let avc = video_tag_body(AvcCodecId::H264 as u8, true, 0, &[1, 2]);
        assert_eq!(&avc[..], &[0x17, 0, 0, 0, 0, 1, 2]);

        let hevc = video_tag_body(AvcCodecId::HEVC as u8, false, 1, &[3, 4]);
        assert_eq!(&hevc[..], &[0x2c, 1, 0, 0, 0, 3, 4]);
    }

    #[test]
    fn audio_tags_use_aac_sequence_and_raw_layout() {
        let sequence = audio_tag_body(true, define::aac_packet_type::AAC_SEQHDR, &[0x12, 0x10]);
        assert_eq!(&sequence[..], &[0xaf, 0, 0x12, 0x10]);

        let raw = audio_tag_body(false, define::aac_packet_type::AAC_RAW, &[1, 2, 3]);
        assert_eq!(&raw[..], &[0xae, 1, 1, 2, 3]);
    }

    #[tokio::test]
    async fn pulls_interleaved_h264_into_flv_frames() -> Result<()> {
        let (address, server) = spawn_interleaved_h264_server().await?;
        let config = RtspPullConfig::from_url(&format!("rtsp://{address}/live"))?;
        let mut session = RtspPullSession::connect(config).await?;
        assert_eq!(session.selected_tracks(), (Some(0), None));

        let sequence = session
            .next_frame()
            .await?
            .context("missing AVC sequence header")?;
        let FrameData::Video { timestamp, data } = sequence else {
            anyhow::bail!("expected video sequence header");
        };
        assert_eq!(timestamp, 0);
        assert_eq!(&data[..2], &[0x17, define::avc_packet_type::AVC_SEQHDR]);

        let frame = session.next_frame().await?.context("missing AVC frame")?;
        let FrameData::Video { timestamp, data } = frame else {
            anyhow::bail!("expected video frame");
        };
        assert_eq!(timestamp, 0);
        assert_eq!(&data[..2], &[0x17, define::avc_packet_type::AVC_NALU]);
        assert_eq!(&data[5..], &[0, 0, 0, 3, 0x65, 0x88, 0x84]);

        server.await.context("RTSP test server task failed")??;
        Ok(())
    }
}
