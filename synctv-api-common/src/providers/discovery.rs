use synctv_proto::{
    providers::common::{discovered_source, DiscoveredSource},
    source_config::{
        media_source_config, playlist_source_config, MediaSourceConfig, PlaylistSourceConfig,
    },
};

pub(crate) fn discovered_media(
    provider: media_source_config::Provider,
    provider_instance_name: Option<&str>,
) -> DiscoveredSource {
    DiscoveredSource {
        source_config: Some(discovered_source::SourceConfig::Media(MediaSourceConfig {
            provider: Some(provider),
        })),
        provider_instance_name: provider_instance_name.unwrap_or_default().to_string(),
    }
}

pub(crate) fn discovered_playlist(
    provider: playlist_source_config::Provider,
    provider_instance_name: Option<&str>,
) -> DiscoveredSource {
    DiscoveredSource {
        source_config: Some(discovered_source::SourceConfig::Playlist(
            PlaylistSourceConfig {
                provider: Some(provider),
            },
        )),
        provider_instance_name: provider_instance_name.unwrap_or_default().to_string(),
    }
}
