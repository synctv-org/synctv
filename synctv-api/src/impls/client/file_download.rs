use futures::StreamExt;

use crate::impls::ApiError;

// gRPC download helpers adapt the shared FileObjectDownload stream into
// protobuf chunks. Core storage owns range resolution and byte streaming; HTTP
// uses the same FileObjectDownload as a normal binary response body.

pub(crate) fn metadata_content_range(
    metadata: &synctv_core::models::FileObjectMetadata,
) -> Option<synctv_proto::client::FileByteRange> {
    metadata.range.map(super::media::file_byte_range_to_proto)
}

/// Generic helper to convert FileObjectDownload into a proto chunk stream.
/// `build_proto` receives (mime_type, sha256, data, content_range, total_size).
fn generic_chunk_stream<T, F>(
    download: synctv_core::models::FileObjectDownload,
    build_proto: F,
) -> impl futures::Stream<Item = Result<T, ApiError>> + Send + 'static
where
    T: Send + 'static,
    F: Fn(String, String, Vec<u8>, Option<synctv_proto::client::FileByteRange>, i64) -> T
        + Send
        + 'static,
{
    let metadata = download.metadata;
    let content_range = metadata_content_range(&metadata);
    let mime_type = metadata.mime_type;
    let content_manifest_sha256 = metadata.content_manifest_sha256;
    let total_size_bytes = metadata.total_size_bytes;
    download.stream.map(move |chunk| {
        chunk
            .map(|chunk| {
                build_proto(
                    mime_type.clone(),
                    content_manifest_sha256.clone(),
                    chunk.to_vec(),
                    content_range,
                    total_size_bytes,
                )
            })
            .map_err(ApiError::from)
    })
}

pub(crate) fn avatar_chunk_stream(
    download: synctv_core::models::FileObjectDownload,
) -> impl futures::Stream<Item = Result<synctv_proto::client::UserAvatarObjectResponse, ApiError>>
       + Send
       + 'static {
    generic_chunk_stream(download, |mime_type, content_manifest_sha256, data, content_range, total_size_bytes| {
        synctv_proto::client::UserAvatarObjectResponse {
            mime_type,
            content_manifest_sha256,
            data,
            content_range,
            total_size_bytes,
        }
    })
}

pub(crate) fn chat_attachment_chunk_stream(
    room_id: String,
    download: synctv_core::models::FileObjectDownload,
) -> impl futures::Stream<Item = Result<synctv_proto::client::ChatAttachmentObjectResponse, ApiError>>
       + Send
       + 'static {
    generic_chunk_stream(download, move |mime_type, content_manifest_sha256, data, content_range, total_size_bytes| {
        synctv_proto::client::ChatAttachmentObjectResponse {
            room_id: room_id.clone(),
            mime_type,
            content_manifest_sha256,
            data,
            content_range,
            total_size_bytes,
        }
    })
}

pub(crate) fn media_cover_chunk_stream(
    download: synctv_core::models::FileObjectDownload,
) -> impl futures::Stream<Item = Result<synctv_proto::client::MediaCoverObjectResponse, ApiError>>
       + Send
       + 'static {
    generic_chunk_stream(download, |mime_type, content_manifest_sha256, data, content_range, total_size_bytes| {
        synctv_proto::client::MediaCoverObjectResponse {
            mime_type,
            content_manifest_sha256,
            data,
            content_range,
            total_size_bytes,
        }
    })
}

pub(crate) fn room_cover_chunk_stream(
    download: synctv_core::models::FileObjectDownload,
) -> impl futures::Stream<Item = Result<synctv_proto::client::RoomCoverObjectResponse, ApiError>>
       + Send
       + 'static {
    generic_chunk_stream(download, |mime_type, content_manifest_sha256, data, content_range, total_size_bytes| {
        synctv_proto::client::RoomCoverObjectResponse {
            mime_type,
            content_manifest_sha256,
            data,
            content_range,
            total_size_bytes,
        }
    })
}

pub(crate) fn playlist_cover_chunk_stream(
    download: synctv_core::models::FileObjectDownload,
) -> impl futures::Stream<Item = Result<synctv_proto::client::PlaylistCoverObjectResponse, ApiError>>
       + Send
       + 'static {
    generic_chunk_stream(download, |mime_type, content_manifest_sha256, data, content_range, total_size_bytes| {
        synctv_proto::client::PlaylistCoverObjectResponse {
            mime_type,
            content_manifest_sha256,
            data,
            content_range,
            total_size_bytes,
        }
    })
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures::{StreamExt, TryStreamExt};

    #[tokio::test]
    async fn avatar_chunk_stream_preserves_stream_boundaries() {
        let download = synctv_core::models::FileObjectDownload {
            metadata: synctv_core::models::FileObjectMetadata {
                storage_backend: "database".to_string(),
                object_key: "object".to_string(),
                mime_type: "image/png".to_string(),
                size_bytes: 4,
                total_size_bytes: 10,
                content_manifest_sha256: "a".repeat(64),
                compression: synctv_core::models::FileBlobCompression::None,
                range: Some(synctv_core::models::FileByteRange {
                    start: 2,
                    end_inclusive: 5,
                }),
                metadata: serde_json::Value::Object(Default::default()),
                created_at: chrono::Utc::now(),
            },
            stream: futures::stream::iter([
                Ok::<_, synctv_core::Error>(Bytes::from_static(b"ab")),
                Ok(Bytes::from_static(b"cd")),
            ])
            .boxed(),
        };

        let chunks = super::avatar_chunk_stream(download)
            .try_collect::<Vec<_>>()
            .await
            .expect("stream should convert");

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].mime_type, "image/png");
        assert_eq!(
            chunks[0].content_range.as_ref().map(|range| range.start),
            Some(2)
        );
        assert_eq!(
            chunks[0]
                .content_range
                .as_ref()
                .map(|range| range.end_inclusive),
            Some(5)
        );
        assert_eq!(chunks[0].total_size_bytes, 10);
        assert_eq!(chunks[0].data, b"ab");
        assert_eq!(chunks[1].data, b"cd");
    }
}
