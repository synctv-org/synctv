use std::collections::HashMap;
use std::pin::Pin;

use futures_util::{SinkExt, Stream, StreamExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_tungstenite::tungstenite::Message;

use super::{TwitchChatEvent, TwitchSession};
use crate::ProviderClientError;

pub type TwitchChatStream =
    Pin<Box<dyn Stream<Item = Result<TwitchChatEvent, ProviderClientError>> + Send + 'static>>;

pub async fn watch_chat(
    channel: &str,
    session: Option<&TwitchSession>,
) -> Result<TwitchChatStream, ProviderClientError> {
    let channel = channel.trim().trim_start_matches('#').to_ascii_lowercase();
    if channel.is_empty() {
        return Err(ProviderClientError::InvalidConfig(
            "Twitch chat channel is required".to_string(),
        ));
    }
    let (socket, _) = tokio_tungstenite::connect_async("wss://irc-ws.chat.twitch.tv:443")
        .await
        .map_err(|error| ProviderClientError::Network(error.to_string()))?;
    let (mut writer, mut reader) = socket.split();
    let authenticated = session
        .and_then(|value| value.auth_token.as_deref())
        .zip(session.and_then(|value| value.login.as_deref()));
    let (pass, nick) = authenticated.map_or_else(
        || {
            let suffix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.subsec_nanos());
            ("SCHMOOPIIE".to_string(), format!("justinfan{suffix}"))
        },
        |(token, login)| (format!("oauth:{token}"), login.to_ascii_lowercase()),
    );
    for command in [
        "CAP REQ :twitch.tv/tags twitch.tv/commands".to_string(),
        format!("PASS {pass}"),
        format!("NICK {nick}"),
        format!("JOIN #{channel}"),
    ] {
        writer
            .send(Message::Text(command.into()))
            .await
            .map_err(|error| ProviderClientError::Network(error.to_string()))?;
    }

    let (sender, receiver) = mpsc::channel(128);
    tokio::spawn(async move {
        while let Some(message) = reader.next().await {
            let message = match message {
                Ok(Message::Text(value)) => value.to_string(),
                Ok(Message::Ping(value)) => {
                    if writer.send(Message::Pong(value)).await.is_err() {
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
            for line in message
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
            {
                if let Some(payload) = line.strip_prefix("PING ") {
                    if writer
                        .send(Message::Text(format!("PONG {payload}").into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
                if let Some(event) = parse_privmsg(line) {
                    if sender.send(Ok(event)).await.is_err() {
                        return;
                    }
                }
            }
        }
    });
    Ok(Box::pin(ReceiverStream::new(receiver)))
}

fn parse_privmsg(line: &str) -> Option<TwitchChatEvent> {
    let (tag_text, rest) = line.strip_prefix('@')?.split_once(' ')?;
    let tags = tag_text
        .split(';')
        .filter_map(|entry| entry.split_once('='))
        .collect::<HashMap<_, _>>();
    let (_, text) = rest.split_once(" PRIVMSG ")?.1.split_once(" :")?;
    let user_name = tags
        .get("display-name")
        .copied()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            rest.strip_prefix(':')?
                .split_once('!')
                .map(|(name, _)| name)
        })?;
    Some(TwitchChatEvent {
        id: tags.get("id").copied().unwrap_or_default().to_string(),
        user_name: unescape_tag(user_name),
        text: text.to_string(),
        color: tags
            .get("color")
            .copied()
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
        badges: tags
            .get("badges")
            .copied()
            .unwrap_or_default()
            .split(',')
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .collect(),
        sent_at_ms: tags.get("tmi-sent-ts").and_then(|value| value.parse().ok()),
    })
}

fn unescape_tag(value: &str) -> String {
    value
        .replace("\\s", " ")
        .replace("\\:", ";")
        .replace("\\r", "\r")
        .replace("\\n", "\n")
        .replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_twitch_irc_privmsg_tags() {
        let event = parse_privmsg("@badge-info=;badges=subscriber/12;color=#9146FF;display-name=Sync\\sTV;id=message-1;tmi-sent-ts=1700000000123 :synctv!synctv@synctv.tmi.twitch.tv PRIVMSG #channel :hello world")
            .expect("PRIVMSG should parse");
        assert_eq!(event.id, "message-1");
        assert_eq!(event.user_name, "Sync TV");
        assert_eq!(event.text, "hello world");
        assert_eq!(event.badges, ["subscriber/12"]);
        assert_eq!(event.sent_at_ms, Some(1_700_000_000_123));
    }
}
