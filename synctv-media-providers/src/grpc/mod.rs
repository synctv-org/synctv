//! gRPC Provider Services
//!
//! This module contains gRPC server implementations for all providers.
//! Generated code is included from Cargo `OUT_DIR`.

// Include generated protobuf code
pub mod alist {
    include!(concat!(
        env!("SYNCTV_MEDIA_PROVIDERS_PROTO_OUT_DIR"),
        "/synctv.media.alist.rs"
    ));
}

pub mod bilibili {
    include!(concat!(
        env!("SYNCTV_MEDIA_PROVIDERS_PROTO_OUT_DIR"),
        "/synctv.media.bilibili.rs"
    ));
}

pub mod emby {
    include!(concat!(
        env!("SYNCTV_MEDIA_PROVIDERS_PROTO_OUT_DIR"),
        "/synctv.media.emby.rs"
    ));
}

// Shared validation
pub mod validation;

// Shared gRPC error mapping
pub mod error_mapper;

// Server implementations
pub mod alist_server;
pub mod bilibili_server;
pub mod emby_server;

// Re-export server types for external registration
pub use alist_server::AlistService;
pub use bilibili_server::BilibiliService;
pub use emby_server::EmbyService;
