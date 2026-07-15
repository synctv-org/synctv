use std::collections::HashMap;

use crate::ProviderClientError;

const CLIENT_MAGIC: u32 = 689;

pub(crate) fn encode(fields: &[(&str, &str)]) -> String {
    let mut output = String::new();
    for (key, value) in fields {
        output.push_str(&escape(key));
        output.push_str("@=");
        output.push_str(&escape(value));
        output.push('/');
    }
    output
}

pub(crate) fn decode(input: &str) -> HashMap<String, String> {
    input
        .split('/')
        .filter_map(|part| part.split_once("@="))
        .map(|(key, value)| (unescape(key), unescape(value)))
        .collect()
}

pub(crate) fn packet(message: &str) -> Result<Vec<u8>, ProviderClientError> {
    let length = u32::try_from(message.len().saturating_add(9))
        .map_err(|_| ProviderClientError::Parse("Douyu STT message is too large".to_string()))?;
    let mut output = Vec::with_capacity(message.len().saturating_add(13));
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(&CLIENT_MAGIC.to_le_bytes());
    output.extend_from_slice(message.as_bytes());
    output.push(0);
    Ok(output)
}

pub(crate) fn take_packets(buffer: &mut Vec<u8>) -> Result<Vec<String>, ProviderClientError> {
    let mut output = Vec::new();
    let mut consumed = 0_usize;
    while buffer.len().saturating_sub(consumed) >= 13 {
        let length = u32::from_le_bytes(
            buffer[consumed..consumed + 4]
                .try_into()
                .map_err(|_| ProviderClientError::Parse("invalid Douyu packet".to_string()))?,
        ) as usize;
        if length < 9 {
            return Err(ProviderClientError::Parse(
                "invalid Douyu packet length".to_string(),
            ));
        }
        let total = length.saturating_add(4);
        if buffer.len().saturating_sub(consumed) < total {
            break;
        }
        let start = consumed + 12;
        let end = consumed + total - 1;
        output.push(String::from_utf8_lossy(&buffer[start..end]).into_owned());
        consumed += total;
    }
    if consumed != 0 {
        buffer.drain(..consumed);
    }
    Ok(output)
}

fn escape(input: &str) -> String {
    input.replace('@', "@A").replace('/', "@S")
}

fn unescape(input: &str) -> String {
    input.replace("@S", "/").replace("@A", "@")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_stream_preserves_escaped_fields_and_partial_frames() {
        let message = encode(&[("type", "chatmsg"), ("txt", "a@b/c")]);
        let packet = packet(&message).expect("packet should encode");
        let mut buffer = packet[..8].to_vec();
        assert!(take_packets(&mut buffer)
            .expect("partial packet should parse")
            .is_empty());
        buffer.extend_from_slice(&packet[8..]);
        let frames = take_packets(&mut buffer).expect("complete packet should parse");
        assert_eq!(frames.len(), 1);
        assert_eq!(decode(&frames[0])["txt"], "a@b/c");
        assert!(buffer.is_empty());
    }
}
