use crate::rtmp::chunk::packetizer::ChunkPacketizer;

use {
    super::{
        common::Common,
        define,
        define::SessionType,
        errors::{SessionError, SessionErrorValue},
    },
    crate::bytesio::{
        bytes_writer::AsyncBytesWriter,
        net_io::{TNetIO, TcpIO},
    },
    crate::flv::amf0::Amf0ValueType,
    crate::rtmp::{
        chunk::{
            define::CHUNK_SIZE,
            unpacketizer::{ChunkUnpacketizer, UnpackResult},
        },
        handshake,
        handshake::{define::ClientHandshakeState, handshake_client::SimpleHandshakeClient},
        messages::{define::RtmpMessageData, parser::MessageParser},
        netconnection::writer::{ConnectProperties, NetConnection},
        netstream::writer::NetStreamWriter,
        protocol_control_messages::writer::ProtocolControlMessagesWriter,
        user_control_messages::writer::EventMessagesWriter,
        utils::RtmpUrlParser,
    },
    crate::streamhub::define::StreamHubEventSender,
    indexmap::IndexMap,
    std::sync::Arc,
    std::time::Duration,
    tokio::{net::TcpStream, sync::Mutex},
};

pub use crate::rtmp::auth::RtmpStreamMode;

enum ClientSessionState {
    Handshake,
    Connect,
    CreateStream,
    Play,
    PublishingContent,
    StartPublish,
    WaitStateChange,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientSessionType {
    Pull,
    Push,
}

const fn should_forward_media(
    client_type: &ClientSessionType,
    mode: RtmpStreamMode,
    audio: bool,
) -> bool {
    matches!(client_type, ClientSessionType::Push)
        || match mode {
            RtmpStreamMode::Default => true,
            RtmpStreamMode::VideoOnly => !audio,
            RtmpStreamMode::AudioOnly => audio,
        }
}

pub struct ClientSessionConfig {
    pub client_type: ClientSessionType,
    pub raw_domain_name: String,
    pub app_name: String,
    pub raw_stream_name: String,
    pub event_producer: StreamHubEventSender,
    pub gop_num: usize,
    pub per_stream_max_bytes: Option<usize>,
    /// Media types forwarded from a pulled RTMP stream.
    /// Push sessions always receive the complete stream and ignore this policy.
    pub media_mode: RtmpStreamMode,
}

pub struct ClientSession {
    io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>,
    timeout: Option<Duration>,
    common: Common,
    handshaker: SimpleHandshakeClient,
    unpacketizer: ChunkUnpacketizer,
    //domain name with port
    raw_domain_name: String,
    app_name: String,
    //stream name with parameters
    raw_stream_name: String,
    stream_name: String,
    state: ClientSessionState,
    client_type: ClientSessionType,
    sub_app_name: Option<String>,
    sub_stream_name: Option<String>,
    /// Maximum number of GOPs cached for publisher prior data.
    gop_num: usize,
    /// Tracks whether this session has an active subscription to the `StreamHub`
    is_subscribed: bool,
    /// Tracks whether this session has published to the `StreamHub`
    is_publishing: bool,
    /// Per-stream GOP cache memory limit in bytes. `None` uses the default.
    per_stream_max_bytes: Option<usize>,
    media_mode: RtmpStreamMode,
}

impl ClientSession {
    pub fn new(stream: TcpStream, config: ClientSessionConfig) -> Self {
        let ClientSessionConfig {
            client_type,
            raw_domain_name,
            app_name,
            raw_stream_name,
            event_producer,
            gop_num,
            per_stream_max_bytes,
            media_mode,
        } = config;

        let remote_addr = match stream.peer_addr() {
            Ok(addr) => {
                tracing::info!("client session peer: {addr}");
                Some(addr)
            }
            Err(err) => {
                tracing::warn!("failed to read RTMP client session peer address: {err}");
                None
            }
        };

        let tcp_io: Box<dyn TNetIO + Send + Sync> = Box::new(TcpIO::new(stream));
        let net_io = Arc::new(Mutex::new(tcp_io));

        let packetizer = if client_type == ClientSessionType::Push {
            Some(ChunkPacketizer::new(Arc::clone(&net_io)))
        } else {
            None
        };

        let common = Common::new(packetizer, event_producer, SessionType::Client, remote_addr);
        let (stream_name, _) = RtmpUrlParser::parse_stream_name_with_query(&raw_stream_name);

        Self {
            io: Arc::clone(&net_io),
            timeout: None,
            common,
            handshaker: SimpleHandshakeClient::new(Arc::clone(&net_io)),
            unpacketizer: ChunkUnpacketizer::new(),
            raw_domain_name,
            app_name,
            raw_stream_name,
            stream_name,
            state: ClientSessionState::Handshake,
            client_type,
            sub_app_name: None,
            sub_stream_name: None,
            gop_num,
            is_subscribed: false,
            is_publishing: false,
            per_stream_max_bytes,
            media_mode,
        }
    }

    pub const fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = Some(timeout);
    }

    pub async fn run(&mut self) -> Result<(), SessionError> {
        let result = self.run_inner().await;

        // Clean up StreamHub registrations on disconnect (regardless of error or normal exit)
        if let Err(e) = self.cleanup().await {
            tracing::warn!("ClientSession cleanup error: {e}");
        }

        result
    }

    /// Clean up `StreamHub` subscriptions/publications on disconnect
    async fn cleanup(&mut self) -> Result<(), SessionError> {
        if self.is_publishing {
            tracing::info!(
                "ClientSession cleanup: unpublishing app={} stream={}",
                self.app_name,
                self.stream_name
            );
            self.common
                .unpublish_to_stream_hub(self.app_name.clone(), self.stream_name.clone())
                .await?;
            self.is_publishing = false;
        }
        if self.is_subscribed {
            let (app, stream) =
                if let (Some(app), Some(stream)) = (&self.sub_app_name, &self.sub_stream_name) {
                    (app.clone(), stream.clone())
                } else {
                    (self.app_name.clone(), self.stream_name.clone())
                };
            tracing::info!(
                "ClientSession cleanup: unsubscribing app={} stream={}",
                app,
                stream
            );
            self.common.unsubscribe_from_stream_hub(app, stream).await?;
            self.is_subscribed = false;
        }
        Ok(())
    }

    async fn run_inner(&mut self) -> Result<(), SessionError> {
        loop {
            match self.state {
                ClientSessionState::Handshake => {
                    tracing::info!("[C -> S] handshake...");
                    self.handshake().await?;
                    continue;
                }
                ClientSessionState::Connect => {
                    tracing::info!("[C -> S] connect...");
                    self.send_connect(&f64::from(define::TRANSACTION_ID_CONNECT))
                        .await?;
                    self.state = ClientSessionState::WaitStateChange;
                }
                ClientSessionState::CreateStream => {
                    tracing::info!("[C -> S] CreateStream...");
                    self.send_create_stream(&f64::from(define::TRANSACTION_ID_CREATE_STREAM))
                        .await?;
                    self.state = ClientSessionState::WaitStateChange;
                }
                ClientSessionState::Play => {
                    tracing::info!("[C -> S] Play...");
                    self.send_play(&0.0, &self.raw_stream_name.clone(), &0.0, &0.0, &false)
                        .await?;
                    self.state = ClientSessionState::WaitStateChange;
                }
                ClientSessionState::PublishingContent => {
                    tracing::info!("[C -> S] PublishingContent...");
                    self.send_publish(&3.0, &self.raw_stream_name.clone(), &"live".to_string())
                        .await?;
                    self.state = ClientSessionState::WaitStateChange;
                }
                ClientSessionState::StartPublish => {
                    tracing::info!("[C -> S] StartPublish...");
                    self.common.send_channel_data().await?;
                }
                ClientSessionState::WaitStateChange => {}
            }

            let data = match self.timeout {
                None => self.io.lock().await.read().await?,
                Some(t) => self.io.lock().await.read_timeout(t).await?,
            };
            self.unpacketizer.extend_data(&data[..])?;

            loop {
                match self.unpacketizer.read_chunks() {
                    Ok(rv) => {
                        if let UnpackResult::Chunks(chunks) = rv {
                            for chunk_info in chunks {
                                let timestamp = chunk_info.message_header.timestamp;
                                if let Some(mut msg) = MessageParser::new(chunk_info).parse()? {
                                    self.process_messages(&mut msg, &timestamp).await?;
                                }
                            }
                        }
                    }
                    Err(err) => {
                        tracing::trace!("read trunks error: {err}");
                        break;
                    }
                }
            }
        }
    }

    async fn handshake(&mut self) -> Result<(), SessionError> {
        // Timeout to prevent malicious servers from indefinitely hanging the connection
        // (consistent with ServerSession's 10-second handshake timeout)
        const HANDSHAKE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(10);
        // Maximum buffer size during handshake to prevent memory exhaustion
        const MAX_HANDSHAKE_BUFFER: usize = 8192;

        let handshake_start = tokio::time::Instant::now();

        loop {
            self.handshaker.handshake().await?;
            if self.handshaker.state == ClientHandshakeState::Finish {
                tracing::info!("handshake finish");
                break;
            }

            let mut bytes_len = 0;
            while bytes_len < handshake::define::RTMP_HANDSHAKE_SIZE * 2 {
                // Check remaining time before each read
                let remaining = HANDSHAKE_TIMEOUT
                    .checked_sub(handshake_start.elapsed())
                    .ok_or(SessionError {
                        value: SessionErrorValue::Timeout,
                    })?;

                let data = match tokio::time::timeout(remaining, self.io.lock().await.read()).await
                {
                    Ok(result) => result?,
                    Err(_) => {
                        return Err(SessionError {
                            value: SessionErrorValue::Timeout,
                        })
                    }
                };

                bytes_len += data.len();
                if bytes_len > MAX_HANDSHAKE_BUFFER {
                    tracing::warn!(
                        "RTMP client handshake buffer exceeded {MAX_HANDSHAKE_BUFFER} bytes, rejecting"
                    );
                    return Err(SessionError {
                        value: SessionErrorValue::Timeout,
                    });
                }
                self.handshaker.extend_data(&data[..])?;
            }
        }

        self.state = ClientSessionState::Connect;

        Ok(())
    }

    pub async fn process_messages(
        &mut self,
        msg: &mut RtmpMessageData,
        timestamp: &u32,
    ) -> Result<(), SessionError> {
        match msg {
            RtmpMessageData::Amf0Command(command) => {
                tracing::info!("[C <- S] on_amf0_command_message...");
                self.on_amf0_command_message(
                    &command.command_name,
                    &command.transaction_id,
                    &command.command_object,
                    &mut command.others,
                )
                .await?;
            }
            RtmpMessageData::SetPeerBandwidth { .. } => {
                tracing::info!("[C <- S] on_set_peer_bandwidth...");
                self.on_set_peer_bandwidth().await?;
            }
            RtmpMessageData::WindowAcknowledgementSize { .. } => {
                tracing::info!("[C <- S] on_windows_acknowledgement_size...");
            }
            RtmpMessageData::SetChunkSize { chunk_size } => {
                tracing::info!("[C <- S] on_set_chunk_size...");
                self.on_set_chunk_size(chunk_size);
            }
            RtmpMessageData::StreamBegin { stream_id } => {
                tracing::info!("[C <- S] on_stream_begin...");
                Self::on_stream_begin(*stream_id);
            }
            RtmpMessageData::StreamIsRecorded { stream_id } => {
                tracing::info!("[C <- S] on_stream_is_recorded...");
                Self::on_stream_is_recorded(*stream_id);
            }
            RtmpMessageData::AudioData { data } => {
                if should_forward_media(&self.client_type, self.media_mode, true) {
                    self.common.on_audio_data(data, *timestamp)?;
                }
            }
            RtmpMessageData::VideoData { data } => {
                if should_forward_media(&self.client_type, self.media_mode, false) {
                    self.common.on_video_data(data, *timestamp)?;
                }
            }
            RtmpMessageData::AmfData { raw_data } => {
                self.common.on_meta_data(raw_data, *timestamp)?;
            }

            _ => {}
        }
        Ok(())
    }

    async fn on_amf0_command_message(
        &mut self,
        command_name: &Amf0ValueType,
        transaction_id: &Amf0ValueType,
        _command_object: &Amf0ValueType,
        others: &mut Vec<Amf0ValueType>,
    ) -> Result<(), SessionError> {
        tracing::info!("[C <- S] on_amf0_command_message...");
        let empty_cmd_name = &String::new();
        let cmd_name = match command_name {
            Amf0ValueType::UTF8String(str) => str,
            _ => empty_cmd_name,
        };

        let is_transaction_id =
            |number: f64, expected: u8| (number - f64::from(expected)).abs() < f64::EPSILON;

        match cmd_name.as_str() {
            "_result" => match transaction_id {
                Amf0ValueType::Number(number)
                    if is_transaction_id(*number, define::TRANSACTION_ID_CONNECT) =>
                {
                    tracing::info!("[C <- S] on_result_connect...");
                    self.on_result_connect().await?;
                }
                Amf0ValueType::Number(number)
                    if is_transaction_id(*number, define::TRANSACTION_ID_CREATE_STREAM) =>
                {
                    tracing::info!("[C <- S] on_result_create_stream...");
                    self.on_result_create_stream();
                }
                _ => {}
            },
            "_error" => {
                Self::on_error();
            }
            "onStatus" => {
                if others.is_empty() {
                    return Err(SessionError {
                        value: SessionErrorValue::InvalidAmf0ValueCount,
                    });
                }
                match others.remove(0) {
                    Amf0ValueType::Object(obj) => self.on_status(&obj).await?,
                    _ => {
                        return Err(SessionError {
                            value: SessionErrorValue::InvalidAmf0ValueCount,
                        })
                    }
                }
            }

            _ => {}
        }

        Ok(())
    }

    pub async fn send_connect(&mut self, transaction_id: &f64) -> Result<(), SessionError> {
        self.send_set_chunk_size().await?;

        let mut netconnection = NetConnection::new(Arc::clone(&self.io));
        let mut properties = ConnectProperties::new_none();

        let url = format!(
            "rtmp://{domain_name}/{app_name}",
            domain_name = self.raw_domain_name,
            app_name = self.app_name
        );
        properties.app = Some(self.app_name.clone());

        match self.client_type {
            ClientSessionType::Pull => {
                properties.flash_ver = Some("LNX 9,0,124,2".to_string());
                properties.tc_url = Some(url.clone());
                properties.fpad = Some(false);
                properties.capabilities = Some(15_f64);
                properties.audio_codecs = Some(4071_f64);
                properties.video_codecs = Some(252_f64);
                properties.video_function = Some(1_f64);
            }
            ClientSessionType::Push => {
                properties.pub_type = Some("nonprivate".to_string());
                properties.flash_ver = Some("FMLE/3.0 (compatible; xiu)".to_string());
                properties.fpad = Some(false);
                properties.tc_url = Some(url.clone());
            }
        }

        netconnection
            .write_connect(transaction_id, &properties)
            .await?;

        Ok(())
    }

    pub async fn send_create_stream(&mut self, transaction_id: &f64) -> Result<(), SessionError> {
        let mut netconnection = NetConnection::new(Arc::clone(&self.io));
        netconnection.write_create_stream(transaction_id).await?;

        Ok(())
    }

    pub async fn send_delete_stream(
        &mut self,
        transaction_id: &f64,
        stream_id: &f64,
    ) -> Result<(), SessionError> {
        let mut netstream = NetStreamWriter::new(Arc::clone(&self.io));
        netstream
            .write_delete_stream(transaction_id, stream_id)
            .await?;

        Ok(())
    }

    pub async fn send_publish(
        &mut self,
        transaction_id: &f64,
        stream_name: &String,
        stream_type: &String,
    ) -> Result<(), SessionError> {
        let mut netstream = NetStreamWriter::new(Arc::clone(&self.io));
        netstream
            .write_publish(transaction_id, stream_name, stream_type)
            .await?;

        Ok(())
    }

    pub async fn send_play(
        &mut self,
        transaction_id: &f64,
        stream_name: &String,
        start: &f64,
        duration: &f64,
        reset: &bool,
    ) -> Result<(), SessionError> {
        let mut netstream = NetStreamWriter::new(Arc::clone(&self.io));
        netstream
            .write_play(transaction_id, stream_name, start, duration, reset)
            .await?;

        let mut netconnection = NetConnection::new(Arc::clone(&self.io));
        netconnection
            .write_get_stream_length(transaction_id, stream_name)
            .await?;

        self.send_set_buffer_length(1, 1300).await?;

        Ok(())
    }

    pub async fn send_set_chunk_size(&mut self) -> Result<(), SessionError> {
        let mut controlmessage =
            ProtocolControlMessagesWriter::new(AsyncBytesWriter::new(self.io.clone()));
        controlmessage.write_set_chunk_size(CHUNK_SIZE).await?;
        Ok(())
    }

    pub async fn send_window_acknowledgement_size(
        &mut self,
        window_size: u32,
    ) -> Result<(), SessionError> {
        let mut controlmessage =
            ProtocolControlMessagesWriter::new(AsyncBytesWriter::new(self.io.clone()));
        controlmessage
            .write_window_acknowledgement_size(window_size)
            .await?;
        Ok(())
    }

    pub async fn send_set_buffer_length(
        &mut self,
        stream_id: u32,
        ms: u32,
    ) -> Result<(), SessionError> {
        let mut eventmessages = EventMessagesWriter::new(AsyncBytesWriter::new(self.io.clone()));
        eventmessages.write_set_buffer_length(stream_id, ms).await?;

        Ok(())
    }

    async fn on_result_connect(&mut self) -> Result<(), SessionError> {
        let mut controlmessage =
            ProtocolControlMessagesWriter::new(AsyncBytesWriter::new(self.io.clone()));
        controlmessage.write_acknowledgement(3107).await?;

        let mut netstream = NetStreamWriter::new(Arc::clone(&self.io));
        netstream
            .write_release_stream(
                &f64::from(define::TRANSACTION_ID_CONNECT),
                &self.stream_name,
            )
            .await?;
        netstream
            .write_fcpublish(
                &f64::from(define::TRANSACTION_ID_CONNECT),
                &self.stream_name,
            )
            .await?;

        self.state = ClientSessionState::CreateStream;

        Ok(())
    }

    const fn on_result_create_stream(&mut self) {
        match self.client_type {
            ClientSessionType::Pull => {
                self.state = ClientSessionState::Play;
            }
            ClientSessionType::Push => {
                self.state = ClientSessionState::PublishingContent;
            }
        }
    }

    fn on_set_chunk_size(&mut self, chunk_size: &mut u32) {
        // Clamp chunk size to valid RTMP range [128, 65536] to prevent issues
        // from malformed or malicious server responses (e.g. chunk_size=0).
        let clamped = (*chunk_size).clamp(128, 65536);
        if clamped != *chunk_size {
            tracing::warn!(
                "Server sent out-of-range chunk_size={}, clamping to {}",
                chunk_size,
                clamped
            );
        }
        self.unpacketizer.update_max_chunk_size(clamped as usize);
    }

    fn on_stream_is_recorded(stream_id: u32) {
        tracing::trace!("stream is recorded stream_id is {stream_id}");
    }

    fn on_stream_begin(stream_id: u32) {
        tracing::trace!("stream is begin stream_id is {stream_id}");
    }

    async fn on_set_peer_bandwidth(&mut self) -> Result<(), SessionError> {
        self.send_window_acknowledgement_size(5_000_000).await?;

        Ok(())
    }

    const fn on_error() {}

    async fn on_status(
        &mut self,
        obj: &IndexMap<String, Amf0ValueType>,
    ) -> Result<(), SessionError> {
        if let Some(Amf0ValueType::UTF8String(code_info)) = obj.get("code") {
            match &code_info[..] {
                "NetStream.Publish.Start" => {
                    self.state = ClientSessionState::StartPublish;
                    //subscribe from local session and publish to remote rtmp server
                    if let (Some(app_name), Some(stream_name)) =
                        (&self.sub_app_name, &self.sub_stream_name)
                    {
                        self.common
                            .subscribe_from_stream_hub(app_name.clone(), stream_name.clone())
                            .await?;
                    } else {
                        self.common
                            .subscribe_from_stream_hub(
                                self.app_name.clone(),
                                self.stream_name.clone(),
                            )
                            .await?;
                    }
                    self.is_subscribed = true;
                }
                "NetStream.Play.Start" => {
                    //pull from remote rtmp server and publish to local session
                    self.common
                        .publish_to_stream_hub(
                            self.app_name.clone(),
                            self.stream_name.clone(),
                            self.gop_num,
                            self.per_stream_max_bytes,
                        )
                        .await?;
                    self.is_publishing = true;
                }
                _ => {}
            }
        }
        tracing::trace!("{}", obj.len());
        Ok(())
    }

    pub fn subscribe(&mut self, app_name: String, stream_name: String) {
        self.sub_app_name = Some(app_name);
        self.sub_stream_name = Some(stream_name);
    }
}

#[cfg(test)]
mod tests {
    use super::{should_forward_media, ClientSessionType, RtmpStreamMode};

    #[test]
    fn pull_mode_filters_only_the_selected_media_type() {
        assert!(should_forward_media(
            &ClientSessionType::Pull,
            RtmpStreamMode::Default,
            true
        ));
        assert!(should_forward_media(
            &ClientSessionType::Pull,
            RtmpStreamMode::Default,
            false
        ));
        assert!(!should_forward_media(
            &ClientSessionType::Pull,
            RtmpStreamMode::VideoOnly,
            true
        ));
        assert!(should_forward_media(
            &ClientSessionType::Pull,
            RtmpStreamMode::VideoOnly,
            false
        ));
        assert!(should_forward_media(
            &ClientSessionType::Pull,
            RtmpStreamMode::AudioOnly,
            true
        ));
        assert!(!should_forward_media(
            &ClientSessionType::Pull,
            RtmpStreamMode::AudioOnly,
            false
        ));
    }

    #[test]
    fn push_mode_keeps_both_media_types() {
        for mode in [
            RtmpStreamMode::Default,
            RtmpStreamMode::VideoOnly,
            RtmpStreamMode::AudioOnly,
        ] {
            assert!(should_forward_media(&ClientSessionType::Push, mode, true));
            assert!(should_forward_media(&ClientSessionType::Push, mode, false));
        }
    }
}
