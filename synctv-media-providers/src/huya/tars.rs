use std::collections::BTreeMap;

use crate::ProviderClientError;

const BYTE: u8 = 0;
const SHORT: u8 = 1;
const INT: u8 = 2;
const LONG: u8 = 3;
const FLOAT: u8 = 4;
const DOUBLE: u8 = 5;
const STRING1: u8 = 6;
const STRING4: u8 = 7;
const MAP: u8 = 8;
const LIST: u8 = 9;
const STRUCT_BEGIN: u8 = 10;
const STRUCT_END: u8 = 11;
const ZERO: u8 = 12;
const SIMPLE_LIST: u8 = 13;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedDanmaku {
    pub id: i64,
    pub user_id: i64,
    pub user_name: String,
    pub text: String,
    pub color: Option<i32>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone)]
enum Value {
    Number(i64),
    String(String),
    Bytes(Vec<u8>),
    Struct(BTreeMap<u8, Value>),
    List,
}

pub(crate) fn registration_packet(presenter_uid: i64, top_sid: i64, sub_sid: i64) -> Vec<u8> {
    let mut user = Vec::new();
    write_number(&mut user, 0, presenter_uid);
    write_number(&mut user, 1, i64::from(presenter_uid == 0));
    write_string(&mut user, 2, "");
    write_string(&mut user, 3, "");
    write_number(&mut user, 4, top_sid);
    write_number(&mut user, 5, sub_sid);
    write_number(&mut user, 6, presenter_uid);
    write_number(&mut user, 7, 3);

    let mut command = Vec::new();
    write_number(&mut command, 0, 1);
    write_bytes(&mut command, 1, &user);
    command
}

pub(crate) fn heartbeat_packet() -> &'static [u8] {
    &[
        0x00, 0x03, 0x1d, 0x00, 0x00, 0x69, 0x00, 0x00, 0x00, 0x69, 0x10, 0x03, 0x2c, 0x3c, 0x4c,
        0x56, 0x08, 0x6f, 0x6e, 0x6c, 0x69, 0x6e, 0x65, 0x75, 0x69, 0x66, 0x0f, 0x4f, 0x6e, 0x55,
        0x73, 0x65, 0x72, 0x48, 0x65, 0x61, 0x72, 0x74, 0x42, 0x65, 0x61, 0x74, 0x7d, 0x00, 0x00,
        0x3c, 0x08, 0x00, 0x01, 0x06, 0x04, 0x74, 0x52, 0x65, 0x71, 0x1d, 0x00, 0x00, 0x2f, 0x0a,
        0x0a, 0x0c, 0x16, 0x00, 0x26, 0x00, 0x36, 0x07, 0x61, 0x64, 0x72, 0x5f, 0x77, 0x61, 0x70,
        0x46, 0x00, 0x0b, 0x12, 0x03, 0xae, 0xf0, 0x0f, 0x22, 0x03, 0xae, 0xf0, 0x0f, 0x3c, 0x42,
        0x6d, 0x52, 0x02, 0x60, 0x5c, 0x60, 0x01, 0x7c, 0x82, 0x00, 0x0b, 0xb0, 0x1f, 0x9c, 0xac,
        0x0b, 0x8c, 0x98, 0x0c, 0xa8, 0x0c, 0x20,
    ]
}

pub(crate) fn decode_danmaku(data: &[u8]) -> Result<Option<DecodedDanmaku>, ProviderClientError> {
    let command = decode_root(data)?;
    if number(&command, 0) != Some(7) {
        return Ok(None);
    }
    let Some(payload) = bytes(&command, 1) else {
        return Ok(None);
    };
    let push = decode_root(payload)?;
    if number(&push, 1) != Some(1400) {
        return Ok(None);
    }
    let Some(message) = bytes(&push, 2) else {
        return Ok(None);
    };
    let notice = decode_root(message)?;
    let Some(sender) = structure(&notice, 0) else {
        return Ok(None);
    };
    let text = string(&notice, 3).unwrap_or_default().to_string();
    if text.is_empty() {
        return Ok(None);
    }
    let color = structure(&notice, 6)
        .and_then(|format| number(format, 0))
        .and_then(|value| i32::try_from(value).ok())
        .filter(|value| *value > 0);
    Ok(Some(DecodedDanmaku {
        id: number(&push, 5).unwrap_or_default(),
        user_id: number(sender, 0).unwrap_or_default(),
        user_name: string(sender, 2).unwrap_or_default().to_string(),
        text,
        color,
        avatar_url: string(sender, 4)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
    }))
}

fn decode_root(data: &[u8]) -> Result<BTreeMap<u8, Value>, ProviderClientError> {
    let mut decoder = Decoder { data, position: 0 };
    decoder.read_fields(false)
}

fn number(fields: &BTreeMap<u8, Value>, tag: u8) -> Option<i64> {
    match fields.get(&tag) {
        Some(Value::Number(value)) => Some(*value),
        _ => None,
    }
}

fn string(fields: &BTreeMap<u8, Value>, tag: u8) -> Option<&str> {
    match fields.get(&tag) {
        Some(Value::String(value)) => Some(value),
        _ => None,
    }
}

fn bytes(fields: &BTreeMap<u8, Value>, tag: u8) -> Option<&[u8]> {
    match fields.get(&tag) {
        Some(Value::Bytes(value)) => Some(value),
        _ => None,
    }
}

fn structure(fields: &BTreeMap<u8, Value>, tag: u8) -> Option<&BTreeMap<u8, Value>> {
    match fields.get(&tag) {
        Some(Value::Struct(value)) => Some(value),
        _ => None,
    }
}

struct Decoder<'a> {
    data: &'a [u8],
    position: usize,
}

impl Decoder<'_> {
    fn read_fields(
        &mut self,
        stop_at_struct_end: bool,
    ) -> Result<BTreeMap<u8, Value>, ProviderClientError> {
        let mut fields = BTreeMap::new();
        while self.position < self.data.len() {
            let (tag, kind) = self.read_head()?;
            if kind == STRUCT_END {
                if stop_at_struct_end {
                    return Ok(fields);
                }
                continue;
            }
            fields.insert(tag, self.read_value(kind)?);
        }
        if stop_at_struct_end {
            return Err(parse_error("unterminated Tars struct"));
        }
        Ok(fields)
    }

    fn read_value(&mut self, kind: u8) -> Result<Value, ProviderClientError> {
        match kind {
            ZERO => Ok(Value::Number(0)),
            BYTE => Ok(Value::Number(i64::from(self.read_i8()?))),
            SHORT => Ok(Value::Number(i64::from(self.read_i16()?))),
            INT => Ok(Value::Number(i64::from(self.read_i32()?))),
            LONG => Ok(Value::Number(self.read_i64()?)),
            FLOAT => {
                self.take(4)?;
                Ok(Value::List)
            }
            DOUBLE => {
                self.take(8)?;
                Ok(Value::List)
            }
            STRING1 => {
                let length = usize::from(self.read_u8()?);
                self.read_string(length).map(Value::String)
            }
            STRING4 => {
                let length = usize::try_from(self.read_u32()?)
                    .map_err(|_| parse_error("Tars string length is too large"))?;
                self.read_string(length).map(Value::String)
            }
            STRUCT_BEGIN => self.read_fields(true).map(Value::Struct),
            SIMPLE_LIST => {
                let (_, element_kind) = self.read_head()?;
                if element_kind != BYTE {
                    return Err(parse_error("Tars simple-list element is not byte"));
                }
                let (_, length_kind) = self.read_head()?;
                let length = usize::try_from(self.read_number(length_kind)?)
                    .map_err(|_| parse_error("Tars byte-list length is invalid"))?;
                Ok(Value::Bytes(self.take(length)?.to_vec()))
            }
            LIST => {
                let (_, length_kind) = self.read_head()?;
                let length = usize::try_from(self.read_number(length_kind)?)
                    .map_err(|_| parse_error("Tars list length is invalid"))?;
                for _ in 0..length {
                    let (_, item_kind) = self.read_head()?;
                    self.read_value(item_kind)?;
                }
                Ok(Value::List)
            }
            MAP => {
                let (_, length_kind) = self.read_head()?;
                let length = usize::try_from(self.read_number(length_kind)?)
                    .map_err(|_| parse_error("Tars map length is invalid"))?;
                for _ in 0..length.saturating_mul(2) {
                    let (_, item_kind) = self.read_head()?;
                    self.read_value(item_kind)?;
                }
                Ok(Value::List)
            }
            _ => Err(parse_error("unsupported Tars field type")),
        }
    }

    fn read_number(&mut self, kind: u8) -> Result<i64, ProviderClientError> {
        match self.read_value(kind)? {
            Value::Number(value) => Ok(value),
            _ => Err(parse_error("Tars numeric field has a non-numeric type")),
        }
    }

    fn read_head(&mut self) -> Result<(u8, u8), ProviderClientError> {
        let head = self.read_u8()?;
        let kind = head & 0x0f;
        let tag = head >> 4;
        if tag == 15 {
            Ok((self.read_u8()?, kind))
        } else {
            Ok((tag, kind))
        }
    }

    fn read_string(&mut self, length: usize) -> Result<String, ProviderClientError> {
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|error| parse_error(&error.to_string()))
    }

    fn read_u8(&mut self) -> Result<u8, ProviderClientError> {
        Ok(*self
            .take(1)?
            .first()
            .ok_or_else(|| parse_error("missing Tars byte"))?)
    }

    fn read_i8(&mut self) -> Result<i8, ProviderClientError> {
        Ok(self.read_u8()?.cast_signed())
    }

    fn read_i16(&mut self) -> Result<i16, ProviderClientError> {
        let bytes: [u8; 2] = self
            .take(2)?
            .try_into()
            .map_err(|_| parse_error("invalid Tars i16"))?;
        Ok(i16::from_be_bytes(bytes))
    }

    fn read_i32(&mut self) -> Result<i32, ProviderClientError> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| parse_error("invalid Tars i32"))?;
        Ok(i32::from_be_bytes(bytes))
    }

    fn read_u32(&mut self) -> Result<u32, ProviderClientError> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| parse_error("invalid Tars u32"))?;
        Ok(u32::from_be_bytes(bytes))
    }

    fn read_i64(&mut self) -> Result<i64, ProviderClientError> {
        let bytes: [u8; 8] = self
            .take(8)?
            .try_into()
            .map_err(|_| parse_error("invalid Tars i64"))?;
        Ok(i64::from_be_bytes(bytes))
    }

    fn take(&mut self, length: usize) -> Result<&[u8], ProviderClientError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| parse_error("Tars field length overflow"))?;
        let value = self
            .data
            .get(self.position..end)
            .ok_or_else(|| parse_error("truncated Tars payload"))?;
        self.position = end;
        Ok(value)
    }
}

fn write_head(output: &mut Vec<u8>, tag: u8, kind: u8) {
    if tag < 15 {
        output.push((tag << 4) | kind);
    } else {
        output.push(0xf0 | kind);
        output.push(tag);
    }
}

fn write_number(output: &mut Vec<u8>, tag: u8, value: i64) {
    if value == 0 {
        write_head(output, tag, ZERO);
    } else if let Ok(value) = i8::try_from(value) {
        write_head(output, tag, BYTE);
        output.push(value.cast_unsigned());
    } else if let Ok(value) = i16::try_from(value) {
        write_head(output, tag, SHORT);
        output.extend_from_slice(&value.to_be_bytes());
    } else if let Ok(value) = i32::try_from(value) {
        write_head(output, tag, INT);
        output.extend_from_slice(&value.to_be_bytes());
    } else {
        write_head(output, tag, LONG);
        output.extend_from_slice(&value.to_be_bytes());
    }
}

fn write_string(output: &mut Vec<u8>, tag: u8, value: &str) {
    if let Ok(length) = u8::try_from(value.len()) {
        write_head(output, tag, STRING1);
        output.push(length);
    } else {
        write_head(output, tag, STRING4);
        output.extend_from_slice(&u32::try_from(value.len()).unwrap_or(u32::MAX).to_be_bytes());
    }
    output.extend_from_slice(value.as_bytes());
}

fn write_bytes(output: &mut Vec<u8>, tag: u8, value: &[u8]) {
    write_head(output, tag, SIMPLE_LIST);
    write_head(output, 0, BYTE);
    write_number(output, 0, i64::try_from(value.len()).unwrap_or(i64::MAX));
    output.extend_from_slice(value);
}

fn parse_error(message: &str) -> ProviderClientError {
    ProviderClientError::Parse(format!("Huya Tars: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_packet_round_trips_command_and_identity() {
        let packet = registration_packet(789, 123, 456);
        let command = decode_root(&packet).expect("command should decode");
        assert_eq!(number(&command, 0), Some(1));
        let identity = decode_root(bytes(&command, 1).expect("identity bytes should exist"))
            .expect("identity should decode");
        assert_eq!(number(&identity, 0), Some(789));
        assert_eq!(number(&identity, 4), Some(123));
        assert_eq!(number(&identity, 5), Some(456));
        assert_eq!(number(&identity, 6), Some(789));
    }

    #[test]
    fn decodes_uri_1400_danmaku_notice() {
        let mut sender = Vec::new();
        write_number(&mut sender, 0, 42);
        write_string(&mut sender, 2, "viewer");
        write_string(&mut sender, 4, "https://example.com/avatar.jpg");

        let mut format = Vec::new();
        write_number(&mut format, 0, 0x12_ab_34);

        let mut notice = Vec::new();
        write_head(&mut notice, 0, STRUCT_BEGIN);
        notice.extend_from_slice(&sender);
        write_head(&mut notice, 0, STRUCT_END);
        write_string(&mut notice, 3, "hello Huya");
        write_head(&mut notice, 6, STRUCT_BEGIN);
        notice.extend_from_slice(&format);
        write_head(&mut notice, 0, STRUCT_END);

        let mut push = Vec::new();
        write_number(&mut push, 1, 1400);
        write_bytes(&mut push, 2, &notice);
        write_number(&mut push, 5, 987_654_321);

        let mut command = Vec::new();
        write_number(&mut command, 0, 7);
        write_bytes(&mut command, 1, &push);

        let decoded = decode_danmaku(&command)
            .expect("danmaku frame should decode")
            .expect("URI 1400 should produce a danmaku");
        assert_eq!(decoded.id, 987_654_321);
        assert_eq!(decoded.user_id, 42);
        assert_eq!(decoded.user_name, "viewer");
        assert_eq!(decoded.text, "hello Huya");
        assert_eq!(decoded.color, Some(0x12_ab_34));
        assert_eq!(
            decoded.avatar_url.as_deref(),
            Some("https://example.com/avatar.jpg")
        );
    }
}
