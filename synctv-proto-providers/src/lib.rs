#![allow(clippy::missing_errors_doc)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! `SyncTV` Protocol Definitions
//!
//! This crate contains all protobuf definitions and generated code for `SyncTV`'s
//! external APIs.

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

pub use synctv_proto_main::{source_config, FieldMask};

/// Encoded file descriptor set for provider proto definitions.
pub const PROVIDERS_FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(
    env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
    "/descriptor.bin"
));

pub static PROVIDERS_DESCRIPTOR_POOL: std::sync::LazyLock<prost_reflect::DescriptorPool> =
    std::sync::LazyLock::new(|| {
        prost_reflect::DescriptorPool::decode(PROVIDERS_FILE_DESCRIPTOR_SET)
            .expect("synctv-proto provider descriptor pool must decode")
    });

pub fn validate<M: prost_reflect::ReflectMessage>(
    message: &M,
) -> Result<(), prost_protovalidate::Error> {
    prost_protovalidate::validate(message)
}

// Providers
#[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
#[allow(clippy::pedantic)]
pub mod providers {
    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod common {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.common.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.common.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod rtmp {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.rtmp.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.rtmp.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod bilibili {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.bilibili.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.bilibili.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod cloudreve {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.cloudreve.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.cloudreve.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod twitch {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.twitch.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.twitch.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod huya {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.huya.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.huya.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod douyu {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.douyu.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.douyu.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod acfun {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.acfun.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.acfun.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod cctv {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.cctv.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.cctv.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod youtube {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.youtube.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.youtube.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod douyin {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.douyin.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.douyin.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod tiktok {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.tiktok.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.tiktok.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod fnos {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.fnos.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.fnos.serde.rs"
        ));
    }

    pub mod qnap {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.qnap.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.qnap.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod synology {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.synology.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.synology.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod nextcloud {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.nextcloud.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.nextcloud.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod seafile {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.seafile.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.seafile.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod truenas {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.truenas.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.truenas.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod alist {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.alist.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.alist.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod emby {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.emby.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.emby.serde.rs"
        ));
    }
}
