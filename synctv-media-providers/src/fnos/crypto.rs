use aes::Aes256;
use base64::Engine;
use cbc::cipher::{block_padding::Pkcs7, BlockModeDecrypt, BlockModeEncrypt, KeyIvInit};
use hmac::{Hmac, KeyInit, Mac};
use rsa::pkcs1::DecodeRsaPublicKey;
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
use sha2::Sha256;

use crate::ProviderClientError;

type Aes256CbcEnc = cbc::Encryptor<Aes256>;
type Aes256CbcDec = cbc::Decryptor<Aes256>;

pub(crate) struct LoginCipher {
    key: [u8; 32],
    iv: [u8; 16],
}

impl LoginCipher {
    pub(crate) fn random() -> Self {
        use rsa::rand_core::RngCore;

        let mut rng = rsa::rand_core::OsRng;
        let mut key = [0_u8; 32];
        let mut iv = [0_u8; 16];
        rng.fill_bytes(&mut key);
        rng.fill_bytes(&mut iv);
        Self { key, iv }
    }

    pub(crate) fn encrypted_request(
        &self,
        public_key: &str,
        payload: &[u8],
    ) -> Result<serde_json::Value, ProviderClientError> {
        let public_key = RsaPublicKey::from_public_key_pem(public_key)
            .or_else(|_| RsaPublicKey::from_pkcs1_pem(public_key))
            .map_err(|error| ProviderClientError::Parse(error.to_string()))?;
        let encrypted_key = public_key
            .encrypt(&mut rsa::rand_core::OsRng, Pkcs1v15Encrypt, &self.key)
            .map_err(|error| ProviderClientError::Parse(error.to_string()))?;
        let encrypted = Aes256CbcEnc::new(&self.key.into(), &self.iv.into())
            .encrypt_padded_vec::<Pkcs7>(payload);
        let base64 = base64::engine::general_purpose::STANDARD;
        Ok(serde_json::json!({
            "req": "encrypted",
            "iv": base64.encode(self.iv),
            "rsa": base64.encode(encrypted_key),
            "aes": base64.encode(encrypted),
        }))
    }

    pub(crate) fn decrypt_secret(&self, encrypted: &str) -> Result<String, ProviderClientError> {
        let base64 = base64::engine::general_purpose::STANDARD;
        let encrypted = base64
            .decode(encrypted)
            .map_err(|error| ProviderClientError::Parse(error.to_string()))?;
        let decrypted = Aes256CbcDec::new(&self.key.into(), &self.iv.into())
            .decrypt_padded_vec::<Pkcs7>(&encrypted)
            .map_err(|error| ProviderClientError::Parse(error.to_string()))?;
        Ok(base64.encode(decrypted))
    }
}

pub(crate) fn sign_request(secret: &str, payload: &str) -> Result<String, ProviderClientError> {
    let secret = base64::engine::general_purpose::STANDARD
        .decode(secret)
        .map_err(|error| ProviderClientError::Parse(error.to_string()))?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&secret)
        .map_err(|error| ProviderClientError::Parse(error.to_string()))?;
    mac.update(payload.as_bytes());
    Ok(format!(
        "{}{}",
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes()),
        payload
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_requests_with_base64_hmac_prefix() {
        let signed = sign_request("c2VjcmV0", r#"{"req":"file.ls"}"#)
            .expect("test operation should succeed");
        assert!(signed.ends_with(r#"{"req":"file.ls"}"#));
        assert_eq!(signed.len(), 44 + r#"{"req":"file.ls"}"#.len());
    }
}
