use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::Stream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use super::stt::{decode, encode, packet, take_packets};
use super::DouyuDanmakuEvent;
use crate::ProviderClientError;

const DOUYU_CHAT_ADDRS: [&str; 2] = ["danmuproxy.douyu.com:8601", "danmuproxy.douyu.com:8602"];

pub type DouyuDanmakuStream =
    Pin<Box<dyn Stream<Item = Result<DouyuDanmakuEvent, ProviderClientError>> + Send + 'static>>;

pub async fn watch_danmaku(room_id: &str) -> Result<DouyuDanmakuStream, ProviderClientError> {
    if room_id.is_empty() || !room_id.chars().all(|value| value.is_ascii_digit()) {
        return Err(ProviderClientError::InvalidConfig(
            "Douyu room ID is invalid".to_string(),
        ));
    }
    let mut last_error = None;
    let mut socket = None;
    for address in DOUYU_CHAT_ADDRS {
        match tokio::net::TcpStream::connect(address).await {
            Ok(value) => {
                socket = Some(value);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let socket = socket.ok_or_else(|| {
        ProviderClientError::Network(last_error.map_or_else(
            || "Douyu danmaku endpoints are unavailable".to_string(),
            |error| error.to_string(),
        ))
    })?;
    let (mut reader, mut writer) = socket.into_split();
    let login = packet(&encode(&[("type", "loginreq"), ("roomid", room_id)]))?;
    let group = packet(&encode(&[
        ("type", "joingroup"),
        ("rid", room_id),
        ("gid", "-9999"),
    ]))?;
    writer
        .write_all(&login)
        .await
        .map_err(|error| ProviderClientError::Network(error.to_string()))?;
    writer
        .write_all(&group)
        .await
        .map_err(|error| ProviderClientError::Network(error.to_string()))?;

    let (sender, receiver) = mpsc::channel(128);
    tokio::spawn(async move {
        let heartbeat = packet("type@=mrkl/").expect("static heartbeat should encode");
        let mut interval = tokio::time::interval(Duration::from_secs(45));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;
        let mut input = vec![0_u8; 16 * 1024];
        let mut buffer = Vec::new();
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if writer.write_all(&heartbeat).await.is_err() {
                        break;
                    }
                }
                read = reader.read(&mut input) => {
                    let size = match read {
                        Ok(0) => break,
                        Ok(size) => size,
                        Err(error) => {
                            let _ = sender.send(Err(ProviderClientError::Network(error.to_string()))).await;
                            break;
                        }
                    };
                    buffer.extend_from_slice(&input[..size]);
                    match take_packets(&mut buffer) {
                        Ok(frames) => {
                            for frame in frames {
                                if let Some(event) = parse_chat(&frame) {
                                    if sender.send(Ok(event)).await.is_err() {
                                        return;
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            tracing::debug!(%error, "Ignoring malformed Douyu danmaku frame");
                            buffer.clear();
                        }
                    }
                }
            }
        }
    });
    Ok(Box::pin(ReceiverStream::new(receiver)))
}

fn parse_chat(payload: &str) -> Option<DouyuDanmakuEvent> {
    let fields = decode(payload);
    if fields.get("type")?.as_str() != "chatmsg" {
        return None;
    }
    let text = fields.get("txt")?.clone();
    if text.is_empty() {
        return None;
    }
    Some(DouyuDanmakuEvent {
        id: fields.get("cid").cloned().unwrap_or_default(),
        user_id: fields.get("uid").cloned().unwrap_or_default(),
        user_name: fields.get("nn").cloned().unwrap_or_default(),
        text,
        color: fields.get("col").and_then(|value| color(value)),
        level: fields.get("level").and_then(|value| value.parse().ok()),
        badge_name: fields.get("bnn").cloned().filter(|value| !value.is_empty()),
        badge_level: fields.get("bl").and_then(|value| value.parse().ok()),
        sent_at_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|value| u64::try_from(value.as_millis()).ok()),
    })
}

fn color(value: &str) -> Option<String> {
    let rgb = match value.parse::<u32>().ok()? {
        0 => 0xff_ff_ff,
        1 => 0xff_15_15,
        2 => 0x1e_87_f0,
        3 => 0x7a_c8_4b,
        4 => 0xff_7f_00,
        5 => 0x9b_3b_f4,
        6 => 0xff_69_b4,
        value if value > 0xff_ff => value.min(0xff_ff_ff),
        _ => 0xff_ff_ff,
    };
    Some(format!("#{rgb:06X}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chat_metadata_and_douyu_color() {
        let event = parse_chat(
            "type@=chatmsg/cid@=m1/uid@=42/nn@=viewer/txt@=hello/col@=1/level@=12/bnn@=fan/bl@=3/",
        )
        .expect("chat event should parse");
        assert_eq!(event.id, "m1");
        assert_eq!(event.user_id, "42");
        assert_eq!(event.user_name, "viewer");
        assert_eq!(event.text, "hello");
        assert_eq!(event.color.as_deref(), Some("#FF1515"));
        assert_eq!(event.level, Some(12));
        assert_eq!(event.badge_name.as_deref(), Some("fan"));
        assert_eq!(event.badge_level, Some(3));
    }
}
