use std::sync::Arc;

use synctv_core::models::UserId;
use synctv_core::models::{YoutubeChannelContent, YoutubePlaylistSourceConfig};
use synctv_core::provider::{ProviderError, YoutubeProvider};
use synctv_media_providers::youtube::{normalize_channel_id, normalize_playlist_id};
use synctv_proto::providers::youtube::{
    list_request, BindInfo, BindRequest, BindResponse, Format, GetBindsResponse, ListItem,
    ListRequest, ListResponse, Metadata, ResolveRequest, ResolveResponse, Subtitle, UnbindRequest,
    UnbindResponse,
};
use synctv_proto::source_config as source_proto;
use synctv_realtime::fanout::RealtimeEventService;

use super::{
    discovery::{discovered_media, discovered_playlist},
    provider_instance_name_for_response, publish_provider_credential_changed,
};

#[derive(Clone)]
pub struct YoutubeApiImpl {
    provider: Arc<YoutubeProvider>,
    event_service: Arc<dyn RealtimeEventService>,
}

impl YoutubeApiImpl {
    #[must_use]
    pub fn new(
        provider: Arc<YoutubeProvider>,
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
                req.visitor_data,
                req.po_token,
                req.cookie,
                instance_name.map(ToString::to_string),
            )
            .await?;
        publish_provider_credential_changed(
            &self.event_service,
            user_id,
            YoutubeProvider::NAME,
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
                has_visitor_data: bind.has_visitor_data,
                has_po_token: bind.has_po_token,
                has_cookie: bind.has_cookie,
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
                YoutubeProvider::NAME,
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
        let player = self
            .provider
            .resolve_for_user(user_id, &req.resource, instance_name)
            .await?;
        youtube_resolve_response(&player, req.shared, instance_name)
    }

    pub async fn list(
        &self,
        user_id: UserId,
        req: ListRequest,
        instance_name: Option<&str>,
    ) -> Result<ListResponse, ProviderError> {
        let (config, source) = youtube_list_config(req.source, req.shared)?;
        let page = self
            .provider
            .list_for_user(user_id, &config, req.cursor.as_deref(), instance_name)
            .await?;
        let has_more = page.next_cursor.is_some();
        Ok(ListResponse {
            items: page
                .items
                .into_iter()
                .map(|item| youtube_list_item(item, req.shared, instance_name))
                .collect(),
            cursor: page.next_cursor,
            has_more,
            source: Some(discovered_playlist(
                source_proto::playlist_source_config::Provider::Youtube(
                    source_proto::YoutubePlaylistSourceConfig {
                        shared: req.shared,
                        source: Some(source),
                    },
                ),
                instance_name,
            )),
        })
    }
}

fn youtube_list_config(
    source: Option<list_request::Source>,
    shared: bool,
) -> Result<
    (
        YoutubePlaylistSourceConfig,
        source_proto::youtube_playlist_source_config::Source,
    ),
    ProviderError,
> {
    let source = source.ok_or_else(|| {
        ProviderError::InvalidConfig("YouTube list source is required".to_string())
    })?;
    Ok(match source {
        list_request::Source::Playlist(value) => (
            YoutubePlaylistSourceConfig::Playlist {
                playlist_id: normalize_playlist_id(&value.resource)?,
                shared,
            },
            source_proto::youtube_playlist_source_config::Source::Playlist(
                source_proto::youtube_playlist_source_config::Playlist {
                    playlist_id: normalize_playlist_id(&value.resource)?,
                },
            ),
        ),
        list_request::Source::Channel(value) => {
            let content = match source_proto::YoutubeChannelContent::try_from(value.content)
                .unwrap_or(source_proto::YoutubeChannelContent::Unspecified)
            {
                source_proto::YoutubeChannelContent::Videos => YoutubeChannelContent::Videos,
                source_proto::YoutubeChannelContent::Shorts => YoutubeChannelContent::Shorts,
                source_proto::YoutubeChannelContent::Live => YoutubeChannelContent::Live,
                source_proto::YoutubeChannelContent::Unspecified => {
                    return Err(ProviderError::InvalidConfig(
                        "YouTube channel content is required".to_string(),
                    ));
                }
            };
            (
                YoutubePlaylistSourceConfig::Channel {
                    channel_id: normalize_channel_id(&value.resource)?,
                    content,
                    shared,
                },
                source_proto::youtube_playlist_source_config::Source::Channel(
                    source_proto::youtube_playlist_source_config::Channel {
                        channel_id: normalize_channel_id(&value.resource)?,
                        content: value.content,
                    },
                ),
            )
        }
        list_request::Source::Search(value) => (
            YoutubePlaylistSourceConfig::Search {
                query: value.query.clone(),
                shared,
            },
            source_proto::youtube_playlist_source_config::Source::Search(
                source_proto::youtube_playlist_source_config::Search { query: value.query },
            ),
        ),
        list_request::Source::Subscriptions(_) => (
            YoutubePlaylistSourceConfig::Subscriptions { shared },
            source_proto::youtube_playlist_source_config::Source::Subscriptions(
                source_proto::youtube_playlist_source_config::Subscriptions {},
            ),
        ),
        list_request::Source::LikedVideos(_) => (
            YoutubePlaylistSourceConfig::LikedVideos { shared },
            source_proto::youtube_playlist_source_config::Source::LikedVideos(
                source_proto::youtube_playlist_source_config::LikedVideos {},
            ),
        ),
        list_request::Source::WatchLater(_) => (
            YoutubePlaylistSourceConfig::WatchLater { shared },
            source_proto::youtube_playlist_source_config::Source::WatchLater(
                source_proto::youtube_playlist_source_config::WatchLater {},
            ),
        ),
    })
}

fn youtube_list_item(
    item: synctv_media_providers::youtube::YoutubeListItem,
    shared: bool,
    provider_instance_name: Option<&str>,
) -> ListItem {
    ListItem {
        source: Some(discovered_media(
            source_proto::media_source_config::Provider::Youtube(
                source_proto::YoutubeMediaSourceConfig {
                    video_id: item.video_id.clone(),
                    shared,
                },
            ),
            provider_instance_name,
        )),
        video_id: item.video_id,
        title: item.title,
        channel_name: item.channel_name,
        channel_id: item.channel_id,
        duration_seconds: item.duration_seconds,
        view_count_text: item.view_count_text,
        published_time_text: item.published_time_text,
        thumbnail_url: item.thumbnail.map(|value| value.url),
        is_live: item.is_live,
        is_short: item.is_short,
    }
}

fn youtube_resolve_response(
    player: &synctv_media_providers::youtube::YoutubePlayerResponse,
    shared: bool,
    provider_instance_name: Option<&str>,
) -> Result<ResolveResponse, ProviderError> {
    let details = player.video_details.as_ref().ok_or_else(|| {
        ProviderError::ApiError("YouTube player returned no video details".to_string())
    })?;
    let streaming = player.streaming_data.as_ref().ok_or_else(|| {
        ProviderError::ApiError("YouTube player returned no streaming data".to_string())
    })?;
    let microformat = player
        .microformat
        .as_ref()
        .and_then(|value| value.player_microformat_renderer.as_ref());
    let live = microformat.and_then(|value| value.live_broadcast_details.as_ref());
    let tracklist = player
        .captions
        .as_ref()
        .and_then(|value| value.player_captions_tracklist_renderer.as_ref());

    let formats = streaming
        .formats
        .iter()
        .map(|format| youtube_format(format, false))
        .chain(
            streaming
                .adaptive_formats
                .iter()
                .map(|format| youtube_format(format, true)),
        )
        .collect();
    let subtitles = tracklist
        .into_iter()
        .flat_map(|value| &value.caption_tracks)
        .map(|track| Subtitle {
            name: track.name.value(),
            language: track.language_code.clone(),
            automatic: track.is_automatic(),
            translatable: track.is_translatable,
        })
        .collect();
    let thumbnail_url = details
        .thumbnail
        .as_ref()
        .and_then(|value| value.thumbnails.last())
        .map(|value| value.url.clone());

    Ok(ResolveResponse {
        metadata: Some(Metadata {
            video_id: details.video_id.clone(),
            title: details.title.clone(),
            channel_id: details.channel_id.clone(),
            channel_name: details.author.clone(),
            description: details.short_description.clone(),
            duration_seconds: details.length_seconds.parse().ok(),
            view_count: details.view_count.parse().ok(),
            thumbnail_url,
            keywords: details.keywords.clone(),
            is_live: details.is_live || details.is_live_content,
            is_private: details.is_private,
            publish_date: microformat.and_then(|value| value.publish_date.clone()),
            upload_date: microformat.and_then(|value| value.upload_date.clone()),
            category: microformat.and_then(|value| value.category.clone()),
            live_start: live.and_then(|value| value.start_timestamp.clone()),
            live_end: live.and_then(|value| value.end_timestamp.clone()),
        }),
        formats,
        subtitles,
        storyboard_spec: player
            .storyboards
            .as_ref()
            .and_then(|value| value.player_storyboard_spec_renderer.as_ref())
            .map(|value| value.spec.clone()),
        source: Some(discovered_media(
            synctv_proto::source_config::media_source_config::Provider::Youtube(
                synctv_proto::source_config::YoutubeMediaSourceConfig {
                    video_id: details.video_id.clone(),
                    shared,
                },
            ),
            provider_instance_name,
        )),
    })
}

fn youtube_format(
    format: &synctv_media_providers::youtube::YoutubeFormat,
    adaptive: bool,
) -> Format {
    Format {
        itag: format.itag,
        name: format.name(),
        container: format.container(),
        bitrate: format.bitrate,
        width: format.width,
        height: format.height,
        fps: format.fps,
        codecs: format.codecs(),
        adaptive,
        audio_only: format.mime_type.starts_with("audio/"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_media_providers::youtube::{
        YoutubeCaptionTrack, YoutubeCaptionTracklist, YoutubeCaptions, YoutubeFormat,
        YoutubePlayerResponse, YoutubeStreamingData, YoutubeText, YoutubeThumbnail,
        YoutubeThumbnailCollection, YoutubeVideoDetails,
    };

    #[test]
    fn resolve_response_contains_native_details_and_neutral_config() {
        let response = youtube_resolve_response(
            &YoutubePlayerResponse {
                streaming_data: Some(YoutubeStreamingData {
                    formats: vec![YoutubeFormat {
                        itag: 22,
                        mime_type: "video/mp4; codecs=\"avc1.64001F, mp4a.40.2\"".to_string(),
                        bitrate: 2_000_000,
                        width: Some(1280),
                        height: Some(720),
                        quality_label: Some("720p".to_string()),
                        ..YoutubeFormat::default()
                    }],
                    ..YoutubeStreamingData::default()
                }),
                video_details: Some(YoutubeVideoDetails {
                    video_id: "dQw4w9WgXcQ".to_string(),
                    title: "Example".to_string(),
                    length_seconds: "212".to_string(),
                    channel_id: "UC-example".to_string(),
                    author: "Creator".to_string(),
                    thumbnail: Some(YoutubeThumbnailCollection {
                        thumbnails: vec![YoutubeThumbnail {
                            url: "https://img.example/cover.jpg".to_string(),
                            width: Some(1280),
                            height: Some(720),
                        }],
                    }),
                    ..YoutubeVideoDetails::default()
                }),
                captions: Some(YoutubeCaptions {
                    player_captions_tracklist_renderer: Some(YoutubeCaptionTracklist {
                        caption_tracks: vec![YoutubeCaptionTrack {
                            name: YoutubeText {
                                simple_text: "English".to_string(),
                                ..YoutubeText::default()
                            },
                            language_code: "en".to_string(),
                            ..YoutubeCaptionTrack::default()
                        }],
                        ..YoutubeCaptionTracklist::default()
                    }),
                }),
                ..YoutubePlayerResponse::default()
            },
            true,
            Some("remote"),
        )
        .expect("YouTube response should map");

        let metadata = response.metadata.expect("metadata");
        assert_eq!(metadata.title, "Example");
        assert_eq!(metadata.duration_seconds, Some(212));
        assert_eq!(response.formats.len(), 1);
        assert_eq!(response.subtitles.len(), 1);
        let discovered = response.source.expect("discovered source");
        assert_eq!(discovered.provider_instance_name, "remote");
        let Some(synctv_proto::providers::common::discovered_source::SourceConfig::Media(config)) =
            discovered.source_config
        else {
            panic!("response should contain a media source config");
        };
        let Some(source_proto::media_source_config::Provider::Youtube(source)) = config.provider
        else {
            panic!("response should contain a YouTube source config");
        };
        assert_eq!(source.video_id, "dQw4w9WgXcQ");
        assert!(source.shared);
    }
}
