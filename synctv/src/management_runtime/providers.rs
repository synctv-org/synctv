mod alist;
mod bilibili;
mod common;
mod douyin;
mod emby;
mod tiktok;
mod twitch;

pub(crate) use alist::*;
pub(crate) use bilibili::*;
pub(crate) use common::*;
pub(crate) use douyin::*;
pub(crate) use emby::*;
pub(crate) use tiktok::*;
pub(crate) use twitch::*;

pub(crate) fn take_instance(value: &mut String) -> Option<String> {
    let value = std::mem::take(value);
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}
