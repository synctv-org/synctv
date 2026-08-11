use super::*;

macro_rules! resolve_provider {
    ($command:expr, $variant:path, $provider:ident, $wrapper:ident, $method:ident) => {
        match $command.command {
            $variant(args) => provider_call!(
                args,
                $method,
                $wrapper,
                synctv_proto::providers::$provider::ResolveRequest {
                    resource: args.resource,
                    instance_name: String::new()
                }
            ),
        }
    };
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

pub(super) use acfun::*;
pub(super) use cctv::*;
pub(super) use cloudreve::*;
pub(super) use douyu::*;
pub(super) use fnos::*;
pub(super) use huya::*;
pub(super) use nextcloud::*;
pub(super) use qnap::*;
pub(super) use seafile::*;
pub(super) use synology::*;
pub(super) use truenas::*;
pub(super) use youtube::*;
