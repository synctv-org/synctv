// A-Bogus follows the public f2 implementation (Apache-2.0):
// https://github.com/Johnserf-Seed/f2/blob/main/f2/utils/abogus.py

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use md5::{Digest as _, Md5};
use rand::RngExt;
use sm3::Sm3;

const A_BOGUS_ALPHABET: &[u8; 64] =
    b"Dkdpgh2ZmsQB80/MfvV36XI1R45-WUAlEixNLwoqYTOPuzKFjJnry79HbGcaStCe";
const UA_ALPHABET: &[u8; 64] = b"ckdp1h4ZKsUB80/Mfvw36XIgR25+WQAlEi7NLboqYTOPuzmFjJnryx9HVGDaStCe";
const X_BOGUS_ALPHABET: &[u8; 64] =
    b"Dkdpgh4ZKsQB80/Mfvw36XI1R25+WUAlEi7NLboqYTOPuzmFjJnryx9HVGcaStCe";
const SORT_INDEX: [u8; 44] = [
    18, 20, 52, 26, 30, 34, 58, 38, 40, 53, 42, 21, 27, 54, 55, 31, 35, 57, 39, 41, 43, 22, 28, 32,
    60, 36, 23, 29, 33, 37, 44, 45, 59, 46, 47, 48, 49, 50, 24, 25, 65, 66, 70, 71,
];
const XOR_INDEX: [u8; 44] = [
    18, 20, 26, 30, 34, 38, 40, 42, 21, 27, 31, 35, 39, 41, 43, 22, 28, 32, 36, 23, 29, 33, 37, 44,
    45, 46, 47, 48, 49, 50, 24, 25, 52, 53, 54, 55, 57, 58, 59, 60, 65, 66, 70, 71,
];
const BIG_ARRAY: [u8; 256] = [
    121, 243, 55, 234, 103, 36, 47, 228, 30, 231, 106, 6, 115, 95, 78, 101, 250, 207, 198, 50, 139,
    227, 220, 105, 97, 143, 34, 28, 194, 215, 18, 100, 159, 160, 43, 8, 169, 217, 180, 120, 247,
    45, 90, 11, 27, 197, 46, 3, 84, 72, 5, 68, 62, 56, 221, 75, 144, 79, 73, 161, 178, 81, 64, 187,
    134, 117, 186, 118, 16, 241, 130, 71, 89, 147, 122, 129, 65, 40, 88, 150, 110, 219, 199, 255,
    181, 254, 48, 4, 195, 248, 208, 32, 116, 167, 69, 201, 17, 124, 125, 104, 96, 83, 80, 127, 236,
    108, 154, 126, 204, 15, 20, 135, 112, 158, 13, 1, 188, 164, 210, 237, 222, 98, 212, 77, 253,
    42, 170, 202, 26, 22, 29, 182, 251, 10, 173, 152, 58, 138, 54, 141, 185, 33, 157, 31, 252, 132,
    233, 235, 102, 196, 191, 223, 240, 148, 39, 123, 92, 82, 128, 109, 57, 24, 38, 113, 209, 245,
    2, 119, 153, 229, 189, 214, 230, 174, 232, 63, 52, 205, 86, 140, 66, 175, 111, 171, 246, 133,
    238, 193, 99, 60, 74, 91, 225, 51, 76, 37, 145, 211, 166, 151, 213, 206, 0, 200, 244, 176, 218,
    44, 184, 172, 49, 216, 93, 168, 53, 21, 183, 41, 67, 85, 224, 155, 226, 242, 87, 177, 146, 70,
    190, 12, 162, 19, 137, 114, 25, 165, 163, 192, 23, 59, 9, 94, 179, 107, 35, 7, 142, 131, 239,
    203, 149, 136, 61, 249, 14, 156,
];

pub(crate) fn generate_verify_fp() -> String {
    const CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let timestamp = now_ms();
    let mut rng = rand::rng();
    let mut id = [0_u8; 36];
    for (index, value) in id.iter_mut().enumerate() {
        *value = match index {
            8 | 13 | 18 | 23 => b'_',
            14 => b'4',
            19 => CHARS[((rng.random_range(0..CHARS.len()) & 3) | 8) % CHARS.len()],
            _ => CHARS[rng.random_range(0..CHARS.len())],
        };
    }
    format!(
        "verify_{}_{}",
        base36(timestamp),
        String::from_utf8_lossy(&id)
    )
}

pub(crate) fn generate_ms_token() -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";
    random_ascii(CHARS, 184)
}

pub(crate) fn generate_nonce() -> String {
    random_ascii(b"abcdef0123456789", 21)
}

pub(crate) fn generate_odin_ttid() -> String {
    random_ascii(b"abcdef0123456789", 160)
}

pub(crate) fn sign_a_bogus(params: &str, user_agent: &str) -> String {
    let timestamp = now_ms();
    let params_hash = sm3_twice(format!("{params}cus").as_bytes());
    let body_hash = sm3_twice(b"cus");
    let ua_cipher = rc4(&[0, 1, 14], user_agent.as_bytes());
    let ua_hash = sm3_digest(custom_base64(&ua_cipher, UA_ALPHABET).as_bytes());
    let fingerprint = "1920|1080|1928|1160|0|0|0|0|1920|1080|1920|1040|1920|1080|24|24|Win32";

    let mut fields = HashMap::from([
        (8, 3_u64),
        (18, 44),
        (20, (timestamp >> 24) & 255),
        (21, (timestamp >> 16) & 255),
        (22, (timestamp >> 8) & 255),
        (23, timestamp & 255),
        (24, timestamp / 0x1_0000_0000),
        (25, timestamp / 0x100_0000_0000),
        (26, 0),
        (27, 0),
        (28, 0),
        (29, 0),
        (30, 0),
        (31, 1),
        (32, 0),
        (33, 0),
        (34, 0),
        (35, 0),
        (36, 0),
        (37, 14),
        (38, u64::from(params_hash[21])),
        (39, u64::from(params_hash[22])),
        (40, u64::from(body_hash[21])),
        (41, u64::from(body_hash[22])),
        (42, u64::from(ua_hash[23])),
        (43, u64::from(ua_hash[24])),
        (44, (timestamp >> 24) & 255),
        (45, (timestamp >> 16) & 255),
        (46, (timestamp >> 8) & 255),
        (47, timestamp & 255),
        (48, 3),
        (49, timestamp / 0x1_0000_0000),
        (50, timestamp / 0x100_0000_0000),
        (52, 0),
        (53, 0),
        (54, 0),
        (55, 0),
        (57, 0xef),
        (58, 0x18),
        (59, 0),
        (60, 0),
        (65, fingerprint.len() as u64),
        (66, 0),
        (70, 0),
        (71, 0),
    ]);
    let checksum = XOR_INDEX.iter().fold(0_u32, |value, key| {
        value ^ low_u32(*fields.get(key).unwrap_or(&0))
    });
    let mut plain = SORT_INDEX
        .iter()
        .map(|key| low_u32(*fields.get(key).unwrap_or(&0)))
        .collect::<Vec<_>>();
    plain.extend(fingerprint.bytes().map(u32::from));
    plain.push(checksum);
    fields.clear();

    let mut transformed = transform(&plain);
    let mut prefix = random_prefix();
    prefix.append(&mut transformed);
    custom_base64_u32(&prefix, A_BOGUS_ALPHABET)
}

pub(crate) fn generate_x_bogus(ms_stub: &[u8; 32], counter: u8) -> String {
    let random1 = rand::random::<u8>();
    let random2 = rand::random::<u8>();
    let nested = Md5::digest(hex_decode(ms_stub));
    let empty_nested = Md5::digest(hex_decode(b"d41d8cd98f00b204e9800998ecf8427e"));
    let mut payload = [
        counter & 0x3f,
        0,
        1,
        0x0e,
        empty_nested[14],
        empty_nested[15],
        nested[14],
        nested[15],
        random2,
        0,
    ];
    payload[9] = payload[..9].iter().fold(0, |value, byte| value ^ byte);
    let encrypted = rc4(&[random2], &payload);
    let mut output = vec![0x40 | (random1 & 0x1f), random2];
    output.extend(encrypted);
    custom_base64(&output, X_BOGUS_ALPHABET)
}

pub(crate) fn md5_hex(input: &str) -> [u8; 32] {
    let digest = Md5::digest(input.as_bytes());
    let encoded = hex::encode(digest);
    let mut output = [0_u8; 32];
    output.copy_from_slice(encoded.as_bytes());
    output
}

fn sm3_twice(input: &[u8]) -> [u8; 32] {
    sm3_digest(&sm3_digest(input))
}

fn sm3_digest(input: &[u8]) -> [u8; 32] {
    let digest = Sm3::digest(input);
    let mut output = [0_u8; 32];
    output.copy_from_slice(&digest);
    output
}

fn low_u32(value: u64) -> u32 {
    let bytes = value.to_le_bytes();
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn rc4(key: &[u8], input: &[u8]) -> Vec<u8> {
    let mut state =
        core::array::from_fn::<_, 256, _>(|index| u8::try_from(index).unwrap_or_default());
    let mut j = 0_u8;
    for index in 0..256 {
        j = j
            .wrapping_add(state[index])
            .wrapping_add(key[index % key.len()]);
        state.swap(index, usize::from(j));
    }
    let (mut i, mut j) = (0_u8, 0_u8);
    input
        .iter()
        .map(|byte| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(state[usize::from(i)]);
            state.swap(usize::from(i), usize::from(j));
            byte ^ state[usize::from(state[usize::from(i)].wrapping_add(state[usize::from(j)]))]
        })
        .collect()
}

fn transform(values: &[u32]) -> Vec<u32> {
    let mut state = BIG_ARRAY;
    let mut index_b = usize::from(state[1]);
    let (mut initial, mut value_e) = (0_u8, 0_u8);
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let sum = if index == 0 {
                initial = state[index_b];
                let index_byte = u8::try_from(index_b).unwrap_or_default();
                let sum = index_byte.wrapping_add(initial);
                state[1] = initial;
                state[index_b] = index_byte;
                sum
            } else {
                initial.wrapping_add(value_e)
            };
            let output = value ^ u32::from(state[usize::from(sum)]);
            let next = (index + 2) % state.len();
            value_e = state[next];
            let swap = usize::from(
                u8::try_from(index_b)
                    .unwrap_or_default()
                    .wrapping_add(value_e),
            );
            initial = state[swap];
            state.swap(swap, next);
            index_b = swap;
            output
        })
        .collect()
}

fn custom_base64(input: &[u8], alphabet: &[u8; 64]) -> String {
    custom_base64_u32(
        &input
            .iter()
            .map(|value| u32::from(*value))
            .collect::<Vec<_>>(),
        alphabet,
    )
}

fn custom_base64_u32(input: &[u32], alphabet: &[u8; 64]) -> String {
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let value = (chunk[0] << 16)
            | (chunk.get(1).copied().unwrap_or(0) << 8)
            | chunk.get(2).copied().unwrap_or(0);
        output.push(alphabet[((value >> 18) & 63) as usize] as char);
        output.push(alphabet[((value >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            output.push(alphabet[((value >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            output.push(alphabet[(value & 63) as usize] as char);
        }
    }
    output.extend(std::iter::repeat_n('=', (4 - output.len() % 4) % 4));
    output
}

fn random_prefix() -> Vec<u32> {
    let mut rng = rand::rng();
    (0..3)
        .flat_map(|_| {
            let value = rng.random_range(0..10_000_u64);
            let bytes = value.to_le_bytes();
            [
                (bytes[0] & 0xaa) | 0x01,
                (bytes[0] & 0x55) | 0x02,
                (bytes[1] & 0xaa) | 0x05,
                (bytes[1] & 0x55) | 0x28,
            ]
        })
        .map(u32::from)
        .collect()
}

fn random_ascii(alphabet: &[u8], length: usize) -> String {
    let mut rng = rand::rng();
    (0..length)
        .map(|_| alphabet[rng.random_range(0..alphabet.len())] as char)
        .collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn base36(mut value: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut output = Vec::new();
    loop {
        output.push(DIGITS[(value % 36) as usize]);
        value /= 36;
        if value == 0 {
            break;
        }
    }
    output.reverse();
    String::from_utf8(output).expect("base36 is ASCII")
}

fn hex_decode(value: &[u8; 32]) -> [u8; 16] {
    let mut output = [0_u8; 16];
    for index in 0..16 {
        output[index] = (hex_nibble(value[index * 2]) << 4) | hex_nibble(value[index * 2 + 1]);
    }
    output
}

const fn hex_nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        b'A'..=b'F' => value - b'A' + 10,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_have_protocol_shape() {
        let fp = generate_verify_fp();
        assert!(fp.starts_with("verify_"));
        assert_eq!(generate_ms_token().len(), 184);
        assert_eq!(generate_nonce().len(), 21);
        assert_eq!(generate_odin_ttid().len(), 160);
        let bogus = sign_a_bogus("aid=6383&aweme_id=1", "test-agent");
        assert!(!bogus.is_empty());
        assert_eq!(generate_x_bogus(&md5_hex("aid=6383"), 1).len(), 16);
    }
}
