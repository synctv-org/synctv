use aes::cipher::{block_padding::Pkcs7, BlockModeDecrypt, BlockModeEncrypt, KeyIvInit};
use rand::Rng;

use crate::ProviderClientError;

type Aes128CbcEncryptor = cbc::Encryptor<aes::Aes128>;
type Aes128CbcDecryptor = cbc::Decryptor<aes::Aes128>;

pub(super) fn encrypt(plaintext: &[u8], key: &[u8]) -> Result<Vec<u8>, ProviderClientError> {
    validate_key(key)?;
    let mut iv = [0_u8; 16];
    rand::rng().fill_bytes(&mut iv);
    let cipher = Aes128CbcEncryptor::new_from_slices(key, &iv)
        .map_err(|error| protocol_error(error.to_string()))?;
    let ciphertext = cipher.encrypt_padded_vec::<Pkcs7>(plaintext);
    let mut output = Vec::with_capacity(iv.len() + ciphertext.len());
    output.extend_from_slice(&iv);
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

pub(super) fn decrypt(ciphertext: &[u8], key: &[u8]) -> Result<Vec<u8>, ProviderClientError> {
    validate_key(key)?;
    if ciphertext.len() <= 16 {
        return Err(protocol_error("encrypted payload is too short"));
    }
    let (iv, body) = ciphertext.split_at(16);
    Aes128CbcDecryptor::new_from_slices(key, iv)
        .map_err(|error| protocol_error(error.to_string()))?
        .decrypt_padded_vec::<Pkcs7>(body)
        .map_err(|error| protocol_error(error.to_string()))
}

fn validate_key(key: &[u8]) -> Result<(), ProviderClientError> {
    if key.len() == 16 {
        Ok(())
    } else {
        Err(protocol_error(format!(
            "AES-128 key has {} bytes",
            key.len()
        )))
    }
}

fn protocol_error(message: impl Into<String>) -> ProviderClientError {
    ProviderClientError::Parse(format!("AcFun live encryption error: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aes_cbc_round_trip() {
        let key = b"0123456789abcdef";
        let encrypted = encrypt(b"AcFun live payload", key).expect("test operation should succeed");
        assert_ne!(&encrypted[16..], b"AcFun live payload");
        assert_eq!(
            decrypt(&encrypted, key).expect("test operation should succeed"),
            b"AcFun live payload"
        );
    }
}
