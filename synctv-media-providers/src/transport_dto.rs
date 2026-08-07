//! Provider transport DTOs used by local and remote provider clients.
//!
//! Core provider adapters depend on this module as their upstream provider
//! transport boundary. gRPC server/client registration stays in `grpc`.

pub mod alist {
    pub use crate::grpc::alist::*;
}

pub mod bilibili {
    pub use crate::grpc::bilibili::*;
}

pub mod emby {
    pub use crate::grpc::emby::*;
}
