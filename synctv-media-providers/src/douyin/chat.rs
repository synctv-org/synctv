use std::io::Read;
use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use flate2::read::GzDecoder;
use futures_util::{SinkExt, Stream, StreamExt};
use prost::Message as _;
use reqwest::header::{COOKIE, ORIGIN, REFERER, USER_AGENT};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use super::proto::{ChatMessage, ControlMessage, PushFrame, Response};
use super::sign::{generate_x_bogus, md5_hex};
use super::{DouyinDanmakuEvent, DouyinSession};
use crate::{ProviderClientError, PROVIDER_USER_AGENT};

const HOSTS: [&str; 3] = [
    "wss://webcast100-ws-web-lq.douyin.com/webcast/im/push/v2/",
    "wss://webcast100-ws-web-hl.douyin.com/webcast/im/push/v2/",
    "wss://webcast100-ws-web-lf.douyin.com/webcast/im/push/v2/",
];
const HEARTBEAT: &[u8] = b":\x02hb";

pub type DouyinDanmakuStream =
    Pin<Box<dyn Stream<Item = Result<DouyinDanmakuEvent, ProviderClientError>> + Send + 'static>>;

pub async fn watch_danmaku(
    room_id: &str,
    session: Option<&DouyinSession>,
) -> Result<DouyinDanmakuStream, ProviderClientError> {
    validate_room_id(room_id)?;
    let user_id = user_unique_id();
    let cookie = session
        .and_then(|session| session.cookie.as_deref())
        .unwrap_or_default();
    let mut socket = None;
    let mut last_error = None;
    for host in HOSTS {
        let url = websocket_url(host, room_id, &user_id)?;
        let mut request = url
            .into_client_request()
            .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
        request.headers_mut().insert(
            USER_AGENT,
            PROVIDER_USER_AGENT
                .parse()
                .map_err(ProviderClientError::from)?,
        );
        request.headers_mut().insert(
            ORIGIN,
            "https://live.douyin.com".parse().expect("static header"),
        );
        request.headers_mut().insert(
            REFERER,
            "https://live.douyin.com/".parse().expect("static header"),
        );
        if !cookie.is_empty() {
            request
                .headers_mut()
                .insert(COOKIE, cookie.parse().map_err(ProviderClientError::from)?);
        }
        match tokio_tungstenite::connect_async(request).await {
            Ok((value, _)) => {
                socket = Some(value);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let socket = socket.ok_or_else(|| {
        ProviderClientError::Network(last_error.map_or_else(
            || "Douyin danmaku endpoints are unavailable".to_string(),
            |error| error.to_string(),
        ))
    })?;
    let (mut writer, mut reader) = socket.split();
    let (sender, receiver) = mpsc::channel(128);
    tokio::spawn(async move {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(10));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    if writer.send(Message::Binary(Bytes::from_static(HEARTBEAT))).await.is_err() {
                        break;
                    }
                }
                message = reader.next() => {
                    let Some(message) = message else {
                        break;
                    };
                    let data = match message {
                        Ok(Message::Binary(data)) => data,
                        Ok(Message::Ping(data)) => {
                            if writer.send(Message::Pong(data)).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        Ok(Message::Close(_)) => break,
                        Ok(_) => continue,
                        Err(error) => {
                            let _ = sender
                                .send(Err(ProviderClientError::Network(error.to_string())))
                                .await;
                            break;
                        }
                    };
                    match decode_frame(&data) {
                        Ok((events, ack)) => {
                            if let Some(ack) = ack {
                                if writer.send(Message::Binary(ack.into())).await.is_err() {
                                    break;
                                }
                            }
                            for event in events {
                                if sender.send(Ok(event)).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Err(error) => {
                            let _ = sender.send(Err(error)).await;
                        }
                    }
                }
            }
        }
    });
    Ok(Box::pin(ReceiverStream::new(receiver)))
}

fn websocket_url(host: &str, room_id: &str, user_id: &str) -> Result<String, ProviderClientError> {
    let signature_input = format!(
        "live_id=1,aid=6383,version_code=180800,webcast_sdk_version=1.0.15,room_id={room_id},sub_room_id=,sub_channel_id=,did_rule=3,user_unique_id={user_id},device_platform=web,device_type=,ac=,identity=audience"
    );
    let signature = generate_x_bogus(&md5_hex(&signature_input), 1);
    let mut url = reqwest::Url::parse(host)
        .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
    url.query_pairs_mut()
        .append_pair("app_name", "douyin_web")
        .append_pair("compress", "gzip")
        .append_pair("device_platform", "web")
        .append_pair("browser_language", "zh-CN")
        .append_pair("browser_platform", "Win32")
        .append_pair("browser_name", "Chrome")
        .append_pair("browser_version", "120.0.0.0")
        .append_pair("aid", "6383")
        .append_pair("live_id", "1")
        .append_pair("enter_from", "web_live")
        .append_pair("version_code", "180800")
        .append_pair("webcast_sdk_version", "1.0.15")
        .append_pair("update_version_code", "1.0.15")
        .append_pair("host", "https://live.douyin.com")
        .append_pair("did_rule", "3")
        .append_pair("identity", "audience")
        .append_pair("endpoint", "live_pc")
        .append_pair("need_persist_msg_count", "15")
        .append_pair("heartbeatDuration", "0")
        .append_pair("room_id", room_id)
        .append_pair("user_unique_id", user_id)
        .append_pair("signature", &signature);
    Ok(url.to_string())
}

fn decode_frame(
    data: &[u8],
) -> Result<(Vec<DouyinDanmakuEvent>, Option<Vec<u8>>), ProviderClientError> {
    let frame = PushFrame::decode(data)?;
    let compressed = frame
        .headers
        .iter()
        .find(|header| header.key == "compress_type")
        .is_none_or(|header| header.value == "gzip");
    let payload = if compressed {
        let mut output = Vec::new();
        GzDecoder::new(frame.payload.as_slice())
            .read_to_end(&mut output)
            .map_err(|error| ProviderClientError::Parse(error.to_string()))?;
        output
    } else {
        frame.payload
    };
    let response = Response::decode(payload.as_slice())?;
    let ack = response.need_ack.then(|| {
        PushFrame {
            log_id: frame.log_id,
            payload_type: "ack".to_string(),
            payload: response.internal_ext.as_bytes().to_vec(),
            ..Default::default()
        }
        .encode_to_vec()
    });
    let mut events = Vec::new();
    for message in response.messages {
        match message.method.as_str() {
            "WebcastChatMessage" => {
                if let Ok(chat) = ChatMessage::decode(message.payload.as_slice()) {
                    if !chat.content.is_empty() {
                        let user = chat.user.unwrap_or_default();
                        let common = chat.common.unwrap_or_default();
                        let sent_at_ms = if chat.event_time > 0 {
                            Some(chat.event_time.saturating_mul(1_000))
                        } else if common.create_time > 0 {
                            Some(common.create_time)
                        } else {
                            None
                        };
                        let color = chat
                            .rtf_content_v2
                            .and_then(|text| text.default_format)
                            .or_else(|| chat.rtf_content.and_then(|text| text.default_format))
                            .map(|format| format.color)
                            .filter(|color| !color.is_empty());
                        events.push(DouyinDanmakuEvent::Chat {
                            id: common.msg_id.to_string(),
                            user_id: user.id.to_string(),
                            user_name: user.nickname,
                            text: chat.content,
                            color,
                            sent_at_ms,
                        });
                    }
                }
            }
            "WebcastControlMessage" => {
                if let Ok(control) = ControlMessage::decode(message.payload.as_slice()) {
                    if control.action == 3 {
                        events.push(DouyinDanmakuEvent::StreamClosed {
                            action: control.action,
                            message: (!control.tips.trim().is_empty()).then_some(control.tips),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    Ok((events, ack))
}

fn validate_room_id(room_id: &str) -> Result<(), ProviderClientError> {
    if room_id.is_empty()
        || room_id.len() > 32
        || !room_id.chars().all(|value| value.is_ascii_digit())
    {
        return Err(ProviderClientError::InvalidConfig(
            "Douyin danmaku room ID is invalid".to_string(),
        ));
    }
    Ok(())
}

fn user_unique_id() -> String {
    const ID_SPAN: u64 = 699_999_999_999_999_999;
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_nanos() % u128::from(ID_SPAN)).unwrap_or_default()
        });
    (7_300_000_000_000_000_000_u64 + value).to_string()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::write::GzEncoder;
    use flate2::Compression;

    use super::super::proto::{Common, Message as ProtoMessage, Text, TextFormat, User};
    use super::*;

    #[test]
    fn builds_signed_websocket_url() {
        let url = websocket_url(HOSTS[0], "12345", "7300000000000000001")
            .expect("test operation should succeed");
        assert!(url.contains("room_id=12345"));
        assert!(url.contains("signature="));
        assert!(url.contains("user_unique_id=7300000000000000001"));
    }

    #[test]
    fn decodes_gzip_chat_and_ack() {
        let chat = ChatMessage {
            common: Some(Common {
                msg_id: 42,
                create_time: 1_700_000_000_123,
                ..Default::default()
            }),
            user: Some(User {
                id: 7,
                nickname: "viewer".to_string(),
                ..Default::default()
            }),
            content: "hello".to_string(),
            rtf_content_v2: Some(Text {
                default_format: Some(TextFormat {
                    color: "#FF0000".to_string(),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let response = Response {
            messages: vec![ProtoMessage {
                method: "WebcastChatMessage".to_string(),
                payload: chat.encode_to_vec(),
                ..Default::default()
            }],
            internal_ext: "cursor".to_string(),
            need_ack: true,
            ..Default::default()
        };
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(&response.encode_to_vec())
            .expect("test operation should succeed");
        let frame = PushFrame {
            log_id: 99,
            payload: encoder.finish().expect("test operation should succeed"),
            ..Default::default()
        };
        let (events, ack) =
            decode_frame(&frame.encode_to_vec()).expect("test operation should succeed");
        assert_eq!(
            events,
            [DouyinDanmakuEvent::Chat {
                id: "42".to_string(),
                user_id: "7".to_string(),
                user_name: "viewer".to_string(),
                text: "hello".to_string(),
                color: Some("#FF0000".to_string()),
                sent_at_ms: Some(1_700_000_000_123),
            }]
        );
        let ack = PushFrame::decode(ack.expect("test operation should succeed").as_slice())
            .expect("test operation should succeed");
        assert_eq!(ack.log_id, 99);
        assert_eq!(ack.payload_type, "ack");
        assert_eq!(ack.payload, b"cursor");
    }
}
