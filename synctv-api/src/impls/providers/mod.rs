//! Provider API implementations.

pub mod alist;
pub mod bilibili;
pub mod common;
pub mod emby;
pub mod playback;
pub mod rtmp;

pub use alist::AlistApiImpl;
pub use bilibili::BilibiliApiImpl;
pub use common::ProviderCommonApiImpl;
pub(crate) use common::{
    extract_instance_name, get_provider_binds, get_provider_credentials,
    resolve_bound_instance_name,
};
pub use emby::EmbyApiImpl;
