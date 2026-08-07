use synctv_media_providers::douyu::{DouyuCodec, DouyuMedia, DouyuMetadata, DouyuStreamFormat};
use synctv_proto::providers::douyu::{self as proto, *};
use synctv_proto::source_config as source_proto;

pub fn resolve_response(media: DouyuMedia) -> ResolveResponse {
    ResolveResponse {
        metadata: Some(metadata_message(media.metadata)),
        qualities: media
            .playback
            .qualities
            .into_iter()
            .map(|quality| Quality {
                name: quality.name,
                cdn: quality.cdn,
                cdn_name: quality.cdn_name,
                rate: quality.rate,
                bitrate: quality.bitrate,
                codec: codec(quality.codec),
                format: stream_format(quality.format),
            })
            .collect(),
        source_config: Some(source_proto::DouyuMediaSourceConfig {
            room: media.playback.room_id,
        }),
    }
}

fn metadata_message(metadata: DouyuMetadata) -> Metadata {
    Metadata {
        room_id: metadata.room_id,
        title: metadata.title,
        author: metadata.author,
        category: metadata.category,
        thumbnail_url: metadata.thumbnail_url,
        avatar_url: metadata.avatar_url,
        is_live: metadata.is_live,
        is_replay: metadata.is_replay,
        is_vip: metadata.is_vip,
        viewer_count: metadata.viewer_count,
        started_at: metadata.started_at,
    }
}

const fn codec(value: DouyuCodec) -> i32 {
    match value {
        DouyuCodec::Avc => proto::Codec::Avc as i32,
        DouyuCodec::Hevc => proto::Codec::Hevc as i32,
        DouyuCodec::Aac => proto::Codec::Aac as i32,
    }
}

const fn stream_format(value: DouyuStreamFormat) -> i32 {
    match value {
        DouyuStreamFormat::Flv => proto::StreamFormat::Flv as i32,
        DouyuStreamFormat::Hls => proto::StreamFormat::Hls as i32,
    }
}
