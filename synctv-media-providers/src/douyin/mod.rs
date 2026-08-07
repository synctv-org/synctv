mod chat;
mod client;
mod sign;
mod types;

pub(crate) mod proto {
    include!(concat!(
        env!("SYNCTV_MEDIA_PROVIDERS_PROTO_OUT_DIR"),
        "/douyin.rs"
    ));
}

pub use chat::{watch_danmaku, DouyinDanmakuStream};
pub use client::{DouyinClient, DouyinEndpoints};
pub use types::{
    DouyinAuthor, DouyinDanmakuEvent, DouyinImage, DouyinListItem, DouyinListPage, DouyinMedia,
    DouyinMediaKind, DouyinMetadata, DouyinResource, DouyinSession, DouyinStreamFormat,
    DouyinVariant,
};
