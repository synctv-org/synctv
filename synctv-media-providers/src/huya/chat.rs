use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, Stream, StreamExt};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_tungstenite::tungstenite::Message;

use super::tars::{decode_danmaku, heartbeat_packet, registration_packet};
use super::{HuyaChatIdentity, HuyaDanmakuEvent};
use crate::ProviderClientError;

const HUYA_DANMAKU_URL: &str = "wss://cdnws.api.huya.com:443";

pub type HuyaDanmakuStream =
    Pin<Box<dyn Stream<Item = Result<HuyaDanmakuEvent, ProviderClientError>> + Send + 'static>>;

pub async fn watch_danmaku(
    identity: HuyaChatIdentity,
) -> Result<HuyaDanmakuStream, ProviderClientError> {
    if identity.presenter_uid <= 0 {
        return Err(ProviderClientError::InvalidConfig(
            "Huya presenter UID is required for danmaku".to_string(),
        ));
    }
    let (socket, _) = tokio_tungstenite::connect_async(HUYA_DANMAKU_URL)
        .await
        .map_err(|error| ProviderClientError::Network(error.to_string()))?;
    let (mut writer, mut reader) = socket.split();
    writer
        .send(Message::Binary(
            registration_packet(identity.presenter_uid, identity.top_sid, identity.sub_sid).into(),
        ))
        .await
        .map_err(|error| ProviderClientError::Network(error.to_string()))?;

    let (sender, receiver) = mpsc::channel(128);
    tokio::spawn(async move {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(60));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    if writer
                        .send(Message::Binary(heartbeat_packet().to_vec().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                message = reader.next() => {
                    let Some(message) = message else { break };
                    match message {
                        Ok(Message::Binary(data)) => match decode_danmaku(&data) {
                            Ok(Some(value)) => {
                                let id = if value.id == 0 {
                                    hex::encode(Sha256::digest(&data))
                                } else {
                                    value.id.to_string()
                                };
                                let event = HuyaDanmakuEvent {
                                    id,
                                    user_id: value.user_id.to_string(),
                                    user_name: value.user_name,
                                    text: value.text,
                                    color: value.color.map(|color| format!("#{color:06X}")),
                                    avatar_url: value.avatar_url,
                                    sent_at_ms: SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .ok()
                                        .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
                                };
                                if sender.send(Ok(event)).await.is_err() {
                                    break;
                                }
                            }
                            Ok(None) => {}
                            Err(error) => {
                                tracing::debug!(%error, "Ignoring malformed Huya danmaku frame");
                            }
                        },
                        Ok(Message::Ping(data)) => {
                            if writer.send(Message::Pong(data)).await.is_err() {
                                break;
                            }
                        }
                        Ok(Message::Close(_)) => break,
                        Ok(_) => {}
                        Err(error) => {
                            let _ = sender
                                .send(Err(ProviderClientError::Network(error.to_string())))
                                .await;
                            break;
                        }
                    }
                }
            }
        }
    });
    Ok(Box::pin(ReceiverStream::new(receiver)))
}
