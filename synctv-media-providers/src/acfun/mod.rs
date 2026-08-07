mod chat;
mod client;
mod crypto;
mod live_protocol;
mod types;

#[allow(dead_code, clippy::enum_variant_names, clippy::unreadable_literal)]
pub(crate) mod proto {
    include!(concat!(
        env!("SYNCTV_MEDIA_PROVIDERS_PROTO_OUT_DIR"),
        "/acproto.rs"
    ));
}

pub use chat::{watch_danmaku, AcFunDanmakuStream};
pub use client::{AcFunClient, AcFunEndpoints};
pub use types::{
    AcFunDanmaku, AcFunLiveDanmakuEvent, AcFunLiveSession, AcFunMedia, AcFunMetadata,
    AcFunPlayback, AcFunQuality, AcFunResource, AcFunResourceKind, AcFunSession, AcFunStreamFormat,
};
