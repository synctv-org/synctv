//! Pure-Rust live-stream test fixtures.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::Result;
use bytes::BytesMut;
use synctv_xiu::{
    bytesio::{
        bytes_writer::AsyncBytesWriter,
        net_io::{TNetIO, TcpIO},
    },
    flv::amf0::Amf0ValueType,
    rtmp::{
        chunk::{
            errors::UnpackErrorValue,
            packetizer::ChunkPacketizer,
            unpacketizer::{ChunkUnpacketizer, UnpackResult},
        },
        handshake::{define::ClientHandshakeState, handshake_client::SimpleHandshakeClient},
        messages::{
            define::{msg_type_id, RtmpMessageData},
            parser::MessageParser,
        },
        netconnection::writer::{ConnectProperties, NetConnection},
        netstream::writer::NetStreamWriter,
        protocol_control_messages::writer::ProtocolControlMessagesWriter,
        session::{common::Common, define::SessionType},
    },
    streamhub::define::STREAM_HUB_EVENT_CHANNEL_CAPACITY,
};
use tokio::{net::TcpStream, sync::Mutex};

type SharedIo = Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>;

/// Minimal RTMP publisher for end-to-end tests.
///
/// It uses xiu's production handshake, AMF command, chunk, and media writers and
/// connects directly to a test RTMP listener. No external media process is
/// required.
pub struct RtmpPublisher {
    io: SharedIo,
    media: Common,
}

/// Media message received from a real RTMP play session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtmpMediaMessage {
    pub timestamp: u32,
    pub media_type: RtmpMediaType,
    pub payload: BytesMut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtmpMediaType {
    Audio,
    Video,
}

/// Minimal RTMP player for end-to-end network-output tests.
pub struct RtmpPlayer {
    io: SharedIo,
    unpacketizer: ChunkUnpacketizer,
}

impl RtmpPublisher {
    /// Connects and publishes `stream_name` under `app_name` in live mode.
    pub async fn connect(
        address: SocketAddr,
        app_name: impl Into<String>,
        stream_name: impl Into<String>,
    ) -> Result<Self> {
        Self::connect_inner(address, app_name.into(), stream_name.into(), true).await
    }

    /// Sends a publish request and returns before the server accepts it.
    ///
    /// Rejection-path tests use this to exercise sessions that never receive
    /// `NetStream.Publish.Start`.
    pub async fn connect_unconfirmed(
        address: SocketAddr,
        app_name: impl Into<String>,
        stream_name: impl Into<String>,
    ) -> Result<Self> {
        Self::connect_inner(address, app_name.into(), stream_name.into(), false).await
    }

    async fn connect_inner(
        address: SocketAddr,
        app_name: String,
        stream_name: String,
        wait_for_confirmation: bool,
    ) -> Result<Self> {
        let (io, mut stream_writer) =
            connect_rtmp_session(address, &app_name, "SyncTV test publisher").await?;
        stream_writer
            .write_publish(&3.0, &stream_name, &"live".to_string())
            .await?;

        if wait_for_confirmation {
            wait_for_publish_start(&io, Duration::from_secs(5)).await?;
        }

        let media = Common::new(
            Some(ChunkPacketizer::new(Arc::clone(&io))),
            // Common's send methods never use the StreamHub event sender.
            tokio::sync::mpsc::channel(STREAM_HUB_EVENT_CHANNEL_CAPACITY).0,
            SessionType::Client,
            None,
        );

        Ok(Self { io, media })
    }

    /// Sends a synthetic AVC sequence header or media frame.
    pub async fn send_video(&mut self, timestamp: u32, keyframe: bool) -> Result<()> {
        self.media
            .send_video(avc_test_tag(timestamp, keyframe), timestamp)
            .await
            .map_err(|error| anyhow::anyhow!("send RTMP video: {error}"))?;
        tokio::time::sleep(Duration::from_millis(5)).await;
        Ok(())
    }

    /// Sends a synthetic AAC sequence header or media frame.
    pub async fn send_audio(&mut self, timestamp: u32) -> Result<()> {
        self.media
            .send_audio(aac_test_tag(timestamp), timestamp)
            .await
            .map_err(|error| anyhow::anyhow!("send RTMP audio: {error}"))?;
        tokio::time::sleep(Duration::from_millis(2)).await;
        Ok(())
    }

    /// Sends an exact RTMP video message payload.
    pub async fn send_raw_video(&mut self, timestamp: u32, data: &[u8]) -> Result<()> {
        self.media
            .send_video(BytesMut::from(data), timestamp)
            .await
            .map_err(|error| anyhow::anyhow!("send raw RTMP video: {error}"))
    }

    /// Sends an exact RTMP audio message payload.
    pub async fn send_raw_audio(&mut self, timestamp: u32, data: &[u8]) -> Result<()> {
        self.media
            .send_audio(BytesMut::from(data), timestamp)
            .await
            .map_err(|error| anyhow::anyhow!("send raw RTMP audio: {error}"))
    }

    /// Closes the publisher connection and ends the publication.
    pub fn close(self) {
        drop(self.media);
        drop(self.io);
    }

    /// Abruptly closes the underlying transport to exercise publisher failure
    /// and server-side lifecycle cleanup paths.
    pub async fn abort(self) -> Result<()> {
        self.io.lock().await.shutdown().await?;
        drop(self.media);
        drop(self.io);
        Ok(())
    }
}

async fn wait_for_publish_start(io: &SharedIo, timeout: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut unpacketizer = ChunkUnpacketizer::new();

    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .ok_or_else(|| anyhow::anyhow!("RTMP publish confirmation timed out"))?;
        let data = io.lock().await.read_timeout(remaining).await?;
        unpacketizer.extend_data(&data)?;

        loop {
            let chunks = match unpacketizer.read_chunks() {
                Ok(UnpackResult::Chunks(chunks)) => chunks,
                Ok(_) => continue,
                Err(error)
                    if matches!(
                        error.value,
                        UnpackErrorValue::CannotParse | UnpackErrorValue::MessageTooLarge(_, _)
                    ) =>
                {
                    return Err(error.into());
                }
                Err(_) => break,
            };

            for chunk in chunks {
                if chunk.message_header.msg_type_id == msg_type_id::SET_CHUNK_SIZE {
                    anyhow::ensure!(chunk.payload.len() >= 4, "truncated RTMP Set Chunk Size");
                    let chunk_size = u32::from_be_bytes([
                        chunk.payload[0],
                        chunk.payload[1],
                        chunk.payload[2],
                        chunk.payload[3],
                    ]) & 0x7fff_ffff;
                    unpacketizer.update_max_chunk_size(usize::try_from(chunk_size)?);
                    continue;
                }

                if chunk.message_header.msg_type_id != msg_type_id::COMMAND_AMF0
                    && chunk.message_header.msg_type_id != msg_type_id::COMMAND_AMF3
                {
                    continue;
                }
                let Some(RtmpMessageData::Amf0Command(command)) =
                    MessageParser::new(chunk).parse()?
                else {
                    continue;
                };
                if command.command_name != Amf0ValueType::UTF8String("onStatus".to_string()) {
                    continue;
                }
                let Some(Amf0ValueType::Object(status)) = command.others.first() else {
                    continue;
                };
                let Some(Amf0ValueType::UTF8String(code)) = status.get("code") else {
                    continue;
                };
                anyhow::ensure!(
                    code == "NetStream.Publish.Start",
                    "RTMP publish rejected with status {code}"
                );
                return Ok(());
            }
        }
    }
}

impl RtmpPlayer {
    /// Connects to an RTMP server and starts a live play session.
    pub async fn connect(
        address: SocketAddr,
        app_name: impl Into<String>,
        stream_name: impl Into<String>,
    ) -> Result<Self> {
        let app_name = app_name.into();
        let stream_name = stream_name.into();
        let (io, mut stream_writer) =
            connect_rtmp_session(address, &app_name, "SyncTV test player").await?;
        stream_writer
            .write_play(&3.0, &stream_name, &-2.0, &-1.0, &true)
            .await?;

        Ok(Self {
            io,
            unpacketizer: ChunkUnpacketizer::new(),
        })
    }

    /// Receives up to `count` media messages, failing when the deadline expires.
    pub async fn receive_media(
        &mut self,
        count: usize,
        timeout: Duration,
    ) -> Result<Vec<RtmpMediaMessage>> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut media = Vec::with_capacity(count);

        while media.len() < count {
            let remaining = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .ok_or_else(|| anyhow::anyhow!("RTMP media receive timed out"))?;
            let data = self.io.lock().await.read_timeout(remaining).await?;
            self.unpacketizer.extend_data(&data)?;

            loop {
                match self.unpacketizer.read_chunks() {
                    Ok(UnpackResult::Chunks(chunks)) => {
                        for chunk in chunks {
                            match chunk.message_header.msg_type_id {
                                msg_type_id::SET_CHUNK_SIZE => {
                                    anyhow::ensure!(
                                        chunk.payload.len() >= 4,
                                        "truncated RTMP Set Chunk Size"
                                    );
                                    let chunk_size = u32::from_be_bytes([
                                        chunk.payload[0],
                                        chunk.payload[1],
                                        chunk.payload[2],
                                        chunk.payload[3],
                                    ]) & 0x7fff_ffff;
                                    self.unpacketizer
                                        .update_max_chunk_size(usize::try_from(chunk_size)?);
                                }
                                msg_type_id::AUDIO | msg_type_id::VIDEO => {
                                    media.push(RtmpMediaMessage {
                                        timestamp: chunk.message_header.timestamp,
                                        media_type: if chunk.message_header.msg_type_id
                                            == msg_type_id::VIDEO
                                        {
                                            RtmpMediaType::Video
                                        } else {
                                            RtmpMediaType::Audio
                                        },
                                        payload: chunk.payload,
                                    });
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.value,
                            UnpackErrorValue::CannotParse | UnpackErrorValue::MessageTooLarge(_, _)
                        ) =>
                    {
                        return Err(error.into());
                    }
                    Err(_) => break,
                }
            }
        }

        Ok(media)
    }
}

async fn connect_rtmp_session(
    address: SocketAddr,
    app_name: &str,
    flash_ver: &str,
) -> Result<(SharedIo, NetStreamWriter)> {
    let stream = TcpStream::connect(address).await?;
    let io: SharedIo = Arc::new(Mutex::new(Box::new(TcpIO::new(stream))));
    complete_client_handshake(&io).await?;

    let mut control = ProtocolControlMessagesWriter::new(AsyncBytesWriter::new(Arc::clone(&io)));
    control
        .write_set_chunk_size(synctv_xiu::rtmp::chunk::define::CHUNK_SIZE)
        .await?;

    let mut connection = NetConnection::new(Arc::clone(&io));
    let mut properties = ConnectProperties::new(app_name.to_string());
    properties.tc_url = Some(format!("rtmp://{address}/{app_name}"));
    properties.flash_ver = Some(flash_ver.to_string());
    connection.write_connect(&1.0, &properties).await?;
    connection.write_create_stream(&2.0).await?;

    Ok((Arc::clone(&io), NetStreamWriter::new(Arc::clone(&io))))
}

async fn complete_client_handshake(io: &SharedIo) -> Result<()> {
    let mut handshake = SimpleHandshakeClient::new(Arc::clone(io));
    while handshake.state != ClientHandshakeState::Finish {
        handshake.handshake().await?;
        if handshake.state == ClientHandshakeState::ReadS0S1S2 {
            let mut received = 0;
            while received < synctv_xiu::rtmp::handshake::define::RTMP_HANDSHAKE_SIZE * 2 + 1 {
                let data = io.lock().await.read().await?;
                received += data.len();
                handshake.extend_data(&data)?;
            }
        }
    }
    Ok(())
}

fn avc_decoder_configuration_record() -> BytesMut {
    // Baseline H.264 SPS/PPS from a small 320x240 stream.
    let sps = [
        0x67, 0x42, 0x00, 0x1f, 0x95, 0xa8, 0x14, 0x01, 0x6e, 0x40, 0x00,
    ];
    let pps = [0x68, 0xce, 0x06, 0xe2];
    let mut data = BytesMut::from(&[0x17, 0x00, 0x00, 0x00, 0x00][..]);
    data.extend_from_slice(&[1, sps[1], sps[2], sps[3], 0xff, 0xe1]);
    data.extend_from_slice(
        &u16::try_from(sps.len())
            .expect("SPS length fits u16")
            .to_be_bytes(),
    );
    data.extend_from_slice(&sps);
    data.extend_from_slice(&[1]);
    data.extend_from_slice(
        &u16::try_from(pps.len())
            .expect("PPS length fits u16")
            .to_be_bytes(),
    );
    data.extend_from_slice(&pps);
    data
}

/// Builds a deterministic AVC test tag for assertions and raw frame injection.
#[must_use]
pub fn avc_test_tag(timestamp: u32, keyframe: bool) -> BytesMut {
    if timestamp == 0 {
        return avc_decoder_configuration_record();
    }

    let mut data = BytesMut::from(&[if keyframe { 0x17 } else { 0x27 }, 0x01, 0, 0, 0][..]);
    let nal: &[u8] = if keyframe {
        &[0x65, 0x88, 0x84, 0x21]
    } else {
        &[0x41, 0x9a, 0x22]
    };
    data.extend_from_slice(
        &u32::try_from(nal.len())
            .expect("NAL length fits u32")
            .to_be_bytes(),
    );
    data.extend_from_slice(nal);
    data
}

/// Builds a deterministic AAC-LC 44.1 kHz stereo test tag.
#[must_use]
pub fn aac_test_tag(timestamp: u32) -> BytesMut {
    if timestamp == 0 {
        BytesMut::from(&[0xaf, 0x00, 0x12, 0x10][..])
    } else {
        BytesMut::from(&[0xaf, 0x01, 0x21, 0x10][..])
    }
}
