use crate::rtmp::auth::{AuthCallback, RtmpStreamMode};
use crate::rtmp::callbacks::StreamEventCallbacks;
use crate::rtmp::chunk::{errors::UnpackErrorValue, packetizer::ChunkPacketizer};

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
        config, handshake,
        handshake::{define::ServerHandshakeState, handshake_server::HandshakeServer},
        messages::{define::RtmpMessageData, parser::MessageParser},
        netconnection::writer::{ConnectProperties, ConnectResponse, NetConnection},
        netstream::writer::NetStreamWriter,
        protocol_control_messages::writer::ProtocolControlMessagesWriter,
        user_control_messages::writer::EventMessagesWriter,
        utils::RtmpUrlParser,
    },
    crate::streamhub::define::StreamHubEventSender,
    bytes::BytesMut,
    indexmap::IndexMap,
    std::{sync::Arc, time::Duration},
    tokio::{net::TcpStream, sync::Mutex},
};

const MAX_HANDSHAKE_BUFFER: usize = 8192;
const MAX_RTMP_STREAM_NAME_WITH_QUERY_LEN: usize = 2048;

enum ServerSessionState {
    Handshake,
    ReadChunk,
    // OnConnect,
    // OnCreateStream,
    //Publish,
    DeleteStream,
    Play,
}

/// Overall session idle timeout: if no complete RTMP message is received
/// within this duration, the session is terminated (prevents slow-rate `DoS`).
const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

fn remove_first_string(values: &mut Vec<Amf0ValueType>) -> Option<String> {
    if values.is_empty() {
        return None;
    }

    match values.remove(0) {
        Amf0ValueType::UTF8String(value) => Some(value),
        _ => None,
    }
}

fn remove_first_number(values: &mut Vec<Amf0ValueType>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }

    match values.remove(0) {
        Amf0ValueType::Number(value) => Some(value),
        _ => None,
    }
}

fn remove_first_bool(values: &mut Vec<Amf0ValueType>) -> Option<bool> {
    if values.is_empty() {
        return None;
    }

    match values.remove(0) {
        Amf0ValueType::Boolean(value) => Some(value),
        _ => None,
    }
}

pub struct ServerSession {
    pub app_name: String,
    pub stream_name: String,
    pub query: Option<String>,
    io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>,
    handshaker: HandshakeServer,
    unpacketizer: ChunkUnpacketizer,
    state: ServerSessionState,
    bytesio_data: BytesMut,
    has_remaing_data: bool,
    connect_properties: ConnectProperties,
    pub common: Common,
    /// Maximum number of GOPs cached for publisher prior data.
    gop_num: usize,
    auth: Option<Arc<dyn AuthCallback>>,
    /// Whether this session has successfully published (vs playing)
    is_publishing: bool,
    /// Tracks when the last complete RTMP message was received (for idle timeout).
    last_message_time: tokio::time::Instant,
    /// Per-stream GOP cache memory limit in bytes. `None` uses the default.
    per_stream_max_bytes: Option<usize>,
    /// Optional callbacks for stream lifecycle events (metrics, etc.)
    callbacks: Arc<StreamEventCallbacks>,
    media_mode: RtmpStreamMode,
}

impl ServerSession {
    pub fn new(
        stream: TcpStream,
        event_producer: StreamHubEventSender,
        gop_num: usize,
        auth: Option<Arc<dyn AuthCallback>>,
        per_stream_max_bytes: Option<usize>,
        callbacks: Arc<StreamEventCallbacks>,
    ) -> Self {
        let remote_addr = match stream.peer_addr() {
            Ok(addr) => {
                tracing::info!("server session peer: {addr}");
                Some(addr)
            }
            Err(err) => {
                tracing::warn!("failed to read RTMP server session peer address: {err}");
                None
            }
        };

        let tcp_io: Box<dyn TNetIO + Send + Sync> = Box::new(TcpIO::new(stream));
        let net_io = Arc::new(Mutex::new(tcp_io));

        Self {
            app_name: String::new(),
            stream_name: String::new(),
            query: None,
            io: Arc::clone(&net_io),
            handshaker: HandshakeServer::new(Arc::clone(&net_io)),
            unpacketizer: ChunkUnpacketizer::new(),
            state: ServerSessionState::Handshake,
            common: Common::new(
                Some(ChunkPacketizer::new(Arc::clone(&net_io))),
                event_producer,
                SessionType::Server,
                remote_addr,
            ),

            bytesio_data: BytesMut::new(),
            has_remaing_data: false,
            connect_properties: ConnectProperties::default(),
            gop_num,
            auth,
            is_publishing: false,
            last_message_time: tokio::time::Instant::now(),
            per_stream_max_bytes,
            callbacks,
            media_mode: RtmpStreamMode::Default,
        }
    }

    pub async fn run(&mut self) -> Result<(), SessionError> {
        loop {
            match self.state {
                ServerSessionState::Handshake => {
                    self.handshake().await?;
                }
                ServerSessionState::ReadChunk => {
                    self.read_parse_chunks().await?;
                }
                ServerSessionState::Play => {
                    self.play().await?;
                }
                ServerSessionState::DeleteStream => {
                    return Ok(());
                }
            }
        }
    }

    async fn teardown_active_stream(&mut self) -> Result<(), SessionError> {
        if self.app_name.is_empty() || self.stream_name.is_empty() {
            return Ok(());
        }

        if self.is_publishing {
            let hub_cleanup_result = self
                .common
                .unpublish_to_stream_hub(self.app_name.clone(), self.stream_name.clone())
                .await;
            if let Some(auth) = &self.auth {
                auth.on_unpublish(&self.app_name, &self.stream_name, self.query.as_deref())
                    .await;
            }
            if let Some(cb) = &self.callbacks.on_publisher_stop {
                cb();
            }
            self.is_publishing = false;
            hub_cleanup_result?;
        } else {
            let hub_cleanup_result = self
                .common
                .unsubscribe_from_stream_hub(self.app_name.clone(), self.stream_name.clone())
                .await;
            if let Some(auth) = &self.auth {
                auth.on_unplay(&self.app_name, &self.stream_name, self.query.as_deref())
                    .await;
            }
            if let Some(cb) = &self.callbacks.on_viewer_leave {
                cb();
            }
            hub_cleanup_result?;
        }

        Ok(())
    }

    pub async fn force_shutdown(&mut self) -> Result<(), SessionError> {
        let teardown_result = self.teardown_active_stream().await;
        let shutdown_result = self.io.lock().await.shutdown().await;
        self.state = ServerSessionState::DeleteStream;

        if let Err(err) = shutdown_result {
            return Err(SessionError {
                value: SessionErrorValue::BytesIOError(err),
            });
        }

        teardown_result
    }

    async fn handshake(&mut self) -> Result<(), SessionError> {
        let mut bytes_len = 0;

        // Timeout to prevent slowloris attacks holding handshake slots indefinitely
        let handshake_timeout = tokio::time::Duration::from_secs(10);
        let handshake_start = tokio::time::Instant::now();

        while bytes_len < handshake::define::RTMP_HANDSHAKE_SIZE + 1 {
            let remaining = handshake_timeout
                .checked_sub(handshake_start.elapsed())
                .ok_or(SessionError {
                    value: super::errors::SessionErrorValue::Timeout,
                })?;
            self.bytesio_data =
                match tokio::time::timeout(remaining, self.io.lock().await.read()).await {
                    Ok(result) => {
                        let data = result?;
                        // Cap single read to MAX_HANDSHAKE_BUFFER to prevent
                        // a large TCP read from blowing past the total limit.
                        if data.len() > MAX_HANDSHAKE_BUFFER {
                            tracing::warn!(
                                read_size = data.len(),
                                max = MAX_HANDSHAKE_BUFFER,
                                "RTMP handshake single read exceeded buffer limit"
                            );
                            return Err(SessionError {
                                value: super::errors::SessionErrorValue::Timeout,
                            });
                        }
                        data
                    }
                    Err(_) => {
                        return Err(SessionError {
                            value: super::errors::SessionErrorValue::Timeout,
                        })
                    }
                };
            bytes_len += self.bytesio_data.len();
            if bytes_len > MAX_HANDSHAKE_BUFFER {
                tracing::warn!(
                    "RTMP handshake buffer exceeded {MAX_HANDSHAKE_BUFFER} bytes, rejecting"
                );
                return Err(SessionError {
                    value: super::errors::SessionErrorValue::Timeout,
                });
            }
            self.handshaker.extend_data(&self.bytesio_data[..])?;
        }

        self.handshaker.handshake().await?;

        if matches!(self.handshaker.state(), ServerHandshakeState::Finish) {
            self.state = ServerSessionState::ReadChunk;
            let left_bytes = self.handshaker.get_remaining_bytes();
            if !left_bytes.is_empty() {
                self.unpacketizer.extend_data(&left_bytes[..])?;
                self.has_remaing_data = true;
            }
            tracing::info!("[ S->C ] [send_set_chunk_size] ");
            self.send_set_chunk_size().await?;
            return Ok(());
        }

        Ok(())
    }

    async fn read_parse_chunks(&mut self) -> Result<(), SessionError> {
        // Check overall session idle timeout to prevent slow-rate DoS.
        if self.last_message_time.elapsed() > SESSION_IDLE_TIMEOUT {
            tracing::warn!(
                "RTMP session idle timeout ({}s) for app={}, stream={}",
                SESSION_IDLE_TIMEOUT.as_secs(),
                self.app_name,
                self.stream_name,
            );
            return Err(SessionError {
                value: SessionErrorValue::Timeout,
            });
        }

        if !self.has_remaing_data {
            let read_result = {
                let mut io = self.io.lock().await;
                io.read_timeout(Duration::from_secs(2)).await
            };
            match read_result {
                Ok(data) => {
                    self.bytesio_data = data;
                }
                Err(err) => {
                    self.teardown_active_stream().await?;

                    return Err(SessionError {
                        value: SessionErrorValue::BytesIOError(err),
                    });
                }
            }

            self.unpacketizer.extend_data(&self.bytesio_data[..])?;
        }

        self.has_remaing_data = false;

        loop {
            match self.unpacketizer.read_chunks() {
                Ok(rv) => {
                    if let UnpackResult::Chunks(chunks) = rv {
                        for chunk_info in chunks {
                            // Reset idle timeout on each complete message
                            self.last_message_time = tokio::time::Instant::now();

                            let timestamp = chunk_info.message_header.timestamp;
                            let msg_stream_id = chunk_info.message_header.msg_streamd_id;

                            if let Some(mut msg) = MessageParser::new(chunk_info).parse()? {
                                self.process_messages(&mut msg, &msg_stream_id, &timestamp)
                                    .await?;
                            }
                        }
                    }
                }
                Err(err) => {
                    if matches!(err.value, UnpackErrorValue::CannotParse) {
                        self.teardown_active_stream().await?;
                        Err(err)?;
                    }
                    break;
                }
            }
        }
        Ok(())
    }

    async fn play(&mut self) -> Result<(), SessionError> {
        match self.common.send_channel_data().await {
            Ok(()) => {}
            Err(err) => {
                self.teardown_active_stream().await?;
                return Err(err);
            }
        }

        Ok(())
    }

    pub async fn send_set_chunk_size(&mut self) -> Result<(), SessionError> {
        let mut controlmessage =
            ProtocolControlMessagesWriter::new(AsyncBytesWriter::new(self.io.clone()));
        controlmessage.write_set_chunk_size(CHUNK_SIZE).await?;

        Ok(())
    }

    pub async fn process_messages(
        &mut self,
        rtmp_msg: &mut RtmpMessageData,
        msg_stream_id: &u32,
        timestamp: &u32,
    ) -> Result<(), SessionError> {
        match rtmp_msg {
            RtmpMessageData::Amf0Command(command) => {
                self.on_amf0_command_message(
                    msg_stream_id,
                    &command.command_name,
                    &command.transaction_id,
                    &command.command_object,
                    &mut command.others,
                )
                .await?;
            }
            RtmpMessageData::SetChunkSize { chunk_size } => {
                self.on_set_chunk_size(*chunk_size as usize);
            }
            RtmpMessageData::AudioData { data } => {
                if self.media_mode != RtmpStreamMode::VideoOnly {
                    self.common.on_audio_data(data, *timestamp)?;
                }
            }
            RtmpMessageData::VideoData { data } => {
                if self.media_mode != RtmpStreamMode::AudioOnly {
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
        stream_id: &u32,
        command_name: &Amf0ValueType,
        transaction_id: &Amf0ValueType,
        command_object: &Amf0ValueType,
        others: &mut Vec<Amf0ValueType>,
    ) -> Result<(), SessionError> {
        let empty_cmd_name = &String::new();
        let cmd_name = match command_name {
            Amf0ValueType::UTF8String(str) => str,
            _ => empty_cmd_name,
        };

        let transaction_id = match transaction_id {
            Amf0ValueType::Number(number) => number,
            _ => &0.0,
        };

        let empty_cmd_obj: IndexMap<String, Amf0ValueType> = IndexMap::new();
        let obj = match command_object {
            Amf0ValueType::Object(obj) => obj,
            _ => &empty_cmd_obj,
        };

        match cmd_name.as_str() {
            "connect" => {
                tracing::info!("[ S<-C ] [connect] ");
                self.on_connect(transaction_id, obj).await?;
            }
            "createStream" => {
                tracing::info!("[ S<-C ] [create stream] ");
                self.on_create_stream(transaction_id).await?;
            }
            "deleteStream" if !others.is_empty() => {
                let deleted_stream_id = match others.pop() {
                    Some(Amf0ValueType::Number(value)) => value,
                    _ => 0.0,
                };

                tracing::info!(
                    "[ S<-C ] [delete stream] app_name: {}, stream_name: {}",
                    self.app_name,
                    self.stream_name
                );

                self.on_delete_stream(transaction_id, &deleted_stream_id)
                    .await?;
                self.state = ServerSessionState::DeleteStream;
            }
            "play" => {
                tracing::info!(
                    "[ S<-C ] [play]  app_name: {}, stream_name: {}",
                    self.app_name,
                    self.stream_name
                );
                self.unpacketizer.session_type = config::SERVER_PULL;
                self.on_play(transaction_id, stream_id, others).await?;
            }
            "publish" => {
                self.unpacketizer.session_type = config::SERVER_PUSH;
                self.on_publish(transaction_id, stream_id, others).await?;
            }
            _ => {}
        }

        Ok(())
    }

    fn on_set_chunk_size(&mut self, chunk_size: usize) {
        // Clamp chunk_size to a safe range to prevent excessive buffer allocation
        // from malicious clients.
        const MIN_CHUNK_SIZE: usize = 128;
        const MAX_CHUNK_SIZE: usize = 65536;
        let clamped = chunk_size.clamp(MIN_CHUNK_SIZE, MAX_CHUNK_SIZE);
        if clamped == chunk_size {
            tracing::info!(
                "[ S<-C ] [set chunk size]  app_name: {}, stream_name: {}, chunk size: {}",
                self.app_name,
                self.stream_name,
                chunk_size
            );
        } else {
            tracing::warn!(
                "[ S<-C ] [set chunk size] clamped {} -> {} for app={}, stream={}",
                chunk_size,
                clamped,
                self.app_name,
                self.stream_name,
            );
        }
        self.unpacketizer.update_max_chunk_size(clamped);
    }

    fn parse_connect_properties(&mut self, command_obj: &IndexMap<String, Amf0ValueType>) {
        for (property, value) in command_obj {
            match property.as_str() {
                "app" => {
                    if let Amf0ValueType::UTF8String(app) = value {
                        self.connect_properties.app = Some(app.clone());
                    }
                }
                "flashVer" => {
                    if let Amf0ValueType::UTF8String(flash_ver) = value {
                        self.connect_properties.flash_ver = Some(flash_ver.clone());
                    }
                }
                "swfUrl" => {
                    if let Amf0ValueType::UTF8String(swf_url) = value {
                        self.connect_properties.swf_url = Some(swf_url.clone());
                    }
                }
                "tcUrl" => {
                    if let Amf0ValueType::UTF8String(tc_url) = value {
                        self.connect_properties.tc_url = Some(tc_url.clone());
                    }
                }
                "fpad" => {
                    if let Amf0ValueType::Boolean(fpad) = value {
                        self.connect_properties.fpad = Some(*fpad);
                    }
                }
                "audioCodecs" => {
                    if let Amf0ValueType::Number(audio_codecs) = value {
                        self.connect_properties.audio_codecs = Some(*audio_codecs);
                    }
                }
                "videoCodecs" => {
                    if let Amf0ValueType::Number(video_codecs) = value {
                        self.connect_properties.video_codecs = Some(*video_codecs);
                    }
                }
                "videoFunction" => {
                    if let Amf0ValueType::Number(video_function) = value {
                        self.connect_properties.video_function = Some(*video_function);
                    }
                }
                "pageUrl" => {
                    if let Amf0ValueType::UTF8String(page_url) = value {
                        self.connect_properties.page_url = Some(page_url.clone());
                    }
                }
                "objectEncoding" => {
                    if let Amf0ValueType::Number(object_encoding) = value {
                        self.connect_properties.object_encoding = Some(*object_encoding);
                    }
                }
                _ => {
                    tracing::warn!("unknown connect properties: {property}:{value:?}");
                }
            }
        }
    }

    async fn on_connect(
        &mut self,
        transaction_id: &f64,
        command_obj: &IndexMap<String, Amf0ValueType>,
    ) -> Result<(), SessionError> {
        self.parse_connect_properties(command_obj);
        tracing::info!("connect properties: {:?}", self.connect_properties);
        let mut control_message =
            ProtocolControlMessagesWriter::new(AsyncBytesWriter::new(self.io.clone()));
        tracing::info!("[ S->C ] [set window_acknowledgement_size]");
        control_message
            .write_window_acknowledgement_size(define::WINDOW_ACKNOWLEDGEMENT_SIZE)
            .await?;

        tracing::info!("[ S->C ] [set set_peer_bandwidth]",);
        control_message
            .write_set_peer_bandwidth(
                define::PEER_BANDWIDTH,
                define::peer_bandwidth_limit_type::DYNAMIC,
            )
            .await?;

        let obj_encoding = command_obj.get("objectEncoding");
        let encoding = match obj_encoding {
            Some(Amf0ValueType::Number(encoding)) => encoding,
            _ => &define::OBJENCODING_AMF0,
        };

        let app_name = command_obj.get("app");
        self.app_name = match app_name {
            Some(Amf0ValueType::UTF8String(app)) => {
                if app.len() > 256 {
                    return Err(SessionError {
                        value: SessionErrorValue::NoAppName,
                    });
                }
                app.split_once(['?', '/'])
                    .map_or_else(|| app.clone(), |(app, _)| app.to_string())
            }
            _ => {
                return Err(SessionError {
                    value: SessionErrorValue::NoAppName,
                });
            }
        };

        let mut netconnection = NetConnection::new(Arc::clone(&self.io));
        tracing::info!("[ S->C ] [set connect_response]",);
        netconnection
            .write_connect_response(ConnectResponse {
                transaction_id: *transaction_id,
                fmsver: define::FMSVER,
                capabilities: define::CAPABILITIES,
                code: "NetConnection.Connect.Success",
                level: define::LEVEL,
                description: "Connection Succeeded.",
                encoding: *encoding,
            })
            .await?;

        Ok(())
    }

    async fn on_create_stream(&mut self, transaction_id: &f64) -> Result<(), SessionError> {
        let mut netconnection = NetConnection::new(Arc::clone(&self.io));
        netconnection
            .write_create_stream_response(transaction_id, &define::STREAM_ID)
            .await?;

        tracing::info!(
            "[ S->C ] [create_stream_response]  app_name: {}",
            self.app_name,
        );

        Ok(())
    }

    async fn on_delete_stream(
        &mut self,
        transaction_id: &f64,
        stream_id: &f64,
    ) -> Result<(), SessionError> {
        self.teardown_active_stream().await?;

        let mut netstream = NetStreamWriter::new(Arc::clone(&self.io));
        netstream
            .write_on_status(
                transaction_id,
                "status",
                "NetStream.DeleteStream.Success",
                "",
            )
            .await?;

        //self.unsubscribe_from_channels().await?;
        tracing::info!(
            "[ S->C ] [delete stream success]  app_name: {}, stream_name: {}",
            self.app_name,
            self.stream_name
        );
        tracing::trace!("{stream_id}");

        Ok(())
    }

    fn get_request_url(&self, raw_stream_name: &str) -> String {
        if let Some(tc_url) = &self.connect_properties.tc_url {
            format!("{tc_url}/{raw_stream_name}")
        } else {
            format!("{}/{}", self.app_name.clone(), raw_stream_name)
        }
    }

    async fn on_play(
        &mut self,
        transaction_id: &f64,
        stream_id: &u32,
        other_values: &mut Vec<Amf0ValueType>,
    ) -> Result<(), SessionError> {
        if other_values.len() > 4 {
            return Err(SessionError {
                value: SessionErrorValue::InvalidAmf0ValueCount,
            });
        }

        let stream_name = remove_first_string(other_values);
        let start = remove_first_number(other_values);
        let duration = remove_first_number(other_values);
        let reset = remove_first_bool(other_values);

        if !other_values.is_empty() {
            return Err(SessionError {
                value: SessionErrorValue::InvalidAmf0ValueCount,
            });
        }

        let stream_name = stream_name.ok_or(SessionError {
            value: SessionErrorValue::NoStreamName,
        })?;
        if stream_name.len() > MAX_RTMP_STREAM_NAME_WITH_QUERY_LEN {
            return Err(SessionError {
                value: SessionErrorValue::NoStreamName,
            });
        }

        self.begin_play_stream(
            transaction_id,
            stream_id,
            stream_name,
            start,
            duration,
            reset,
        )
        .await
    }

    async fn begin_play_stream(
        &mut self,
        transaction_id: &f64,
        stream_id: &u32,
        raw_stream_name: String,
        start: Option<f64>,
        duration: Option<f64>,
        reset: Option<bool>,
    ) -> Result<(), SessionError> {
        let mut event_messages = EventMessagesWriter::new(AsyncBytesWriter::new(self.io.clone()));
        event_messages.write_stream_begin(*stream_id).await?;
        tracing::info!(
            "[ S->C ] [stream begin]  app_name: {}, stream_name: {}",
            self.app_name,
            self.stream_name
        );
        tracing::trace!(
            "{} {} {}",
            start.is_some(),
            duration.is_some(),
            reset.is_some()
        );

        let mut netstream = NetStreamWriter::new(Arc::clone(&self.io));
        netstream
            .write_on_status(transaction_id, "status", "NetStream.Play.Reset", "reset")
            .await?;

        netstream
            .write_on_status(
                transaction_id,
                "status",
                "NetStream.Play.Start",
                "play start",
            )
            .await?;

        netstream
            .write_on_status(
                transaction_id,
                "status",
                "NetStream.Data.Start",
                "data start.",
            )
            .await?;

        netstream
            .write_on_status(
                transaction_id,
                "status",
                "NetStream.Play.PublishNotify",
                "play publish notify.",
            )
            .await?;

        event_messages.write_stream_is_record(*stream_id).await?;

        (self.stream_name, self.query) =
            RtmpUrlParser::parse_stream_name_with_query(&raw_stream_name);
        if let Some(auth) = &self.auth {
            auth.on_play(&self.app_name, &self.stream_name, self.query.as_deref())
                .await
                .map_err(|e| SessionError {
                    value: SessionErrorValue::AuthFailed(e.to_string()),
                })?;
        }

        let query = self
            .query
            .as_ref()
            .map_or_else(|| String::from("none"), std::clone::Clone::clone);

        tracing::info!(
            "[ S->C ] [stream is record]  app_name: {}, stream_name: {}, query: {}",
            self.app_name,
            self.stream_name,
            query
        );

        // Publish/play command data is now available, so the session request URL can be finalized.
        self.common.request_url = self.get_request_url(&raw_stream_name);
        self.common
            .subscribe_from_stream_hub(self.app_name.clone(), self.stream_name.clone())
            .await?;

        self.state = ServerSessionState::Play;

        // Notify viewer join via callback
        if let Some(cb) = &self.callbacks.on_viewer_join {
            cb();
        }

        Ok(())
    }

    async fn on_publish(
        &mut self,
        transaction_id: &f64,
        stream_id: &u32,
        other_values: &mut Vec<Amf0ValueType>,
    ) -> Result<(), SessionError> {
        let length = other_values.len();

        if length < 2 {
            return Err(SessionError {
                value: SessionErrorValue::InvalidAmf0ValueCount,
            });
        }

        let stream_name_with_query = match other_values.remove(0) {
            Amf0ValueType::UTF8String(val) => {
                if val.len() > MAX_RTMP_STREAM_NAME_WITH_QUERY_LEN {
                    return Err(SessionError {
                        value: SessionErrorValue::InvalidAmf0ValueCount,
                    });
                }
                val
            }
            _ => {
                return Err(SessionError {
                    value: SessionErrorValue::InvalidAmf0ValueCount,
                });
            }
        };

        if stream_name_with_query.is_empty() {
            let Some(swf_url) = self.connect_properties.swf_url.as_ref() else {
                return Err(SessionError {
                    value: SessionErrorValue::NoStreamName,
                });
            };

            let mut url = RtmpUrlParser::new(swf_url.clone());
            url.parse_url().map_err(|e| SessionError {
                value: SessionErrorValue::AuthFailed(format!("invalid swf_url: {e}")),
            })?;
            if url.stream_name.is_empty() {
                return Err(SessionError {
                    value: SessionErrorValue::NoStreamName,
                });
            }
            self.stream_name = url.stream_name;
            self.query = url.query;
        } else {
            (self.stream_name, self.query) =
                RtmpUrlParser::parse_stream_name_with_query(&stream_name_with_query);
        }
        if let Some(auth) = &self.auth {
            let rewrite = auth
                .on_publish(&self.app_name, &self.stream_name, self.query.as_deref())
                .await
                .map_err(|e| SessionError {
                    value: SessionErrorValue::AuthFailed(e.to_string()),
                })?;

            // Apply identifier rewrite if the auth callback resolved a JWT token
            // to a canonical (room_id, media_id) pair.
            if let Some(rewrite) = rewrite {
                tracing::info!(
                    "Auth rewrite: ({}, {}) -> ({}, {})",
                    self.app_name,
                    self.stream_name,
                    rewrite.app_name,
                    rewrite.stream_name
                );
                self.app_name = rewrite.app_name;
                self.stream_name = rewrite.stream_name;
                self.media_mode = rewrite.media_mode;
            }
        }

        // Publish/play command data is now available, so the session request URL can be finalized.
        self.common.request_url = self.get_request_url(&stream_name_with_query);

        let Amf0ValueType::UTF8String(_) = other_values.remove(0) else {
            return Err(SessionError {
                value: SessionErrorValue::InvalidAmf0ValueCount,
            });
        };

        let query = self
            .query
            .as_ref()
            .map_or_else(|| String::from("none"), std::clone::Clone::clone);

        tracing::info!(
            "[ S<-C ] [publish]  app_name: {}, stream_name: {}, query: {}",
            self.app_name,
            self.stream_name,
            query
        );

        tracing::info!(
            "[ S->C ] [stream begin]  app_name: {}, stream_name: {}, query: {}",
            self.app_name,
            self.stream_name,
            query
        );

        // Helper closure for cleanup on error - use rollback for proper Redis cleanup
        let cleanup_auth = || async {
            if let Some(auth) = &self.auth {
                // Use on_publish_rollback instead of on_unpublish because:
                // 1. on_publish registered the publisher in Redis
                // 2. on_unpublish intentionally does NOT clean up Redis (PublisherManager does)
                // 3. When StreamHub fails, PublisherManager never gets called
                // 4. So we need rollback to clean up Redis immediately
                auth.on_publish_rollback(&self.app_name, &self.stream_name, self.query.as_deref())
                    .await;
            }
        };

        let mut event_messages = EventMessagesWriter::new(AsyncBytesWriter::new(self.io.clone()));
        if let Err(e) = event_messages.write_stream_begin(*stream_id).await {
            tracing::error!(
                "Failed to send stream_begin after successful auth, cleaning up: {}",
                e
            );
            cleanup_auth().await;
            return Err(e.into());
        }

        let mut netstream = NetStreamWriter::new(Arc::clone(&self.io));
        if let Err(e) = netstream
            .write_on_status(transaction_id, "status", "NetStream.Publish.Start", "")
            .await
        {
            tracing::error!(
                "Failed to send NetStream.Publish.Start after successful auth, cleaning up: {}",
                e
            );
            cleanup_auth().await;
            return Err(e.into());
        }
        tracing::info!(
            "[ S->C ] [NetStream.Publish.Start]  app_name: {}, stream_name: {}",
            self.app_name,
            self.stream_name
        );

        if let Err(e) = self
            .common
            .publish_to_stream_hub(
                self.app_name.clone(),
                self.stream_name.clone(),
                self.gop_num,
                self.per_stream_max_bytes,
            )
            .await
        {
            tracing::error!(
                "Failed to publish to StreamHub after successful auth, cleaning up: {}",
                e
            );
            cleanup_auth().await;
            return Err(e);
        }

        self.is_publishing = true;

        // Notify publisher start via callback
        if let Some(cb) = &self.callbacks.on_publisher_start {
            cb();
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use bytes::{Bytes, BytesMut};
    use std::collections::VecDeque;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex as StdMutex,
    };
    use tokio::time::timeout;

    use crate::bytesio::bytesio_errors::{BytesIOError, BytesIOErrorValue};
    use crate::bytesio::net_io::{NetType, TNetIO};
    use crate::streamhub::define::STREAM_HUB_EVENT_CHANNEL_CAPACITY;

    struct ChunkedNetIo {
        reads: VecDeque<BytesMut>,
        shutdowns: Arc<AtomicUsize>,
    }

    impl ChunkedNetIo {
        fn new(reads: Vec<BytesMut>) -> Self {
            Self {
                reads: reads.into(),
                shutdowns: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl TNetIO for ChunkedNetIo {
        async fn write(&mut self, _bytes: Bytes) -> Result<(), BytesIOError> {
            Ok(())
        }

        async fn read(&mut self) -> Result<BytesMut, BytesIOError> {
            self.reads.pop_front().ok_or(BytesIOError {
                value: BytesIOErrorValue::ConnectionClosed,
            })
        }

        async fn read_timeout(&mut self, _duration: Duration) -> Result<BytesMut, BytesIOError> {
            self.read().await
        }

        async fn shutdown(&mut self) -> Result<(), BytesIOError> {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn get_net_type(&self) -> NetType {
            NetType::TCP
        }
    }

    struct RecordingAuthCallback {
        events: Arc<StdMutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl AuthCallback for RecordingAuthCallback {
        async fn on_publish(
            &self,
            _app_name: &str,
            _stream_name: &str,
            _query: Option<&str>,
        ) -> Result<
            Option<crate::rtmp::auth::AuthPublishRewrite>,
            Box<dyn std::error::Error + Send + Sync>,
        > {
            Ok(None)
        }

        async fn on_play(
            &self,
            _app_name: &str,
            _stream_name: &str,
            _query: Option<&str>,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }

        async fn on_unpublish(&self, _app_name: &str, _stream_name: &str, _query: Option<&str>) {
            self.events
                .lock()
                .expect("lock auth events")
                .push("unpublish");
        }

        async fn on_unplay(&self, _app_name: &str, _stream_name: &str, _query: Option<&str>) {
            self.events.lock().expect("lock auth events").push("unplay");
        }
    }

    fn build_test_session(
        event_sender: StreamHubEventSender,
        auth: Option<Arc<dyn AuthCallback>>,
        callbacks: Arc<StreamEventCallbacks>,
    ) -> ServerSession {
        let io: Box<dyn TNetIO + Send + Sync> = Box::new(ChunkedNetIo::new(vec![]));
        let io = Arc::new(Mutex::new(io));

        ServerSession {
            app_name: "live".to_string(),
            stream_name: "room/stream".to_string(),
            query: Some("token=abc".to_string()),
            io: Arc::clone(&io),
            handshaker: HandshakeServer::new(Arc::clone(&io)),
            unpacketizer: ChunkUnpacketizer::new(),
            state: ServerSessionState::ReadChunk,
            bytesio_data: BytesMut::new(),
            has_remaing_data: false,
            connect_properties: ConnectProperties::default(),
            common: Common::new(None, event_sender, SessionType::Server, None),
            gop_num: 1,
            auth,
            is_publishing: false,
            last_message_time: tokio::time::Instant::now(),
            per_stream_max_bytes: None,
            callbacks,
            media_mode: RtmpStreamMode::Default,
        }
    }

    fn build_c0c1() -> Vec<u8> {
        let mut data = Vec::with_capacity(1 + handshake::define::RTMP_HANDSHAKE_SIZE);
        data.push(handshake::define::RTMP_VERSION);
        data.extend_from_slice(&12345_u32.to_be_bytes());
        data.extend_from_slice(&[0u8; 4]);
        data.extend(
            (0..(handshake::define::RTMP_HANDSHAKE_SIZE - 8))
                .map(|i| u8::try_from(i % 255).expect("modulo 255 value must fit in u8")),
        );
        data
    }

    #[tokio::test]
    async fn test_server_session_handshake_accepts_fragmented_c0_c1() {
        let c0c1 = build_c0c1();
        let split_at = handshake::define::RTMP_HANDSHAKE_SIZE;
        let io: Box<dyn TNetIO + Send + Sync> = Box::new(ChunkedNetIo::new(vec![
            BytesMut::from(&c0c1[..split_at]),
            BytesMut::from(&c0c1[split_at..]),
        ]));
        let io = Arc::new(Mutex::new(io));
        let (event_sender, _event_receiver) =
            tokio::sync::mpsc::channel(STREAM_HUB_EVENT_CHANNEL_CAPACITY);

        let mut session = ServerSession {
            app_name: String::new(),
            stream_name: String::new(),
            query: None,
            io: Arc::clone(&io),
            handshaker: HandshakeServer::new(Arc::clone(&io)),
            unpacketizer: ChunkUnpacketizer::new(),
            state: ServerSessionState::Handshake,
            bytesio_data: BytesMut::new(),
            has_remaing_data: false,
            connect_properties: ConnectProperties::default(),
            common: Common::new(None, event_sender, SessionType::Server, None),
            gop_num: 1,
            auth: None,
            is_publishing: false,
            last_message_time: tokio::time::Instant::now(),
            per_stream_max_bytes: None,
            callbacks: Arc::new(StreamEventCallbacks::default()),
            media_mode: RtmpStreamMode::Default,
        };

        let result = timeout(Duration::from_secs(1), session.handshake()).await;
        assert!(
            result.is_ok(),
            "fragmented handshake should complete promptly"
        );
        assert!(
            matches!(result.expect("timeout should not fire"), Ok(())),
            "fragmented handshake should not fail when C0/C1 arrives across TCP frames"
        );
        assert!(matches!(session.state, ServerSessionState::Handshake));
        assert!(matches!(
            session.handshaker.state(),
            ServerHandshakeState::ReadC2
        ));
    }

    #[tokio::test]
    async fn test_teardown_publish_runs_external_cleanup_even_when_streamhub_unpublish_fails() {
        let (event_sender, event_receiver) =
            tokio::sync::mpsc::channel(STREAM_HUB_EVENT_CHANNEL_CAPACITY);
        drop(event_receiver);

        let auth_events = Arc::new(StdMutex::new(Vec::new()));
        let auth: Arc<dyn AuthCallback> = Arc::new(RecordingAuthCallback {
            events: Arc::clone(&auth_events),
        });
        let publisher_stop_count = Arc::new(AtomicUsize::new(0));
        let callbacks = Arc::new(StreamEventCallbacks {
            on_publisher_stop: Some({
                let publisher_stop_count = Arc::clone(&publisher_stop_count);
                Arc::new(move || {
                    publisher_stop_count.fetch_add(1, Ordering::SeqCst);
                })
            }),
            ..StreamEventCallbacks::default()
        });

        let mut session = build_test_session(event_sender, Some(auth), callbacks);
        session.is_publishing = true;

        let result = session.teardown_active_stream().await;

        assert!(matches!(
            result,
            Err(SessionError {
                value: SessionErrorValue::ChannelError(_)
            })
        ));
        assert!(
            !session.is_publishing,
            "publishing state should reflect the torn-down local session even when StreamHub cleanup fails"
        );
        assert_eq!(
            auth_events.lock().expect("lock auth events").as_slice(),
            ["unpublish"],
            "auth cleanup must still run so external tracking is cleared when StreamHub cleanup fails"
        );
        assert_eq!(
            publisher_stop_count.load(Ordering::SeqCst),
            1,
            "publisher stop callback must still run when StreamHub cleanup fails"
        );
    }

    #[tokio::test]
    async fn test_teardown_play_runs_external_cleanup_even_when_streamhub_unsubscribe_fails() {
        let (event_sender, event_receiver) =
            tokio::sync::mpsc::channel(STREAM_HUB_EVENT_CHANNEL_CAPACITY);
        drop(event_receiver);

        let auth_events = Arc::new(StdMutex::new(Vec::new()));
        let auth: Arc<dyn AuthCallback> = Arc::new(RecordingAuthCallback {
            events: Arc::clone(&auth_events),
        });
        let viewer_leave_count = Arc::new(AtomicUsize::new(0));
        let callbacks = Arc::new(StreamEventCallbacks {
            on_viewer_leave: Some({
                let viewer_leave_count = Arc::clone(&viewer_leave_count);
                Arc::new(move || {
                    viewer_leave_count.fetch_add(1, Ordering::SeqCst);
                })
            }),
            ..StreamEventCallbacks::default()
        });

        let mut session = build_test_session(event_sender, Some(auth), callbacks);

        let result = session.teardown_active_stream().await;

        assert!(matches!(
            result,
            Err(SessionError {
                value: SessionErrorValue::ChannelError(_)
            })
        ));
        assert_eq!(
            auth_events.lock().expect("lock auth events").as_slice(),
            ["unplay"],
            "auth cleanup must still run so viewer bookkeeping is cleared when StreamHub cleanup fails"
        );
        assert_eq!(
            viewer_leave_count.load(Ordering::SeqCst),
            1,
            "viewer leave callback must still run when StreamHub cleanup fails"
        );
    }

    #[tokio::test]
    async fn test_force_shutdown_closes_transport_and_transitions_to_delete_stream() {
        let (event_sender, _event_receiver) =
            tokio::sync::mpsc::channel(STREAM_HUB_EVENT_CHANNEL_CAPACITY);
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let io: Box<dyn TNetIO + Send + Sync> = Box::new(ChunkedNetIo {
            reads: VecDeque::new(),
            shutdowns: Arc::clone(&shutdowns),
        });
        let io = Arc::new(Mutex::new(io));

        let mut session = ServerSession {
            app_name: "live".to_string(),
            stream_name: "room/stream".to_string(),
            query: Some("token=abc".to_string()),
            io: Arc::clone(&io),
            handshaker: HandshakeServer::new(Arc::clone(&io)),
            unpacketizer: ChunkUnpacketizer::new(),
            state: ServerSessionState::ReadChunk,
            bytesio_data: BytesMut::new(),
            has_remaing_data: false,
            connect_properties: ConnectProperties::default(),
            common: Common::new(None, event_sender, SessionType::Server, None),
            gop_num: 1,
            auth: None,
            is_publishing: false,
            last_message_time: tokio::time::Instant::now(),
            per_stream_max_bytes: None,
            callbacks: Arc::new(StreamEventCallbacks::default()),
            media_mode: RtmpStreamMode::Default,
        };

        let result = session.force_shutdown().await;

        assert!(
            result.is_ok(),
            "force shutdown should succeed without active stream"
        );
        assert_eq!(
            shutdowns.load(Ordering::SeqCst),
            1,
            "force shutdown must close the underlying transport"
        );
        assert!(
            matches!(session.state, ServerSessionState::DeleteStream),
            "force shutdown must transition the session out of the read loop"
        );
    }
}
