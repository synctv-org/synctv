use crate::rtmp::auth::AuthCallback;
use crate::rtmp::callbacks::StreamEventCallbacks;
use crate::rtmp::chunk::{errors::UnpackErrorValue, packetizer::ChunkPacketizer};

use {
    super::{
        common::Common,
        define,
        define::SessionType,
        errors::{SessionError, SessionErrorValue},
    },
    crate::rtmp::{
        chunk::{
            define::CHUNK_SIZE,
            unpacketizer::{ChunkUnpacketizer, UnpackResult},
        },
        config, handshake,
        handshake::{define::ServerHandshakeState, handshake_server::HandshakeServer},
        messages::{define::RtmpMessageData, parser::MessageParser},
        netconnection::writer::{ConnectProperties, NetConnection},
        netstream::writer::NetStreamWriter,
        protocol_control_messages::writer::ProtocolControlMessagesWriter,
        user_control_messages::writer::EventMessagesWriter,
        utils::RtmpUrlParser,
    },
    bytes::BytesMut,
    crate::bytesio::{
        bytes_writer::AsyncBytesWriter,
        bytesio::{TNetIO, TcpIO},
    },
    indexmap::IndexMap,
    std::{sync::Arc, time::Duration},
    crate::streamhub::define::StreamHubEventSender,
    tokio::{net::TcpStream, sync::Mutex},
    crate::flv::amf0::Amf0ValueType,
};

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
    /*configure how many gops will be cached.*/
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
        let remote_addr = if let Ok(addr) = stream.peer_addr() {
            tracing::info!("server session: {addr}");
            Some(addr)
        } else {
            None
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

    async fn handshake(&mut self) -> Result<(), SessionError> {
        let mut bytes_len = 0;

        // Timeout to prevent slowloris attacks holding handshake slots indefinitely
        let handshake_timeout = tokio::time::Duration::from_secs(10);
        let handshake_start = tokio::time::Instant::now();
        // M-1: Maximum buffer size during handshake to prevent memory exhaustion
        const MAX_HANDSHAKE_BUFFER: usize = 8192;

        while bytes_len < handshake::define::RTMP_HANDSHAKE_SIZE {
            let remaining = handshake_timeout.checked_sub(handshake_start.elapsed())
                .ok_or(SessionError {
                    value: super::errors::SessionErrorValue::Timeout,
                })?;
            self.bytesio_data = match tokio::time::timeout(remaining, self.io.lock().await.read()).await {
                Ok(result) => result?,
                Err(_) => return Err(SessionError {
                    value: super::errors::SessionErrorValue::Timeout,
                }),
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
        // M-2: Check overall session idle timeout (prevents slow-rate DoS)
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
            match self
                .io
                .lock()
                .await
                .read_timeout(Duration::from_secs(2))
                .await
            {
                Ok(data) => {
                    self.bytesio_data = data;
                }
                Err(err) => {
                    if self.is_publishing {
                        if let Some(auth) = &self.auth {
                            auth.on_unpublish(
                                &self.app_name,
                                &self.stream_name,
                                self.query.as_deref(),
                            )
                            .await;
                        }
                        self.common
                            .unpublish_to_stream_hub(self.app_name.clone(), self.stream_name.clone())
                            .await?;
                        if let Some(cb) = &self.callbacks.on_publisher_stop {
                            cb();
                        }
                    } else {
                        if let Some(auth) = &self.auth {
                            auth.on_unplay(
                                &self.app_name,
                                &self.stream_name,
                                self.query.as_deref(),
                            )
                            .await;
                        }
                        self.common
                            .unsubscribe_from_stream_hub(self.app_name.clone(), self.stream_name.clone())
                            .await?;
                        if let Some(cb) = &self.callbacks.on_viewer_leave {
                            cb();
                        }
                    }

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
                        if self.is_publishing {
                            if let Some(auth) = &self.auth {
                                auth.on_unpublish(
                                    &self.app_name,
                                    &self.stream_name,
                                    self.query.as_deref(),
                                )
                                .await;
                            }
                            self.common
                                .unpublish_to_stream_hub(self.app_name.clone(), self.stream_name.clone())
                                .await?;
                            if let Some(cb) = &self.callbacks.on_publisher_stop {
                                cb();
                            }
                        } else {
                            if let Some(auth) = &self.auth {
                                auth.on_unplay(
                                    &self.app_name,
                                    &self.stream_name,
                                    self.query.as_deref(),
                                )
                                .await;
                            }
                            self.common
                                .unsubscribe_from_stream_hub(self.app_name.clone(), self.stream_name.clone())
                                .await?;
                            if let Some(cb) = &self.callbacks.on_viewer_leave {
                                cb();
                            }
                        }
                        return Err(err)?;
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
                if let Some(auth) = &self.auth {
                    auth.on_unplay(
                        &self.app_name,
                        &self.stream_name,
                        self.query.as_deref(),
                    )
                    .await;
                }
                self.common
                    .unsubscribe_from_stream_hub(self.app_name.clone(), self.stream_name.clone())
                    .await?;
                if let Some(cb) = &self.callbacks.on_viewer_leave {
                    cb();
                }
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
            RtmpMessageData::Amf0Command {
                command_name,
                transaction_id,
                command_object,
                others,
            } => {
                self.on_amf0_command_message(
                    msg_stream_id,
                    command_name,
                    transaction_id,
                    command_object,
                    others,
                )
                .await?;
            }
            RtmpMessageData::SetChunkSize { chunk_size } => {
                self.on_set_chunk_size(*chunk_size as usize)?;
            }
            RtmpMessageData::AudioData { data } => {
                self.common.on_audio_data(data, timestamp).await?;
            }
            RtmpMessageData::VideoData { data } => {
                self.common.on_video_data(data, timestamp).await?;
            }
            RtmpMessageData::AmfData { raw_data } => {
                self.common.on_meta_data(raw_data, timestamp).await?;
            }

            _ => {}
        }
        Ok(())
    }

    pub async fn on_amf0_command_message(
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
            "deleteStream"
                if !others.is_empty() => {
                    let stream_id = match others.pop() {
                        Some(Amf0ValueType::Number(streamid)) => streamid,
                        _ => 0.0,
                    };

                    tracing::info!(
                        "[ S<-C ] [delete stream] app_name: {}, stream_name: {}",
                        self.app_name,
                        self.stream_name
                    );

                    self.on_delete_stream(transaction_id, &stream_id).await?;
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

    fn on_set_chunk_size(&mut self, chunk_size: usize) -> Result<(), SessionError> {
        // L-3: Clamp chunk_size to safe range [128, 65536] to prevent
        // excessive buffer allocation from malicious clients.
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
        Ok(())
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
                // the value can weirdly have the query params, lets just remove it
                // example: live/stream?token=123
                app.split(&['?', '/']).next().unwrap_or(app).to_string()
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
            .write_connect_response(
                transaction_id,
                define::FMSVER,
                &define::CAPABILITIES,
                &String::from("NetConnection.Connect.Success"),
                define::LEVEL,
                &String::from("Connection Succeeded."),
                encoding,
            )
            .await?;

        Ok(())
    }

    pub async fn on_create_stream(&mut self, transaction_id: &f64) -> Result<(), SessionError> {
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

    pub async fn on_delete_stream(
        &mut self,
        transaction_id: &f64,
        stream_id: &f64,
    ) -> Result<(), SessionError> {
        if self.is_publishing {
            self.common
                .unpublish_to_stream_hub(self.app_name.clone(), self.stream_name.clone())
                .await?;

            if let Some(auth) = &self.auth {
                auth.on_unpublish(
                    &self.app_name,
                    &self.stream_name,
                    self.query.as_deref(),
                )
                .await;
            }

            // Fixed #116: Notify publisher disconnect via callback
            if let Some(cb) = &self.callbacks.on_publisher_stop {
                cb();
            }
        } else {
            self.common
                .unsubscribe_from_stream_hub(self.app_name.clone(), self.stream_name.clone())
                .await?;

            if let Some(auth) = &self.auth {
                auth.on_unplay(
                    &self.app_name,
                    &self.stream_name,
                    self.query.as_deref(),
                )
                .await;
            }

            // Fixed #116: Notify viewer disconnect via callback
            if let Some(cb) = &self.callbacks.on_viewer_leave {
                cb();
            }
        }

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

    fn get_request_url(&mut self, raw_stream_name: String) -> String {
        if let Some(tc_url) = &self.connect_properties.tc_url {
            format!("{tc_url}/{raw_stream_name}")
        } else {
            format!("{}/{}", self.app_name.clone(), raw_stream_name)
        }
    }

    #[allow(clippy::never_loop)]
    pub async fn on_play(
        &mut self,
        transaction_id: &f64,
        stream_id: &u32,
        other_values: &mut Vec<Amf0ValueType>,
    ) -> Result<(), SessionError> {
        let length = other_values.len() as u8;
        let mut index: u8 = 0;

        let mut stream_name: Option<String> = None;
        let mut start: Option<f64> = None;
        let mut duration: Option<f64> = None;
        let mut reset: Option<bool> = None;

        loop {
            if index >= length {
                break;
            }
            index += 1;
            stream_name = match other_values.remove(0) {
                Amf0ValueType::UTF8String(val) => Some(val),
                _ => None,
            };

            if index >= length {
                break;
            }
            index += 1;
            start = match other_values.remove(0) {
                Amf0ValueType::Number(val) => Some(val),
                _ => None,
            };

            if index >= length {
                break;
            }
            index += 1;
            duration = match other_values.remove(0) {
                Amf0ValueType::Number(val) => Some(val),
                _ => None,
            };

            if index >= length {
                break;
            }
            //index = index + 1;
            reset = match other_values.remove(0) {
                Amf0ValueType::Boolean(val) => Some(val),
                _ => None,
            };
            break;
        }

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

        let raw_stream_name = stream_name.ok_or(SessionError {
            value: SessionErrorValue::NoStreamName,
        })?;

        if raw_stream_name.len() > 256 {
            return Err(SessionError {
                value: SessionErrorValue::NoStreamName,
            });
        }

        (self.stream_name, self.query) =
            RtmpUrlParser::parse_stream_name_with_query(&raw_stream_name);
        if let Some(auth) = &self.auth {
            auth.on_play(
                &self.app_name,
                &self.stream_name,
                self.query.as_deref(),
            )
            .await
            .map_err(|e| SessionError {
                value: SessionErrorValue::AuthFailed(e.to_string()),
            })?;
        }

        let query = if let Some(query_val) = &self.query {
            query_val.clone()
        } else {
            String::from("none")
        };

        tracing::info!(
            "[ S->C ] [stream is record]  app_name: {}, stream_name: {}, query: {}",
            self.app_name,
            self.stream_name,
            query
        );

        /*Now it can update the request url*/
        self.common.request_url = self.get_request_url(raw_stream_name);
        self.common
            .subscribe_from_stream_hub(self.app_name.clone(), self.stream_name.clone())
            .await?;

        self.state = ServerSessionState::Play;

        // Fixed #116: Notify viewer join via callback
        if let Some(cb) = &self.callbacks.on_viewer_join {
            cb();
        }

        Ok(())
    }

    pub async fn on_publish(
        &mut self,
        transaction_id: &f64,
        stream_id: &u32,
        other_values: &mut Vec<Amf0ValueType>,
    ) -> Result<(), SessionError> {
        let length = other_values.len();

        if length < 2 {
            return Err(SessionError {
                value: SessionErrorValue::Amf0ValueCountNotCorrect,
            });
        }

        let stream_name_with_query = match other_values.remove(0) {
            Amf0ValueType::UTF8String(val) => {
                if val.len() > 256 {
                    return Err(SessionError {
                        value: SessionErrorValue::Amf0ValueCountNotCorrect,
                    });
                }
                val
            }
            _ => {
                return Err(SessionError {
                    value: SessionErrorValue::Amf0ValueCountNotCorrect,
                });
            }
        };

        if stream_name_with_query.is_empty() {
            tracing::warn!("stream_name_with_query is empty, extracing info from swf_url instead...");
            let mut url = RtmpUrlParser::new(
                self.connect_properties
                    .swf_url
                    .clone()
                    .unwrap_or_default(),
            );

            match url.parse_url() {
                Ok(()) => {
                    self.stream_name = url.stream_name;
                    self.query = url.query;
                }
                Err(e) => {
                    tracing::warn!("Failed to parse swf_url: {e}");
                }
            }
        } else {
            (self.stream_name, self.query) =
                RtmpUrlParser::parse_stream_name_with_query(&stream_name_with_query);
        }
        if let Some(auth) = &self.auth {
            let rewrite = auth.on_publish(
                &self.app_name,
                &self.stream_name,
                self.query.as_deref(),
            )
            .await
            .map_err(|e| SessionError {
                value: SessionErrorValue::AuthFailed(e.to_string()),
            })?;

            // Apply identifier rewrite if the auth callback resolved a JWT token
            // to a canonical (room_id, media_id) pair.
            if let Some(rewrite) = rewrite {
                tracing::info!(
                    "Auth rewrite: ({}, {}) -> ({}, {})",
                    self.app_name, self.stream_name,
                    rewrite.app_name, rewrite.stream_name
                );
                self.app_name = rewrite.app_name;
                self.stream_name = rewrite.stream_name;
            }
        }

        /*Now it can update the request url*/
        self.common.request_url = self.get_request_url(stream_name_with_query);

        let _ = match other_values.remove(0) {
            Amf0ValueType::UTF8String(val) => val,
            _ => {
                return Err(SessionError {
                    value: SessionErrorValue::Amf0ValueCountNotCorrect,
                });
            }
        };

        let query = if let Some(query_val) = &self.query {
            query_val.clone()
        } else {
            String::from("none")
        };

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
                auth.on_publish_rollback(
                    &self.app_name,
                    &self.stream_name,
                    self.query.as_deref(),
                )
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

        // Fixed #116: Notify publisher start via callback
        if let Some(cb) = &self.callbacks.on_publisher_start {
            cb();
        }

        Ok(())
    }
}
