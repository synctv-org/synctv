#![allow(clippy::missing_errors_doc)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! `SyncTV` Protocol Definitions
//!
//! This crate contains all protobuf definitions and generated code for `SyncTV`'s
//! external APIs.

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

mod field_mask;
pub use field_mask::FieldMask;

pub static DESCRIPTOR_POOL: std::sync::LazyLock<prost_reflect::DescriptorPool> =
    std::sync::LazyLock::new(|| {
        prost_reflect::DescriptorPool::decode(FILE_DESCRIPTOR_SET)
            .expect("synctv-proto descriptor pool must decode")
    });

/// Encoded file descriptor set for client/admin/oauth2 proto definitions.
/// Used by tonic-reflection to serve gRPC server reflection.
pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(
    env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
    "/descriptor.bin"
));

pub fn validate<M: prost_reflect::ReflectMessage>(
    message: &M,
) -> Result<(), prost_protovalidate::Error> {
    prost_protovalidate::validate(message)
}

// Common shared types (enums, RoomMember)
#[allow(clippy::pedantic)]
pub mod google {
    pub mod rpc {
        include!(concat!(env!("SYNCTV_PROTO_MAIN_OUT_DIR"), "/google.rpc.rs"));
        include!(concat!(
            env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
            "/google.rpc.serde.rs"
        ));
    }
}

// Common shared types (enums, RoomMember)
#[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
#[allow(clippy::pedantic)]
pub mod common {
    include!(concat!(
        env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
        "/synctv.common.rs"
    ));
    include!(concat!(
        env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
        "/synctv.common.serde.rs"
    ));
}

// Provider source configuration contracts
#[allow(clippy::large_enum_variant)]
#[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
#[allow(clippy::pedantic)]
pub mod source_config {
    include!(concat!(
        env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
        "/synctv.source_config.rs"
    ));
    include!(concat!(
        env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
        "/synctv.source_config.serde.rs"
    ));
}

// Client API
#[allow(clippy::large_enum_variant)]
#[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
#[allow(clippy::pedantic)]
pub mod client {
    include!(concat!(
        env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
        "/synctv.client.rs"
    ));
    include!(concat!(
        env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
        "/synctv.client.serde.rs"
    ));
}

// Admin API
#[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
#[allow(clippy::pedantic)]
pub mod admin {
    include!(concat!(
        env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
        "/synctv.admin.rs"
    ));
    include!(concat!(
        env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
        "/synctv.admin.serde.rs"
    ));
}
