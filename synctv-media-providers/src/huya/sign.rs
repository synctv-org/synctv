use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use md5::{Digest, Md5};
use url::form_urlencoded;
use uuid::Uuid;

use crate::ProviderClientError;

fn md5_hex(value: impl AsRef<[u8]>) -> String {
    hex::encode(Md5::digest(value))
}

fn guest_uid() -> u64 {
    let bytes = Uuid::new_v4().into_bytes();
    12_340_000 + u64::from(u16::from_be_bytes([bytes[0], bytes[1]])) % 10_000
}

fn rotated_uid(uid: u64) -> u64 {
    let bytes = uid.to_le_bytes();
    let low = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    (uid & !0xffff_ffff) | u64::from(low.rotate_left(8))
}

pub(crate) fn sign_anti_code(
    stream_name: &str,
    anti_code: &str,
    presenter_uid: Option<u64>,
    bitrate: u64,
) -> Result<String, ProviderClientError> {
    let params = form_urlencoded::parse(anti_code.as_bytes())
        .into_owned()
        .collect::<std::collections::HashMap<_, _>>();
    let Some(fm) = params.get("fm") else {
        return Ok(anti_code.to_string());
    };
    let ws_time = params.get("wsTime").cloned().unwrap_or_else(|| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        format!("{:x}", now + 86_400)
    });
    let ctype = params
        .get("ctype")
        .cloned()
        .unwrap_or_else(|| "huya_live".to_string());
    let uid = presenter_uid
        .filter(|value| *value != 0 && !stream_name.starts_with(&value.to_string()))
        .unwrap_or_else(guest_uid);
    let user = rotated_uid(uid);
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let seq_id = timestamp_ms.saturating_add(u128::from(uid));
    let secret_hash = md5_hex(format!("{seq_id}|{ctype}|100"));
    let preserve_original = |error: String| {
        if params.contains_key("wsSecret") && params.contains_key("wsTime") {
            tracing::warn!(%error, "Using Huya's original signed anti-code");
            Ok(anti_code.to_string())
        } else {
            Err(ProviderClientError::InvalidConfig(error))
        }
    };
    let decoded = match base64::engine::general_purpose::STANDARD.decode(fm) {
        Ok(decoded) => decoded,
        Err(error) => return preserve_original(error.to_string()),
    };
    let template = match String::from_utf8(decoded) {
        Ok(template) => template,
        Err(error) => return preserve_original(error.to_string()),
    };
    let prefix = template.split('_').next().unwrap_or_default();
    let ws_secret = md5_hex(format!(
        "{prefix}_{user}_{stream_name}_{secret_hash}_{ws_time}"
    ));

    let mut output = form_urlencoded::Serializer::new(String::new());
    output.append_pair("wsSecret", &ws_secret);
    output.append_pair("wsTime", &ws_time);
    output.append_pair("seqid", &seq_id.to_string());
    output.append_pair("ctype", &ctype);
    output.append_pair("ver", "1");
    if let Some(fs) = params.get("fs") {
        output.append_pair("fs", fs);
    }
    output.append_pair("fm", fm);
    output.append_pair("t", "100");
    output.append_pair("u", &user.to_string());
    if bitrate != 0 {
        output.append_pair("ratio", &bitrate.to_string());
    }
    Ok(output.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_each_huya_quality() {
        let query = sign_anti_code(
            "stream-name",
            "wsTime=65aa0000&fm=YWJjX3RlbXBsYXRl&ctype=huya_live&fs=1",
            Some(12_345_678),
            4_000,
        )
        .expect("anti-code should sign");
        let values = form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(values.get("ratio").map(String::as_str), Some("4000"));
        assert_eq!(values.get("wsTime").map(String::as_str), Some("65aa0000"));
        assert_eq!(values.get("ctype").map(String::as_str), Some("huya_live"));
        assert_eq!(values.get("wsSecret").map(String::len), Some(32));
    }

    #[test]
    fn preserves_server_anti_code_when_fm_template_is_binary() {
        let anti_code = "wsSecret=server-signature&wsTime=65aa0000&fm=%2Fw%3D%3D";

        assert_eq!(
            sign_anti_code("stream-name", anti_code, Some(12_345_678), 0)
                .expect("server anti-code should remain usable"),
            anti_code
        );
    }
}
