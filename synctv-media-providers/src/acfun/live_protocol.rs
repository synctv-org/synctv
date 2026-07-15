use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use flate2::read::GzDecoder;
use prost::Message;

use super::crypto::{decrypt, encrypt};
use super::proto;
use super::{AcFunLiveDanmakuEvent, AcFunLiveSession};
use crate::ProviderClientError;

const MAGIC: [u8; 4] = 0xABCD_0001_u32.to_be_bytes();
const MAX_PACKET_SIZE: usize = 8 * 1024 * 1024;
const KPN: &str = "ACFUN_APP";
const KPF: &str = "PC_WEB";
const SUB_BIZ: &str = "mainApp";
const SDK_VERSION: &str = "kwai-acfun-live-link";
const LINK_VERSION: &str = "2.13.8";

const REGISTER: &str = "Basic.Register";
const KEEP_ALIVE: &str = "Basic.KeepAlive";
const ENTER_ROOM: &str = "ZtLiveCsEnterRoom";
const ENTER_ROOM_ACK: &str = "ZtLiveCsEnterRoomAck";
const HEARTBEAT: &str = "ZtLiveCsHeartbeat";
const GLOBAL_COMMAND: &str = "Global.ZtLiveInteractive.CsCmd";
const PUSH_MESSAGE: &str = "Push.ZtLiveInteractive.Message";
const ACTION_SIGNAL: &str = "ZtLiveScActionSignal";
const COMMENT: &str = "CommonActionSignalComment";
const TICKET_INVALID: &str = "ZtLiveScTicketInvalid";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outgoing {
    Register,
    KeepAlive,
    EnterRoom,
    Heartbeat,
    PushAck,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Incoming {
    Registered,
    HeartbeatInterval(u64),
    Comment(AcFunLiveDanmakuEvent),
    Push,
    TicketInvalid,
    Stop,
}

pub(super) struct LiveProtocol {
    user_id: i64,
    instance_id: i64,
    security_key: Vec<u8>,
    service_token: String,
    live_id: String,
    enter_room_attach: String,
    tickets: Vec<String>,
    ticket_index: usize,
    session_key: Option<Vec<u8>>,
    sequence: i64,
    header_sequence: i64,
    heartbeat_sequence: i64,
}

impl LiveProtocol {
    pub(super) fn new(session: AcFunLiveSession) -> Result<Self, ProviderClientError> {
        if session.tickets.is_empty() || session.live_id.is_empty() {
            return Err(protocol_error("live session is incomplete"));
        }
        let security_key = base64::engine::general_purpose::STANDARD
            .decode(session.security_key)
            .map_err(|error| protocol_error(error.to_string()))?;
        if security_key.len() != 16 {
            return Err(protocol_error("service security key must contain 16 bytes"));
        }
        Ok(Self {
            user_id: session.user_id,
            instance_id: 0,
            security_key,
            service_token: session.service_token,
            live_id: session.live_id,
            enter_room_attach: session.enter_room_attach,
            tickets: session.tickets,
            ticket_index: 0,
            session_key: None,
            sequence: 1,
            header_sequence: 1,
            heartbeat_sequence: 0,
        })
    }

    pub(super) fn encode(&mut self, outgoing: Outgoing) -> Result<Vec<u8>, ProviderClientError> {
        let (mut header, payload) = match outgoing {
            Outgoing::Register => self.register(),
            Outgoing::KeepAlive => self.keep_alive(),
            Outgoing::EnterRoom => self.enter_room(),
            Outgoing::Heartbeat => self.heartbeat(),
            Outgoing::PushAck => self.push_ack(),
        };
        let payload = payload.encode_to_vec();
        header.decoded_payload_len = u32::try_from(payload.len())
            .map_err(|_| protocol_error("payload exceeds protocol length"))?;
        let body = match header.encryption_mode() {
            proto::packet_header::EncryptionMode::KEncryptionNone => payload,
            proto::packet_header::EncryptionMode::KEncryptionServiceToken => {
                encrypt(&payload, &self.security_key)?
            }
            proto::packet_header::EncryptionMode::KEncryptionSessionKey => encrypt(
                &payload,
                self.session_key
                    .as_deref()
                    .ok_or_else(|| protocol_error("session key is unavailable"))?,
            )?,
        };
        let header = header.encode_to_vec();
        if header.len() + body.len() > MAX_PACKET_SIZE {
            return Err(protocol_error("packet exceeds size limit"));
        }
        let mut packet = Vec::with_capacity(12 + header.len() + body.len());
        packet.extend_from_slice(&MAGIC);
        packet.extend_from_slice(
            &u32::try_from(header.len())
                .map_err(|_| protocol_error("header exceeds protocol length"))?
                .to_be_bytes(),
        );
        packet.extend_from_slice(
            &u32::try_from(body.len())
                .map_err(|_| protocol_error("body exceeds protocol length"))?
                .to_be_bytes(),
        );
        packet.extend_from_slice(&header);
        packet.extend_from_slice(&body);
        Ok(packet)
    }

    pub(super) fn decode(
        &mut self,
        buffer: &mut Vec<u8>,
    ) -> Result<Vec<Incoming>, ProviderClientError> {
        let mut events = Vec::new();
        loop {
            if buffer.len() < 12 {
                break;
            }
            if buffer[..4] != MAGIC {
                return Err(protocol_error("packet magic is invalid"));
            }
            let header_len = read_u32(&buffer[4..8])?;
            let body_len = read_u32(&buffer[8..12])?;
            let packet_len = 12_usize
                .checked_add(header_len)
                .and_then(|value| value.checked_add(body_len))
                .ok_or_else(|| protocol_error("packet length overflow"))?;
            if packet_len > MAX_PACKET_SIZE {
                return Err(protocol_error("packet exceeds size limit"));
            }
            if buffer.len() < packet_len {
                break;
            }
            let packet = buffer.drain(..packet_len).collect::<Vec<_>>();
            let header = proto::PacketHeader::decode(&packet[12..12 + header_len])?;
            self.header_sequence = header.seq_id;
            let encrypted = &packet[12 + header_len..];
            let payload = match header.encryption_mode() {
                proto::packet_header::EncryptionMode::KEncryptionNone => encrypted.to_vec(),
                proto::packet_header::EncryptionMode::KEncryptionServiceToken => {
                    decrypt(encrypted, &self.security_key)?
                }
                proto::packet_header::EncryptionMode::KEncryptionSessionKey => decrypt(
                    encrypted,
                    self.session_key
                        .as_deref()
                        .ok_or_else(|| protocol_error("session key is unavailable"))?,
                )?,
            };
            if payload.len() != header.decoded_payload_len as usize {
                return Err(protocol_error("decoded payload length differs from header"));
            }
            let downstream = proto::DownstreamPayload::decode(payload.as_slice())?;
            events.extend(self.decode_payload(&downstream)?);
        }
        Ok(events)
    }

    fn decode_payload(
        &mut self,
        downstream: &proto::DownstreamPayload,
    ) -> Result<Vec<Incoming>, ProviderClientError> {
        if downstream.error_code == 10018 {
            return Ok(vec![Incoming::Stop]);
        }
        if downstream.error_code != 0 {
            return Err(protocol_error(format!(
                "server error {}: {}",
                downstream.error_code, downstream.error_msg
            )));
        }
        match downstream.command.as_str() {
            REGISTER => {
                let response = proto::RegisterResponse::decode(downstream.payload_data.as_slice())?;
                if response.sess_key.len() != 16 {
                    return Err(protocol_error(
                        "register response contains an invalid session key",
                    ));
                }
                self.instance_id = response.instance_id;
                self.session_key = Some(response.sess_key);
                Ok(vec![Incoming::Registered])
            }
            GLOBAL_COMMAND => {
                let ack = proto::ZtLiveCsCmdAck::decode(downstream.payload_data.as_slice())?;
                if ack.error_code != 0 {
                    return Err(protocol_error(format!(
                        "command {} failed with {}: {}",
                        ack.cmd_ack_type, ack.error_code, ack.error_msg
                    )));
                }
                if ack.cmd_ack_type == ENTER_ROOM_ACK {
                    let ack = proto::ZtLiveCsEnterRoomAck::decode(ack.payload.as_slice())?;
                    let interval = u64::try_from(ack.heartbeat_interval_ms)
                        .ok()
                        .filter(|value| *value > 0)
                        .unwrap_or(10_000);
                    Ok(vec![Incoming::HeartbeatInterval(interval)])
                } else {
                    Ok(Vec::new())
                }
            }
            PUSH_MESSAGE => self.decode_push(&downstream.payload_data),
            KEEP_ALIVE | "Basic.Ping" => Ok(Vec::new()),
            "Basic.Unregister" => Ok(vec![Incoming::Stop]),
            _ => Ok(Vec::new()),
        }
    }

    fn decode_push(&mut self, value: &[u8]) -> Result<Vec<Incoming>, ProviderClientError> {
        let message = proto::ZtLiveScMessage::decode(value)?;
        let payload =
            if message.compression_type() == proto::zt_live_sc_message::CompressionType::Gzip {
                let mut decoded = Vec::new();
                GzDecoder::new(message.payload.as_slice())
                    .take(MAX_PACKET_SIZE as u64)
                    .read_to_end(&mut decoded)
                    .map_err(|error| protocol_error(error.to_string()))?;
                decoded
            } else {
                message.payload
            };
        let mut events = vec![Incoming::Push];
        match message.message_type.as_str() {
            ACTION_SIGNAL => events.extend(parse_comments(&payload)?),
            TICKET_INVALID => {
                self.ticket_index = (self.ticket_index + 1) % self.tickets.len();
                events.push(Incoming::TicketInvalid);
            }
            "ZtLiveScStatusChanged" => {
                let status = proto::ZtLiveScStatusChanged::decode(payload.as_slice())?;
                if matches!(
                    status.r#type(),
                    proto::zt_live_sc_status_changed::Type::LiveClosed
                        | proto::zt_live_sc_status_changed::Type::LiveBanned
                ) {
                    events.push(Incoming::Stop);
                }
            }
            _ => {}
        }
        Ok(events)
    }

    fn header(&self) -> proto::PacketHeader {
        proto::PacketHeader {
            app_id: 0,
            uid: self.user_id,
            instance_id: self.instance_id,
            encryption_mode: proto::packet_header::EncryptionMode::KEncryptionSessionKey as i32,
            seq_id: self.sequence,
            kpn: KPN.to_string(),
            ..Default::default()
        }
    }

    fn payload(&self, command: &str, data: Vec<u8>) -> proto::UpstreamPayload {
        proto::UpstreamPayload {
            command: command.to_string(),
            seq_id: self.sequence,
            retry_count: 1,
            payload_data: data,
            sub_biz: SUB_BIZ.to_string(),
            ..Default::default()
        }
    }

    fn command(&self, command: &str, data: Vec<u8>) -> Vec<u8> {
        proto::ZtLiveCsCmd {
            cmd_type: command.to_string(),
            payload: data,
            ticket: self.tickets[self.ticket_index].clone(),
            live_id: self.live_id.clone(),
        }
        .encode_to_vec()
    }

    fn register(&mut self) -> (proto::PacketHeader, proto::UpstreamPayload) {
        let request = proto::RegisterRequest {
            app_info: Some(proto::AppInfo {
                sdk_version: SDK_VERSION.to_string(),
                link_version: LINK_VERSION.to_string(),
                ..Default::default()
            }),
            device_info: Some(proto::DeviceInfo {
                platform_type: proto::device_info::PlatformType::H5Windows as i32,
                device_model: "h5".to_string(),
                ..Default::default()
            }),
            presence_status: proto::register_request::PresenceStatus::KPresenceOnline as i32,
            app_active_status: proto::register_request::ActiveStatus::KAppInForeground as i32,
            instance_id: self.instance_id,
            zt_common_info: Some(proto::ZtCommonInfo {
                kpn: KPN.to_string(),
                kpf: KPF.to_string(),
                uid: self.user_id,
                ..Default::default()
            }),
            ..Default::default()
        };
        let payload = self.payload(REGISTER, request.encode_to_vec());
        let mut header = self.header();
        header.encryption_mode =
            proto::packet_header::EncryptionMode::KEncryptionServiceToken as i32;
        header.token_info = Some(proto::TokenInfo {
            token_type: proto::token_info::TokenType::KServiceToken as i32,
            token: self.service_token.as_bytes().to_vec(),
        });
        self.sequence += 1;
        (header, payload)
    }

    fn keep_alive(&mut self) -> (proto::PacketHeader, proto::UpstreamPayload) {
        let request = proto::KeepAliveRequest {
            presence_status: proto::register_request::PresenceStatus::KPresenceOnline as i32,
            app_active_status: proto::register_request::ActiveStatus::KAppInForeground as i32,
            ..Default::default()
        };
        let payload = self.payload(KEEP_ALIVE, request.encode_to_vec());
        let header = self.header();
        self.sequence += 1;
        (header, payload)
    }

    fn enter_room(&mut self) -> (proto::PacketHeader, proto::UpstreamPayload) {
        let request = proto::ZtLiveCsEnterRoom {
            enter_room_attach: self.enter_room_attach.clone(),
            client_live_sdk_version: SDK_VERSION.to_string(),
            ..Default::default()
        };
        let command = self.command(ENTER_ROOM, request.encode_to_vec());
        let payload = self.payload(GLOBAL_COMMAND, command);
        let header = self.header();
        self.sequence += 1;
        (header, payload)
    }

    fn heartbeat(&mut self) -> (proto::PacketHeader, proto::UpstreamPayload) {
        let request = proto::ZtLiveCsHeartbeat {
            client_timestamp_ms: i64::try_from(now_ms()).unwrap_or(i64::MAX),
            sequence: self.heartbeat_sequence,
        };
        let command = self.command(HEARTBEAT, request.encode_to_vec());
        let payload = self.payload(GLOBAL_COMMAND, command);
        let header = self.header();
        self.heartbeat_sequence += 1;
        self.sequence += 1;
        (header, payload)
    }

    fn push_ack(&self) -> (proto::PacketHeader, proto::UpstreamPayload) {
        let payload = self.payload(PUSH_MESSAGE, Vec::new());
        let mut header = self.header();
        header.seq_id = self.header_sequence;
        (header, payload)
    }
}

fn parse_comments(payload: &[u8]) -> Result<Vec<Incoming>, ProviderClientError> {
    let action = proto::ZtLiveScActionSignal::decode(payload)?;
    let mut comments = Vec::new();
    for item in action.item {
        if item.signal_type != COMMENT {
            continue;
        }
        for value in item.payload {
            let comment = proto::CommonActionSignalComment::decode(value.as_slice())?;
            if comment.content.is_empty() {
                continue;
            }
            let user = comment.user_info.unwrap_or_default();
            let (badge_name, badge_level) = parse_badge(&user.badge);
            let sent_at_ms = u64::try_from(comment.send_time_ms).ok();
            comments.push(Incoming::Comment(AcFunLiveDanmakuEvent {
                id: format!("{}:{}", user.user_id, comment.send_time_ms),
                user_id: user.user_id.to_string(),
                user_name: user.nickname,
                avatar_url: user
                    .avatar
                    .into_iter()
                    .find_map(|node| (!node.url.is_empty()).then_some(node.url)),
                text: comment.content,
                color: None,
                badge_name,
                badge_level,
                sent_at_ms,
            }));
        }
    }
    Ok(comments)
}

fn parse_badge(value: &str) -> (Option<String>, Option<u32>) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(value) else {
        return (None, None);
    };
    let medal = value.get("medalInfo").unwrap_or(&value);
    let name = medal
        .get("clubName")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let level = medal
        .get("level")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    (name, level)
}

fn read_u32(value: &[u8]) -> Result<usize, ProviderClientError> {
    let bytes: [u8; 4] = value
        .try_into()
        .map_err(|_| protocol_error("packet length field is incomplete"))?;
    Ok(u32::from_be_bytes(bytes) as usize)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|value| u64::try_from(value.as_millis()).ok())
        .unwrap_or_default()
}

fn protocol_error(message: impl Into<String>) -> ProviderClientError {
    ProviderClientError::Parse(format!("AcFun live protocol error: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_comment_identity_avatar_and_medal() {
        let comment = proto::CommonActionSignalComment {
            content: "hello".to_string(),
            send_time_ms: 1234,
            user_info: Some(proto::ZtLiveUserInfo {
                user_id: 42,
                nickname: "viewer".to_string(),
                avatar: vec![proto::ImageCdnNode {
                    url: "https://img.example/avatar.jpg".to_string(),
                    ..Default::default()
                }],
                badge: r#"{"medalInfo":{"clubName":"fan","level":3}}"#.to_string(),
                ..Default::default()
            }),
        };
        let action = proto::ZtLiveScActionSignal {
            item: vec![proto::ZtLiveActionSignalItem {
                signal_type: COMMENT.to_string(),
                payload: vec![comment.encode_to_vec()],
            }],
        };
        let events =
            parse_comments(&action.encode_to_vec()).expect("test operation should succeed");
        let Incoming::Comment(event) = &events[0] else {
            panic!("expected comment")
        };
        assert_eq!(event.user_id, "42");
        assert_eq!(event.user_name, "viewer");
        assert_eq!(event.badge_name.as_deref(), Some("fan"));
        assert_eq!(event.badge_level, Some(3));
    }
}
