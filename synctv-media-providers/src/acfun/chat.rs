use std::pin::Pin;
use std::time::Duration;

use futures_util::{SinkExt, Stream, StreamExt};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_stream::wrappers::ReceiverStream;
use tokio_tungstenite::tungstenite::Message;

use super::live_protocol::{Incoming, LiveProtocol, Outgoing};
use super::{AcFunLiveDanmakuEvent, AcFunLiveSession};
use crate::ProviderClientError;

const DANMAKU_URL: &str = "wss://klink-newproduct-ws3.kwaizt.com/";

pub type AcFunDanmakuStream = Pin<
    Box<dyn Stream<Item = Result<AcFunLiveDanmakuEvent, ProviderClientError>> + Send + 'static>,
>;

pub async fn watch_danmaku(
    session: AcFunLiveSession,
) -> Result<AcFunDanmakuStream, ProviderClientError> {
    let mut protocol = LiveProtocol::new(session)?;
    let (socket, _) = tokio_tungstenite::connect_async(DANMAKU_URL)
        .await
        .map_err(|error| ProviderClientError::Network(error.to_string()))?;
    let (mut writer, mut reader) = socket.split();
    send(&mut writer, protocol.encode(Outgoing::Register)?).await?;

    let (sender, receiver) = mpsc::channel(128);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        let mut buffer = Vec::new();
        let mut registered = false;
        let mut heartbeat_interval = Duration::from_secs(10);
        let mut last_heartbeat = Instant::now();
        let mut heartbeat_count = 0_u64;
        loop {
            tokio::select! {
                _ = ticker.tick(), if registered => {
                    if last_heartbeat.elapsed() < heartbeat_interval {
                        continue;
                    }
                    if send_protocol(&mut writer, &mut protocol, Outgoing::Heartbeat).await.is_err() {
                        break;
                    }
                    heartbeat_count += 1;
                    if heartbeat_count.is_multiple_of(5)
                        && send_protocol(&mut writer, &mut protocol, Outgoing::KeepAlive).await.is_err()
                    {
                        break;
                    }
                    last_heartbeat = Instant::now();
                }
                message = reader.next() => {
                    let Some(message) = message else { break };
                    match message {
                        Ok(Message::Binary(value)) => {
                            buffer.extend_from_slice(&value);
                            let events = match protocol.decode(&mut buffer) {
                                Ok(events) => events,
                                Err(error) => {
                                    let _ = sender.send(Err(error)).await;
                                    break;
                                }
                            };
                            for event in events {
                                match event {
                                    Incoming::Registered => {
                                        registered = true;
                                        if send_protocol(&mut writer, &mut protocol, Outgoing::KeepAlive).await.is_err()
                                            || send_protocol(&mut writer, &mut protocol, Outgoing::EnterRoom).await.is_err()
                                        {
                                            return;
                                        }
                                    }
                                    Incoming::HeartbeatInterval(value) => {
                                        heartbeat_interval = Duration::from_millis(value.clamp(1_000, 120_000));
                                        last_heartbeat = Instant::now();
                                    }
                                    Incoming::Comment(event) => {
                                        if sender.send(Ok(event)).await.is_err() {
                                            return;
                                        }
                                    }
                                    Incoming::Push => {
                                        if send_protocol(&mut writer, &mut protocol, Outgoing::PushAck).await.is_err() {
                                            return;
                                        }
                                    }
                                    Incoming::TicketInvalid => {
                                        if send_protocol(&mut writer, &mut protocol, Outgoing::EnterRoom).await.is_err() {
                                            return;
                                        }
                                    }
                                    Incoming::Stop => return,
                                }
                            }
                        }
                        Ok(Message::Ping(value)) => {
                            if writer.send(Message::Pong(value)).await.is_err() {
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

async fn send_protocol<S>(
    writer: &mut S,
    protocol: &mut LiveProtocol,
    outgoing: Outgoing,
) -> Result<(), ProviderClientError>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    send(writer, protocol.encode(outgoing)?).await
}

async fn send<S>(writer: &mut S, value: Vec<u8>) -> Result<(), ProviderClientError>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    writer
        .send(Message::Binary(value.into()))
        .await
        .map_err(|error| ProviderClientError::Network(error.to_string()))
}
