pub const OPAQUE_CIPHERSUITE_RISTRETTO255_SHA512_ARGON2ID: &str =
    "opaque-ristretto255-sha512-argon2id";
pub const OPAQUE_SERVER_SETUP_VERSION: i32 = 1;

#[derive(Debug, Clone)]
pub struct OpaquePasswordRecord {
    pub record: Vec<u8>,
    pub credential_identifier: Vec<u8>,
    pub ciphersuite: String,
    pub server_setup_version: i32,
}
