//! Native Huya live and video client.

mod chat;
mod client;
mod sign;
mod tars;
mod types;

pub use chat::{watch_danmaku, HuyaDanmakuStream};
pub use client::{HuyaClient, HuyaEndpoints};
pub use types::{
    HuyaChatIdentity, HuyaDanmakuEvent, HuyaMedia, HuyaMetadata, HuyaPlayback, HuyaQuality,
    HuyaResource, HuyaResourceKind, HuyaSession, HuyaStreamFormat,
};
