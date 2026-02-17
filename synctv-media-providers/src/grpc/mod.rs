//! gRPC Provider Services
//!
//! This module contains gRPC server implementations for all providers.
//! Generated code is committed under `src/proto/` (same pattern as `synctv-proto`).

// Include generated protobuf code
pub mod alist {
    include!("../proto/synctv.media.alist.rs");
}

pub mod bilibili {
    include!("../proto/synctv.media.bilibili.rs");
}

pub mod emby {
    include!("../proto/synctv.media.emby.rs");
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
