use md5::{Digest, Md5};

use super::types::{EncryptionData, SignedRequest};

pub(crate) fn sign(
    encryption: &EncryptionData,
    room_id: &str,
    device_id: &str,
    timestamp: u64,
) -> SignedRequest {
    let mut secret = encryption.rand_str.clone();
    for _ in 0..encryption.enc_time {
        secret = md5_hex(format!("{secret}{}", encryption.key).as_bytes());
    }
    let salt = if encryption.is_special {
        String::new()
    } else {
        format!("{room_id}{timestamp}")
    };
    SignedRequest {
        auth: md5_hex(format!("{secret}{}{salt}", encryption.key).as_bytes()),
        timestamp,
        device_id: device_id.to_string(),
        enc_data: encryption.enc_data.clone(),
    }
}

fn md5_hex(input: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(input);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_matches_douyu_iterative_md5_contract() {
        let encryption = EncryptionData {
            rand_str: "seed".to_string(),
            enc_time: 2,
            key: "key".to_string(),
            is_special: false,
            enc_data: "opaque".to_string(),
        };
        let signed = sign(&encryption, "123", "device", 1_700_000_000);
        assert_eq!(signed.auth, "10182213072cf7d9afe0daacd25abf7a");
        assert_eq!(signed.timestamp, 1_700_000_000);
        assert_eq!(signed.enc_data, "opaque");
    }
}
