pub(crate) fn take_instance(value: &mut String) -> Option<String> {
    let value = std::mem::take(value);
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

mod acfun;
mod cctv;
mod cloudreve;
mod douyu;
mod fnos;
mod huya;
mod nextcloud;
mod qnap;
mod seafile;
mod synology;
mod truenas;
mod youtube;

pub(crate) use acfun::*;
pub(crate) use cctv::*;
pub(crate) use cloudreve::*;
pub(crate) use douyu::*;
pub(crate) use fnos::*;
pub(crate) use huya::*;
pub(crate) use nextcloud::*;
pub(crate) use qnap::*;
pub(crate) use seafile::*;
pub(crate) use synology::*;
pub(crate) use truenas::*;
pub(crate) use youtube::*;
