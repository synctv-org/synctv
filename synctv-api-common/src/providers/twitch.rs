use std::sync::Arc;

use synctv_core::models::UserId;
use synctv_core::provider::{ProviderError, TwitchProvider};
use synctv_media_providers::twitch::{
    TwitchBrowseItem, TwitchBrowseKind, TwitchCategory, TwitchChannelSearchItem, TwitchMetadata,
    TwitchPlayback, TwitchResourceKind, TwitchScheduleSegment as NativeScheduleSegment,
    TwitchSession, TwitchStreamItem,
};
use synctv_proto::providers::twitch::{self as proto, *};
use synctv_proto::source_config::{
    self as source_proto, twitch_media_source_config, TwitchPlaylistContent,
};
use synctv_realtime::fanout::RealtimeEventService;

use super::{provider_instance_name_for_response, publish_provider_credential_changed};

#[derive(Clone)]
pub struct TwitchApiImpl {
    provider: Arc<TwitchProvider>,
    event_service: Arc<dyn RealtimeEventService>,
}

impl TwitchApiImpl {
    #[must_use]
    pub fn new(
        provider: Arc<TwitchProvider>,
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
        let (server_id, identity) = self
            .provider
            .persist_session(
                user_id,
                TwitchSession {
                    login: None,
                    user_id: None,
                    client_id: None,
                    scopes: Vec::new(),
                    auth_token: Some(req.auth_token),
                    device_id: req.device_id,
                    client_integrity: req.client_integrity,
                },
                instance_name.map(ToString::to_string),
            )
            .await?;
        publish_provider_credential_changed(
            &self.event_service,
            user_id,
            TwitchProvider::NAME,
            &server_id,
        );
        Ok(BindResponse {
            server_id,
            login: identity.login,
            twitch_user_id: identity.user_id,
            client_id: identity.client_id,
            scopes: identity.scopes,
            expires_in: identity.expires_in,
        })
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
                login: bind.login,
                twitch_user_id: bind.twitch_user_id,
                client_id: bind.client_id,
                scopes: bind.scopes,
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
                TwitchProvider::NAME,
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
        let (playback, metadata) = self
            .provider
            .resolve_for_user(user_id, &req.resource, instance_name)
            .await?;
        Ok(resolve_response(playback, metadata))
    }

    pub async fn list_channel_items(
        &self,
        user_id: UserId,
        req: ListChannelItemsRequest,
        instance_name: Option<&str>,
    ) -> Result<ListChannelItemsResponse, ProviderError> {
        let content = TwitchPlaylistContent::try_from(req.content)
            .unwrap_or(TwitchPlaylistContent::Unspecified);
        let kind = match content {
            TwitchPlaylistContent::Videos => TwitchBrowseKind::Videos,
            TwitchPlaylistContent::Highlights => TwitchBrowseKind::Highlights,
            TwitchPlaylistContent::Uploads => TwitchBrowseKind::Uploads,
            TwitchPlaylistContent::Clips => TwitchBrowseKind::Clips,
            TwitchPlaylistContent::Unspecified => {
                return Err(ProviderError::InvalidConfig(
                    "Twitch playlist content is required".to_string(),
                ));
            }
        };
        let page = self
            .provider
            .list_channel_items_for_user(
                user_id,
                &req.channel,
                kind,
                req.cursor.as_deref(),
                req.page_size.max(1),
                instance_name,
            )
            .await?;
        let has_more = page.next_cursor.is_some();
        Ok(ListChannelItemsResponse {
            items: page.items.into_iter().map(list_item).collect(),
            cursor: page.next_cursor,
            has_more,
            source_config: Some(source_proto::TwitchPlaylistSourceConfig {
                shared: false,
                source: Some(
                    source_proto::twitch_playlist_source_config::Source::Channel(
                        source_proto::twitch_playlist_source_config::Channel {
                            channel: req.channel,
                            content: req.content,
                        },
                    ),
                ),
            }),
        })
    }

    pub async fn list_followed_live(
        &self,
        user_id: UserId,
        req: ListFollowedLiveRequest,
        instance_name: Option<&str>,
    ) -> Result<ListFollowedLiveResponse, ProviderError> {
        let page = self
            .provider
            .list_followed_live_for_user(
                user_id,
                req.cursor.as_deref(),
                req.page_size.max(1),
                instance_name,
            )
            .await?;
        let has_more = page.next_cursor.is_some();
        Ok(ListFollowedLiveResponse {
            items: page.items.into_iter().map(stream_item).collect(),
            cursor: page.next_cursor,
            has_more,
            source_config: Some(twitch_playlist_source(
                false,
                source_proto::twitch_playlist_source_config::Source::FollowedLive(
                    source_proto::twitch_playlist_source_config::FollowedLive {},
                ),
            )),
        })
    }

    pub async fn list_category_streams(
        &self,
        user_id: UserId,
        req: ListCategoryStreamsRequest,
        instance_name: Option<&str>,
    ) -> Result<ListCategoryStreamsResponse, ProviderError> {
        let page = self
            .provider
            .list_category_streams_for_user(
                user_id,
                &req.category_id,
                req.cursor.as_deref(),
                req.page_size.max(1),
                instance_name,
            )
            .await?;
        let has_more = page.next_cursor.is_some();
        Ok(ListCategoryStreamsResponse {
            items: page.items.into_iter().map(stream_item).collect(),
            cursor: page.next_cursor,
            has_more,
            source_config: Some(twitch_playlist_source(
                false,
                source_proto::twitch_playlist_source_config::Source::CategoryLive(
                    source_proto::twitch_playlist_source_config::CategoryLive {
                        category_id: req.category_id,
                        category_name: req.category_name,
                    },
                ),
            )),
        })
    }

    pub async fn list_top_categories(
        &self,
        user_id: UserId,
        req: ListTopCategoriesRequest,
        instance_name: Option<&str>,
    ) -> Result<ListTopCategoriesResponse, ProviderError> {
        let page = self
            .provider
            .list_top_categories_for_user(
                user_id,
                req.cursor.as_deref(),
                req.page_size.max(1),
                instance_name,
            )
            .await?;
        let has_more = page.next_cursor.is_some();
        Ok(ListTopCategoriesResponse {
            items: page.items.into_iter().map(category_item).collect(),
            cursor: page.next_cursor,
            has_more,
        })
    }

    pub async fn search_live_channels(
        &self,
        user_id: UserId,
        req: SearchLiveChannelsRequest,
        instance_name: Option<&str>,
    ) -> Result<SearchLiveChannelsResponse, ProviderError> {
        let page = self
            .provider
            .search_live_channels_for_user(
                user_id,
                &req.query,
                req.cursor.as_deref(),
                req.page_size.max(1),
                instance_name,
            )
            .await?;
        let has_more = page.next_cursor.is_some();
        Ok(SearchLiveChannelsResponse {
            items: page.items.into_iter().map(search_channel_item).collect(),
            cursor: page.next_cursor,
            has_more,
            source_config: Some(twitch_playlist_source(
                false,
                source_proto::twitch_playlist_source_config::Source::SearchLive(
                    source_proto::twitch_playlist_source_config::SearchLive { query: req.query },
                ),
            )),
        })
    }

    pub async fn list_schedule(
        &self,
        user_id: UserId,
        req: ListScheduleRequest,
        instance_name: Option<&str>,
    ) -> Result<ListScheduleResponse, ProviderError> {
        let page = self
            .provider
            .schedule_for_user(
                user_id,
                &req.broadcaster_id,
                req.cursor.as_deref(),
                req.page_size.max(1),
                instance_name,
            )
            .await?;
        let has_more = page.next_cursor.is_some();
        let source_config = twitch_live_source(page.broadcaster_login.clone());
        Ok(ListScheduleResponse {
            broadcaster_id: page.broadcaster_id,
            broadcaster_login: page.broadcaster_login,
            broadcaster_name: page.broadcaster_name,
            segments: page.segments.into_iter().map(schedule_segment).collect(),
            cursor: page.next_cursor,
            has_more,
            source_config: Some(source_config),
        })
    }
}

fn twitch_playlist_source(
    shared: bool,
    source: source_proto::twitch_playlist_source_config::Source,
) -> source_proto::TwitchPlaylistSourceConfig {
    source_proto::TwitchPlaylistSourceConfig {
        shared,
        source: Some(source),
    }
}

fn twitch_live_source(channel: String) -> source_proto::TwitchMediaSourceConfig {
    source_proto::TwitchMediaSourceConfig {
        source: Some(twitch_media_source_config::Source::Live(
            source_proto::TwitchLiveSourceConfig {
                channel,
                shared: false,
            },
        )),
    }
}

fn stream_item(item: TwitchStreamItem) -> StreamItem {
    StreamItem {
        source_config: Some(twitch_live_source(item.channel.clone())),
        stream_id: item.stream_id,
        user_id: item.user_id,
        channel: item.channel,
        display_name: item.display_name,
        title: item.title,
        category_id: item.category_id,
        category_name: item.category_name,
        thumbnail_url: item.thumbnail_url,
        viewer_count: item.viewer_count,
        started_at: item.started_at,
        language: item.language,
        tags: item.tags,
        is_mature: item.is_mature,
    }
}

fn category_item(item: TwitchCategory) -> CategoryItem {
    CategoryItem {
        source_config: Some(twitch_playlist_source(
            false,
            source_proto::twitch_playlist_source_config::Source::CategoryLive(
                source_proto::twitch_playlist_source_config::CategoryLive {
                    category_id: item.id.clone(),
                    category_name: item.name.clone(),
                },
            ),
        )),
        id: item.id,
        name: item.name,
        box_art_url: item.box_art_url,
    }
}

fn search_channel_item(item: TwitchChannelSearchItem) -> SearchChannelItem {
    SearchChannelItem {
        source_config: Some(twitch_live_source(item.channel.clone())),
        user_id: item.user_id,
        channel: item.channel,
        display_name: item.display_name,
        title: item.title,
        category_id: item.category_id,
        category_name: item.category_name,
        thumbnail_url: item.thumbnail_url,
        is_live: item.is_live,
        started_at: item.started_at,
        language: item.language,
        tags: item.tags,
    }
}

fn schedule_segment(segment: NativeScheduleSegment) -> ScheduleSegment {
    ScheduleSegment {
        id: segment.id,
        start_time: segment.start_time,
        end_time: segment.end_time,
        title: segment.title,
        category_id: segment.category_id,
        category_name: segment.category_name,
        canceled_until: segment.canceled_until,
        is_recurring: segment.is_recurring,
    }
}

fn resolve_response(playback: TwitchPlayback, metadata: TwitchMetadata) -> ResolveResponse {
    let kind = resource_kind(playback.resource.kind);
    let source = match playback.resource.kind {
        TwitchResourceKind::Channel => {
            twitch_media_source_config::Source::Live(source_proto::TwitchLiveSourceConfig {
                channel: playback.resource.id.clone(),
                shared: false,
            })
        }
        TwitchResourceKind::Video => {
            twitch_media_source_config::Source::Video(source_proto::TwitchVideoSourceConfig {
                video_id: playback.resource.id.clone(),
                shared: false,
            })
        }
        TwitchResourceKind::Clip => {
            twitch_media_source_config::Source::Clip(source_proto::TwitchClipSourceConfig {
                slug: playback.resource.id.clone(),
                shared: false,
            })
        }
    };
    ResolveResponse {
        kind,
        metadata: Some(metadata_message(metadata)),
        qualities: playback
            .qualities
            .into_iter()
            .map(|quality| Quality {
                name: quality.name,
                url: quality.url,
                bandwidth: quality.bandwidth,
                width: quality.width,
                height: quality.height,
                frame_rate: quality.frame_rate,
                codecs: quality.codecs,
            })
            .collect(),
        source_config: Some(source_proto::TwitchMediaSourceConfig {
            source: Some(source),
        }),
    }
}

fn metadata_message(metadata: TwitchMetadata) -> Metadata {
    Metadata {
        id: metadata.id,
        title: metadata.title,
        author: metadata.author,
        category: metadata.game,
        thumbnail_url: metadata.thumbnail_url,
        is_live: metadata.is_live,
        description: metadata.description,
        duration_seconds: metadata.duration_seconds,
        view_count: metadata.view_count,
        published_at: metadata.published_at,
        chapters: metadata
            .chapters
            .into_iter()
            .map(|chapter| Chapter {
                title: chapter.title,
                start_seconds: chapter.start_seconds,
                end_seconds: chapter.end_seconds,
            })
            .collect(),
        storyboard_url: metadata.storyboard_url,
    }
}

fn list_item(item: TwitchBrowseItem) -> ListItem {
    ListItem {
        kind: resource_kind(item.resource.kind),
        id: item.resource.id,
        title: item.title,
        thumbnail_url: item.thumbnail_url,
        duration_seconds: item.duration_seconds,
        view_count: item.view_count,
        published_at: item.published_at,
    }
}

const fn resource_kind(kind: TwitchResourceKind) -> i32 {
    match kind {
        TwitchResourceKind::Channel => proto::ResourceKind::Channel as i32,
        TwitchResourceKind::Video => proto::ResourceKind::Video as i32,
        TwitchResourceKind::Clip => proto::ResourceKind::Clip as i32,
    }
}
