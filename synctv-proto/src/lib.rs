#![allow(clippy::missing_errors_doc)]

//! Facade for SyncTV protocol domains.

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

#[cfg(feature = "main")]
pub use synctv_proto_main::{
    DESCRIPTOR_POOL, FILE_DESCRIPTOR_SET, FieldMask, admin, client, common, google, source_config,
};

#[cfg(feature = "providers")]
pub use synctv_proto_providers::{
    PROVIDERS_DESCRIPTOR_POOL, PROVIDERS_FILE_DESCRIPTOR_SET, providers,
};

#[cfg(feature = "playback-provider")]
pub use synctv_proto_playback_provider::{
    PLAYBACK_PROVIDER_DESCRIPTOR_POOL, PLAYBACK_PROVIDER_FILE_DESCRIPTOR_SET, playback_provider,
};

#[cfg(any(feature = "main", feature = "providers", feature = "playback-provider"))]
pub fn validate<M: prost_reflect::ReflectMessage>(
    message: &M,
) -> Result<(), prost_protovalidate::Error> {
    prost_protovalidate::validate(message)
}
