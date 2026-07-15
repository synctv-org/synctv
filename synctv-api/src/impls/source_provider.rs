use synctv_core::models::SourceProvider;
use synctv_proto::source_config as source_config_proto;

use super::ApiError;

pub(crate) fn proto_source_provider_required(value: i32) -> Result<SourceProvider, ApiError> {
    match source_config_proto::SourceProvider::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Unsupported source_provider".to_string()))?
    {
        source_config_proto::SourceProvider::Unspecified => Err(ApiError::InvalidInput(
            "source_provider is required".to_string(),
        )),
        source_config_proto::SourceProvider::DirectUrl => Ok(SourceProvider::DirectUrl),
        source_config_proto::SourceProvider::Bilibili => Ok(SourceProvider::Bilibili),
        source_config_proto::SourceProvider::Alist => Ok(SourceProvider::Alist),
        source_config_proto::SourceProvider::Emby => Ok(SourceProvider::Emby),
        source_config_proto::SourceProvider::Rtmp => Ok(SourceProvider::Rtmp),
        source_config_proto::SourceProvider::LiveProxy => Ok(SourceProvider::LiveProxy),
        source_config_proto::SourceProvider::Cloudreve => Ok(SourceProvider::Cloudreve),
        source_config_proto::SourceProvider::Twitch => Ok(SourceProvider::Twitch),
        source_config_proto::SourceProvider::Huya => Ok(SourceProvider::Huya),
        source_config_proto::SourceProvider::Douyu => Ok(SourceProvider::Douyu),
        source_config_proto::SourceProvider::Douyin => Ok(SourceProvider::Douyin),
        source_config_proto::SourceProvider::Tiktok => Ok(SourceProvider::TikTok),
        source_config_proto::SourceProvider::Acfun => Ok(SourceProvider::AcFun),
        source_config_proto::SourceProvider::Cctv => Ok(SourceProvider::Cctv),
        source_config_proto::SourceProvider::Fnos => Ok(SourceProvider::Fnos),
        source_config_proto::SourceProvider::Qnap => Ok(SourceProvider::Qnap),
        source_config_proto::SourceProvider::Synology => Ok(SourceProvider::Synology),
        source_config_proto::SourceProvider::Nextcloud => Ok(SourceProvider::Nextcloud),
        source_config_proto::SourceProvider::Seafile => Ok(SourceProvider::Seafile),
        source_config_proto::SourceProvider::Truenas => Ok(SourceProvider::TrueNas),
        source_config_proto::SourceProvider::Youtube => Ok(SourceProvider::Youtube),
    }
}

pub(crate) fn proto_source_provider_filter(value: i32) -> Result<Option<SourceProvider>, ApiError> {
    if value == source_config_proto::SourceProvider::Unspecified as i32 {
        Ok(None)
    } else {
        proto_source_provider_required(value).map(Some)
    }
}

pub(crate) fn proto_source_provider_vec(values: Vec<i32>) -> Result<Vec<SourceProvider>, ApiError> {
    values
        .into_iter()
        .map(proto_source_provider_required)
        .collect()
}

pub(crate) const fn core_source_provider_to_proto(provider: SourceProvider) -> i32 {
    match provider {
        SourceProvider::DirectUrl => source_config_proto::SourceProvider::DirectUrl as i32,
        SourceProvider::Bilibili => source_config_proto::SourceProvider::Bilibili as i32,
        SourceProvider::Alist => source_config_proto::SourceProvider::Alist as i32,
        SourceProvider::Emby => source_config_proto::SourceProvider::Emby as i32,
        SourceProvider::Rtmp => source_config_proto::SourceProvider::Rtmp as i32,
        SourceProvider::LiveProxy => source_config_proto::SourceProvider::LiveProxy as i32,
        SourceProvider::Cloudreve => source_config_proto::SourceProvider::Cloudreve as i32,
        SourceProvider::Twitch => source_config_proto::SourceProvider::Twitch as i32,
        SourceProvider::Huya => source_config_proto::SourceProvider::Huya as i32,
        SourceProvider::Douyu => source_config_proto::SourceProvider::Douyu as i32,
        SourceProvider::Douyin => source_config_proto::SourceProvider::Douyin as i32,
        SourceProvider::AcFun => source_config_proto::SourceProvider::Acfun as i32,
        SourceProvider::Cctv => source_config_proto::SourceProvider::Cctv as i32,
        SourceProvider::Fnos => source_config_proto::SourceProvider::Fnos as i32,
        SourceProvider::Qnap => source_config_proto::SourceProvider::Qnap as i32,
        SourceProvider::Synology => source_config_proto::SourceProvider::Synology as i32,
        SourceProvider::Nextcloud => source_config_proto::SourceProvider::Nextcloud as i32,
        SourceProvider::Seafile => source_config_proto::SourceProvider::Seafile as i32,
        SourceProvider::TrueNas => source_config_proto::SourceProvider::Truenas as i32,
        SourceProvider::Youtube => source_config_proto::SourceProvider::Youtube as i32,
        SourceProvider::TikTok => source_config_proto::SourceProvider::Tiktok as i32,
    }
}

pub(crate) fn core_source_provider_vec_to_proto(providers: &[SourceProvider]) -> Vec<i32> {
    providers
        .iter()
        .copied()
        .map(core_source_provider_to_proto)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROVIDER_MAPPINGS: &[(SourceProvider, source_config_proto::SourceProvider)] = &[
        (
            SourceProvider::DirectUrl,
            source_config_proto::SourceProvider::DirectUrl,
        ),
        (
            SourceProvider::Bilibili,
            source_config_proto::SourceProvider::Bilibili,
        ),
        (
            SourceProvider::Alist,
            source_config_proto::SourceProvider::Alist,
        ),
        (
            SourceProvider::Emby,
            source_config_proto::SourceProvider::Emby,
        ),
        (
            SourceProvider::Rtmp,
            source_config_proto::SourceProvider::Rtmp,
        ),
        (
            SourceProvider::LiveProxy,
            source_config_proto::SourceProvider::LiveProxy,
        ),
        (
            SourceProvider::Cloudreve,
            source_config_proto::SourceProvider::Cloudreve,
        ),
        (
            SourceProvider::Twitch,
            source_config_proto::SourceProvider::Twitch,
        ),
        (
            SourceProvider::Huya,
            source_config_proto::SourceProvider::Huya,
        ),
        (
            SourceProvider::Douyu,
            source_config_proto::SourceProvider::Douyu,
        ),
        (
            SourceProvider::AcFun,
            source_config_proto::SourceProvider::Acfun,
        ),
        (
            SourceProvider::Cctv,
            source_config_proto::SourceProvider::Cctv,
        ),
        (
            SourceProvider::Fnos,
            source_config_proto::SourceProvider::Fnos,
        ),
        (
            SourceProvider::Qnap,
            source_config_proto::SourceProvider::Qnap,
        ),
    ];

    #[test]
    fn proto_core_source_provider_round_trips_all_variants() {
        for (core, proto) in PROVIDER_MAPPINGS {
            let proto_value = *proto as i32;
            assert_eq!(core_source_provider_to_proto(*core), proto_value);
            assert_eq!(
                proto_source_provider_required(proto_value)
                    .expect("proto source provider should convert to core"),
                *core
            );
            assert_eq!(
                proto_source_provider_filter(proto_value)
                    .expect("proto source provider filter should convert to core"),
                Some(*core)
            );
        }
    }

    #[test]
    fn source_provider_filter_treats_unspecified_as_absent() {
        assert_eq!(
            proto_source_provider_filter(source_config_proto::SourceProvider::Unspecified as i32)
                .expect("unspecified source provider should be accepted as absent"),
            None
        );
        assert!(matches!(
            proto_source_provider_required(
                source_config_proto::SourceProvider::Unspecified as i32
            ),
            Err(ApiError::InvalidInput(message)) if message.contains("required")
        ));
    }

    #[test]
    fn source_provider_vec_uses_same_mapping() {
        let proto_values = PROVIDER_MAPPINGS
            .iter()
            .map(|(_, proto)| *proto as i32)
            .collect::<Vec<_>>();
        let core_values = PROVIDER_MAPPINGS
            .iter()
            .map(|(core, _)| *core)
            .collect::<Vec<_>>();

        assert_eq!(
            proto_source_provider_vec(proto_values)
                .expect("proto source provider vector should convert to core"),
            core_values.clone()
        );
        assert_eq!(
            core_source_provider_vec_to_proto(&core_values),
            PROVIDER_MAPPINGS
                .iter()
                .map(|(_, proto)| *proto as i32)
                .collect::<Vec<_>>()
        );
    }
}
