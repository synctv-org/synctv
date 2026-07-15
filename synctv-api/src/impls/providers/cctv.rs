use synctv_media_providers::cctv::{CctvMedia, CctvMetadata, CctvStreamKind};
use synctv_proto::providers::cctv::{self as proto, *};
use synctv_proto::source_config;

pub(crate) fn resolve_response(media: CctvMedia, resource: String) -> ResolveResponse {
    ResolveResponse {
        metadata: Some(metadata_message(media.metadata)),
        streams: media
            .playback
            .streams
            .into_iter()
            .map(|stream| Stream {
                name: stream.name,
                url: stream.url,
                kind: stream_kind(stream.kind),
            })
            .collect(),
        source_config: Some(source_config::CctvMediaSourceConfig { resource }),
    }
}

fn metadata_message(metadata: CctvMetadata) -> Metadata {
    Metadata {
        video_id: metadata.video_id,
        title: metadata.title,
        description: metadata.description,
        uploader: metadata.uploader,
        producer: metadata.producer,
        channel: metadata.channel,
        column: metadata.column,
        tags: metadata.tags,
        thumbnail_url: metadata.thumbnail_url,
        duration_seconds: metadata.duration_seconds,
        published_at: metadata.published_at,
        chapters: metadata
            .chapters
            .into_iter()
            .map(|chapter| Chapter {
                id: chapter.id,
                title: chapter.title,
                start_ms: chapter.start_ms,
                end_ms: chapter.end_ms,
            })
            .collect(),
        protected: metadata.protected,
    }
}

const fn stream_kind(value: CctvStreamKind) -> i32 {
    match value {
        CctvStreamKind::VideoHls => proto::StreamKind::VideoHls as i32,
        CctvStreamKind::AudioHls => proto::StreamKind::AudioHls as i32,
        CctvStreamKind::Http => proto::StreamKind::Http as i32,
    }
}
