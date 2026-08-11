use synctv_media_providers::acfun::{
    AcFunMedia, AcFunMetadata, AcFunResourceKind, AcFunStreamFormat,
};
use synctv_proto::providers::acfun::{self as proto, *};
use synctv_proto::source_config::{self as source_proto, ac_fun_media_source_config};

use super::discovery::discovered_media;

pub fn resolve_response(
    media: AcFunMedia,
    provider_instance_name: Option<&str>,
) -> ResolveResponse {
    let kind = resource_kind(media.playback.resource.kind);
    let source = match media.playback.resource.kind {
        AcFunResourceKind::Video => {
            ac_fun_media_source_config::Source::Video(source_proto::AcFunVideoSourceConfig {
                video_id: media.playback.resource.id.clone(),
            })
        }
        AcFunResourceKind::Bangumi => {
            ac_fun_media_source_config::Source::Bangumi(source_proto::AcFunBangumiSourceConfig {
                bangumi_id: media.playback.resource.id.clone(),
                episode_query: media.playback.resource.query.clone(),
            })
        }
        AcFunResourceKind::Live => {
            ac_fun_media_source_config::Source::Live(source_proto::AcFunLiveSourceConfig {
                author_id: media.playback.resource.id.clone(),
            })
        }
    };
    let has_live_danmaku = media.live_session.is_some();
    ResolveResponse {
        kind,
        metadata: Some(metadata_message(media.metadata, has_live_danmaku)),
        qualities: media
            .playback
            .qualities
            .into_iter()
            .map(|quality| Quality {
                name: quality.name,
                url: quality.url,
                format: stream_format(quality.format),
                bitrate: quality.bitrate,
                width: quality.width,
                height: quality.height,
                fps: quality.fps,
                codec: quality.codec,
                quality_type: quality.quality_type,
            })
            .collect(),
        source: Some(discovered_media(
            source_proto::media_source_config::Provider::AcFun(
                source_proto::AcFunMediaSourceConfig {
                    source: Some(source),
                },
            ),
            provider_instance_name,
        )),
    }
}

fn metadata_message(metadata: AcFunMetadata, has_live_danmaku: bool) -> Metadata {
    Metadata {
        id: metadata.id,
        title: metadata.title,
        author: metadata.author,
        author_id: metadata.author_id,
        category: metadata.category,
        thumbnail_url: metadata.thumbnail_url,
        avatar_url: metadata.avatar_url,
        description: metadata.description,
        tags: metadata.tags,
        is_live: metadata.is_live,
        duration_seconds: metadata.duration_seconds,
        view_count: metadata.view_count,
        like_count: metadata.like_count,
        comment_count: metadata.comment_count,
        published_at: metadata.published_at,
        started_at: metadata.started_at,
        has_danmaku: metadata.danmaku_resource_id.is_some(),
        has_live_danmaku,
    }
}

const fn resource_kind(value: AcFunResourceKind) -> i32 {
    match value {
        AcFunResourceKind::Video => proto::ResourceKind::Video as i32,
        AcFunResourceKind::Bangumi => proto::ResourceKind::Bangumi as i32,
        AcFunResourceKind::Live => proto::ResourceKind::Live as i32,
    }
}

const fn stream_format(value: AcFunStreamFormat) -> i32 {
    match value {
        AcFunStreamFormat::Hls => proto::StreamFormat::Hls as i32,
        AcFunStreamFormat::Flv => proto::StreamFormat::Flv as i32,
    }
}
