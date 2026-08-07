#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

#[cfg(any(feature = "tls-aws-lc", feature = "tls-ring"))]
pub fn install_process_crypto_provider() {
    if rustls::crypto::CryptoProvider::install_default(default_crypto_provider()).is_err() {
        tracing::debug!("Process rustls crypto provider was already installed");
    }
}

#[cfg(not(any(feature = "tls-aws-lc", feature = "tls-ring")))]
pub const fn install_process_crypto_provider() {}

#[cfg(any(feature = "tls-aws-lc", feature = "tls-ring"))]
fn default_crypto_provider() -> rustls::crypto::CryptoProvider {
    #[cfg(feature = "tls-aws-lc")]
    {
        rustls::crypto::aws_lc_rs::default_provider()
    }

    #[cfg(all(not(feature = "tls-aws-lc"), feature = "tls-ring"))]
    {
        rustls::crypto::ring::default_provider()
    }
}

pub mod execution;
pub mod http;
pub mod id;
pub mod messages;
pub mod redaction;
pub mod reserved;
pub mod ssrf;
pub mod time;
pub mod validation;

pub use execution::{ExecutionControl, ExecutionControlError};
