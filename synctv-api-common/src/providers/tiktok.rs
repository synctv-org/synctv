use std::sync::Arc;

use synctv_core::models::UserId;
use synctv_core::provider::{ProviderError, TikTokProvider};
use synctv_media_providers::tiktok::{
    TikTokAuthor, TikTokImage, TikTokListItem, TikTokMedia, TikTokMediaKind, TikTokStreamFormat,
};
use synctv_proto::providers::tiktok::{
    self as proto, BindInfo, BindRequest, BindResponse, GetBindsResponse, GetUserRequest,
    GetUserResponse, ListUserPostsRequest, ListUserPostsResponse, ResolveRequest, ResolveResponse,
    UnbindRequest, UnbindResponse,
};
use synctv_realtime::fanout::RealtimeEventService;

use super::{
    discovery::{discovered_media, discovered_playlist},
    provider_instance_name_for_response, publish_provider_credential_changed,
};

#[derive(Clone)]
pub struct TikTokApiImpl {
    provider: Arc<TikTokProvider>,
    event_service: Arc<dyn RealtimeEventService>,
}

impl TikTokApiImpl {
    #[must_use]
    pub fn new(
        provider: Arc<TikTokProvider>,
        event_service: Arc<dyn RealtimeEventService>,
    ) -> Self {
        Self {
            provider,
            event_service,
        }
    }

    pub async fn bind(
        &self,
        user_id: UserId,
        req: BindRequest,
        instance_name: Option<&str>,
    ) -> Result<BindResponse, ProviderError> {
        let server_id = self
            .provider
            .persist_session(
                user_id,
                req.label,
                req.cookie,
                instance_name.map(ToString::to_string),
            )
            .await?;
        publish_provider_credential_changed(
            &self.event_service,
            user_id,
            synctv_core::models::SourceProvider::TikTok,
            &server_id,
        );
        Ok(BindResponse { server_id })
    }

    pub async fn get_binds(
        &self,
        user_id: UserId,
        instance_name: Option<&str>,
    ) -> Result<GetBindsResponse, ProviderError> {
        let binds = self
            .provider
            .list_binds(user_id, instance_name)
            .await?
            .into_iter()
            .map(|bind| BindInfo {
                id: bind.id.to_string(),
                server_id: bind.server_id,
                label: bind.label,
                has_cookie: true,
                created_at: bind.created_at,
                provider_instance_name: provider_instance_name_for_response(
                    bind.provider_instance_name,
                ),
            })
            .collect();
        Ok(GetBindsResponse { binds })
    }

    pub async fn unbind(
        &self,
        user_id: UserId,
        req: UnbindRequest,
    ) -> Result<UnbindResponse, ProviderError> {
        let removed = self
            .provider
            .delete_credential(user_id, &req.server_id)
            .await?;
        if removed {
            publish_provider_credential_changed(
                &self.event_service,
                user_id,
                synctv_core::models::SourceProvider::TikTok,
                &req.server_id,
            );
        }
        Ok(UnbindResponse { removed })
    }

    pub async fn resolve(
        &self,
        user_id: UserId,
        req: ResolveRequest,
        instance_name: Option<&str>,
    ) -> Result<ResolveResponse, ProviderError> {
        let media = self
            .provider
            .resolve_for_user(user_id, &req.resource, instance_name)
            .await?;
        Ok(resolve_response(media, req.shared, instance_name))
    }

    pub async fn list_user_posts(
        &self,
        user_id: UserId,
        req: ListUserPostsRequest,
        instance_name: Option<&str>,
    ) -> Result<ListUserPostsResponse, ProviderError> {
        let page = self
            .provider
            .list_user_posts_for_user(
                user_id,
                &req.sec_uid,
                req.cursor.as_deref(),
                req.page_size.max(1),
                instance_name,
            )
            .await?;
        Ok(ListUserPostsResponse {
            items: page
                .items
                .into_iter()
                .map(|item| list_item(item, req.shared, instance_name))
                .collect(),
            cursor: page.cursor,
            has_more: page.has_more,
            source: Some(tiktok_playlist_source(
                req.sec_uid,
                req.shared,
                instance_name,
            )),
        })
    }

    pub async fn get_user(
        &self,
        user_id: UserId,
        req: GetUserRequest,
        instance_name: Option<&str>,
    ) -> Result<GetUserResponse, ProviderError> {
        let sec_uid = self
            .provider
            .user_sec_uid_for_user(user_id, &req.resource, instance_name)
            .await?;
        Ok(GetUserResponse {
            source: Some(tiktok_playlist_source(
                sec_uid.clone(),
                req.shared,
                instance_name,
            )),
            sec_uid,
        })
    }
}

fn resolve_response(
    media: TikTokMedia,
    shared: bool,
    provider_instance_name: Option<&str>,
) -> ResolveResponse {
    let source = match &media.resource {
        synctv_media_providers::tiktok::TikTokResource::Video { video_id } => {
            synctv_proto::source_config::tik_tok_media_source_config::Source::Video(
                synctv_proto::source_config::TikTokVideoSourceConfig {
                    video_id: video_id.clone(),
                    shared,
                },
            )
        }
        synctv_media_providers::tiktok::TikTokResource::Live { unique_id } => {
            synctv_proto::source_config::tik_tok_media_source_config::Source::Live(
                synctv_proto::source_config::TikTokLiveSourceConfig {
                    unique_id: unique_id.clone(),
                    shared,
                },
            )
        }
    };
    ResolveResponse {
        metadata: Some(proto::Metadata {
            id: media.metadata.id,
            kind: match media.metadata.kind {
                TikTokMediaKind::Video => proto::MediaKind::Video as i32,
                TikTokMediaKind::Live => proto::MediaKind::Live as i32,
            },
            title: media.metadata.title,
            description: media.metadata.description,
            author: Some(author(media.metadata.author)),
            cover: media.metadata.cover.map(image),
            dynamic_cover: media.metadata.dynamic_cover.map(image),
            duration_ms: media.metadata.duration_ms,
            created_at: media.metadata.created_at,
            is_live: media.metadata.is_live,
            view_count: media.metadata.view_count,
            like_count: media.metadata.like_count,
            comment_count: media.metadata.comment_count,
            share_count: media.metadata.share_count,
            collect_count: media.metadata.collect_count,
            concurrent_viewers: media.metadata.concurrent_viewers,
            music_title: media.metadata.music_title,
            music_author: media.metadata.music_author,
            subtitles: media
                .metadata
                .subtitles
                .into_iter()
                .map(|subtitle| proto::Subtitle {
                    language: subtitle.language,
                    format: subtitle.format,
                    url: subtitle.url,
                })
                .collect(),
        }),
        room_id: media.room_id,
        variants: media
            .variants
            .into_iter()
            .map(|variant| proto::Variant {
                url: variant.url,
                format: match variant.format {
                    TikTokStreamFormat::Mp4 => proto::StreamFormat::Mp4,
                    TikTokStreamFormat::Flv => proto::StreamFormat::Flv,
                    TikTokStreamFormat::Hls => proto::StreamFormat::Hls,
                    TikTokStreamFormat::Audio => proto::StreamFormat::Audio,
                } as i32,
                quality: variant.quality,
                codec: variant.codec,
                width: variant.width,
                height: variant.height,
                bitrate: variant.bitrate,
                audio_only: variant.audio_only,
                watermarked: variant.watermarked,
                headers_required: variant.headers_required,
            })
            .collect(),
        source: Some(discovered_media(
            synctv_proto::source_config::media_source_config::Provider::Tiktok(
                synctv_proto::source_config::TikTokMediaSourceConfig {
                    source: Some(source),
                },
            ),
            provider_instance_name,
        )),
    }
}

fn list_item(
    item: TikTokListItem,
    shared: bool,
    provider_instance_name: Option<&str>,
) -> proto::ListItem {
    let source = discovered_media(
        synctv_proto::source_config::media_source_config::Provider::Tiktok(
            synctv_proto::source_config::TikTokMediaSourceConfig {
                source: Some(
                    synctv_proto::source_config::tik_tok_media_source_config::Source::Video(
                        synctv_proto::source_config::TikTokVideoSourceConfig {
                            video_id: item.video_id.clone(),
                            shared,
                        },
                    ),
                ),
            },
        ),
        provider_instance_name,
    );
    proto::ListItem {
        video_id: item.video_id,
        title: item.title,
        author: Some(author(item.author)),
        cover: item.cover.map(image),
        duration_ms: item.duration_ms,
        created_at: item.created_at,
        source: Some(source),
    }
}

fn tiktok_playlist_source(
    sec_uid: String,
    shared: bool,
    provider_instance_name: Option<&str>,
) -> synctv_proto::providers::common::DiscoveredSource {
    discovered_playlist(
        synctv_proto::source_config::playlist_source_config::Provider::Tiktok(
            synctv_proto::source_config::TikTokPlaylistSourceConfig { sec_uid, shared },
        ),
        provider_instance_name,
    )
}

fn author(author: TikTokAuthor) -> proto::Author {
    proto::Author {
        id: author.id,
        sec_uid: author.sec_uid,
        unique_id: author.unique_id,
        nickname: author.nickname,
        avatar: author.avatar.map(image),
    }
}

fn image(image: TikTokImage) -> proto::Image {
    proto::Image {
        url: image.url,
        width: image.width,
        height: image.height,
    }
}
