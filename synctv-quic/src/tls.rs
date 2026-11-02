//! TLS configuration for the QUIC listener.

use std::sync::Arc;

/// Returns the process-wide rustls crypto provider, installing the one
/// selected by this crate's TLS feature when none is installed yet. Builds
/// that link both providers (e.g. test clients) cannot rely on rustls'
/// automatic selection, so every config here goes through this helper.
#[must_use]
pub fn crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    if let Some(provider) = rustls::crypto::CryptoProvider::get_default() {
        return Arc::clone(provider);
    }
    #[cfg(feature = "tls-ring")]
    let provider = rustls::crypto::ring::default_provider();
    #[cfg(not(feature = "tls-ring"))]
    let provider = rustls::crypto::aws_lc_rs::default_provider();
    // A concurrent installer may win the race; either provider is fine.
    let _ = provider.clone().install_default();
    rustls::crypto::CryptoProvider::get_default().map_or_else(|| Arc::new(provider), Arc::clone)
}

fn server_config_builder() -> rustls::ConfigBuilder<rustls::ServerConfig, rustls::WantsVerifier> {
    rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("the selected provider supports TLS 1.3")
}

/// Adds the HTTP/3 ALPN to a rustls server config.
#[must_use]
pub fn rustls_config_with_alpn(mut config: rustls::ServerConfig) -> rustls::ServerConfig {
    config.alpn_protocols = crate::H3_ALPN
        .iter()
        .map(|alpn| alpn.as_bytes().to_vec())
        .collect();
    config
}

/// Loads a PEM certificate chain + key into an HTTP/3 rustls server config.
///
/// # Errors
///
/// Fails when the files cannot be read or parsed, or the key does not match
/// the certificate.
pub async fn load_tls_server_config(
    cert_path: &str,
    key_path: &str,
) -> anyhow::Result<rustls::ServerConfig> {
    let (cert_pem, key_pem) = tokio::join!(tokio::fs::read(cert_path), tokio::fs::read(key_path));
    let cert_pem =
        cert_pem.map_err(|error| anyhow::anyhow!("failed to read {cert_path}: {error}"))?;
    let key_pem = key_pem.map_err(|error| anyhow::anyhow!("failed to read {key_path}: {error}"))?;

    let mut cert_reader = std::io::BufReader::new(cert_pem.as_slice());
    let cert_chain = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| anyhow::anyhow!("failed to parse {cert_path}: {error}"))?;
    if cert_chain.is_empty() {
        anyhow::bail!("no PEM certificates found in {cert_path}");
    }

    let private_key = rustls_pemfile::private_key(&mut std::io::BufReader::new(key_pem.as_slice()))
        .map_err(|error| anyhow::anyhow!("failed to parse {key_path}: {error}"))?
        .ok_or_else(|| anyhow::anyhow!("no supported PEM private key found in {key_path}"))?;

    let config = server_config_builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, private_key)
        .map_err(|error| anyhow::anyhow!("invalid certificate/key pair: {error}"))?;
    Ok(rustls_config_with_alpn(config))
}

/// Computes the base64 SHA-256 of the first certificate's DER encoding in a
/// PEM file: the value clients pass as a WebTransport `serverCertificateHashes`
/// entry.
///
/// # Errors
///
/// Fails when the file cannot be read or contains no certificate.
pub async fn certificate_hash_base64(cert_path: &str) -> anyhow::Result<String> {
    use base64::Engine as _;
    use sha2::Digest as _;

    let pem = tokio::fs::read(cert_path)
        .await
        .map_err(|error| anyhow::anyhow!("failed to read {cert_path}: {error}"))?;
    let der = rustls_pemfile::certs(&mut std::io::BufReader::new(pem.as_slice()))
        .next()
        .transpose()
        .map_err(|error| anyhow::anyhow!("failed to parse {cert_path}: {error}"))?
        .ok_or_else(|| anyhow::anyhow!("no PEM certificates found in {cert_path}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(der.as_ref())))
}

/// Wraps a rustls config into a Quinn server config.
#[must_use]
pub fn quinn_server_config(config: rustls::ServerConfig) -> quinn::ServerConfig {
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(config)
        .expect("rustls config with TLS 1.3 converts to a Quinn server config");
    quinn::ServerConfig::with_crypto(Arc::new(crypto))
}

/// Generates an ephemeral self-signed ECDSA P-256 certificate for local
/// development and tests. Browsers accept such certificates for WebTransport
/// only through the `serverCertificateHashes` option, whose
/// constraints (ECDSA, validity of at most 14 days) this helper honors.
///
/// Returns the ready-to-use server config and the DER certificate.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn generate_self_signed_config() -> (rustls::ServerConfig, Vec<u8>) {
    let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()])
        .expect("default certificate params are valid");
    params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(1);
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(13);
    let key_pair = rcgen::KeyPair::generate().expect("key generation is infallible");
    let cert = params
        .self_signed(&key_pair)
        .expect("self-signed generation is infallible");

    let der = cert.der().clone();
    let pkcs8 = key_pair.serialize_der();
    let config = server_config_builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![der.clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
                pkcs8,
            )),
        )
        .expect("fresh self-signed pair is valid");
    (rustls_config_with_alpn(config), der.as_ref().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn certificate_hash_matches_the_der_encoding() {
        use base64::Engine as _;
        use sha2::Digest as _;

        let dir = std::env::temp_dir().join(format!(
            "synctv-quic-tls-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after the epoch")
                .as_millis()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");

        let mut params =
            rcgen::CertificateParams::new(vec!["localhost".to_string()]).expect("params");
        params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(1);
        params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(13);
        let key_pair = rcgen::KeyPair::generate().expect("key pair");
        let cert = params.self_signed(&key_pair).expect("cert");
        let cert_path = dir.join("cert.pem");
        std::fs::write(&cert_path, cert.pem()).expect("write pem");

        let hash = certificate_hash_base64(cert_path.to_str().expect("utf8 path"))
            .await
            .expect("hash");
        assert_eq!(
            hash,
            base64::engine::general_purpose::STANDARD
                .encode(sha2::Sha256::digest(cert.der().as_ref())),
            "hash covers the DER encoding, not the PEM bytes"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
