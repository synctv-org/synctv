use synctv_media_providers::huya::{HuyaMedia, HuyaMetadata, HuyaResourceKind, HuyaStreamFormat};
use synctv_proto::providers::huya::{self as proto, *};
use synctv_proto::source_config::{self as source_proto, huya_media_source_config};

use super::discovery::discovered_media;

pub fn resolve_response(media: HuyaMedia, provider_instance_name: Option<&str>) -> ResolveResponse {
    let kind = resource_kind(media.playback.resource.kind);
    let source = match media.playback.resource.kind {
        HuyaResourceKind::Live => {
            huya_media_source_config::Source::Live(source_proto::HuyaLiveSourceConfig {
                room_id: media.playback.resource.id.clone(),
            })
        }
        HuyaResourceKind::Video => {
            huya_media_source_config::Source::Video(source_proto::HuyaVideoSourceConfig {
                video_id: media.playback.resource.id.clone(),
            })
        }
    };
    ResolveResponse {
        kind,
        metadata: Some(metadata_message(media.metadata)),
        qualities: media
            .playback
            .qualities
            .into_iter()
            .map(|quality| Quality {
                name: quality.name,
                cdn: quality.cdn,
                format: stream_format(quality.format),
                url: quality.url,
                bitrate: quality.bitrate,
                width: quality.width,
                height: quality.height,
            })
            .collect(),
        source: Some(discovered_media(
            source_proto::media_source_config::Provider::Huya(
                source_proto::HuyaMediaSourceConfig {
                    source: Some(source),
                },
            ),
            provider_instance_name,
        )),
    }
}

fn metadata_message(metadata: HuyaMetadata) -> Metadata {
    Metadata {
        id: metadata.id,
        title: metadata.title,
        author: metadata.author,
        author_id: metadata.author_id,
        category: metadata.category,
        thumbnail_url: metadata.thumbnail_url,
        avatar_url: metadata.avatar_url,
        is_live: metadata.is_live,
        description: metadata.description,
        duration_seconds: metadata.duration_seconds,
        view_count: metadata.view_count,
        comment_count: metadata.comment_count,
        like_count: metadata.like_count,
        published_at: metadata.published_at,
    }
}

const fn resource_kind(kind: HuyaResourceKind) -> i32 {
    match kind {
        HuyaResourceKind::Live => proto::ResourceKind::Live as i32,
        HuyaResourceKind::Video => proto::ResourceKind::Video as i32,
    }
}

const fn stream_format(format: HuyaStreamFormat) -> i32 {
    match format {
        HuyaStreamFormat::Flv => proto::StreamFormat::Flv as i32,
        HuyaStreamFormat::Hls => proto::StreamFormat::Hls as i32,
    }
}
