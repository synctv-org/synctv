//! Twitch media provider adapter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use sha2::{Digest, Sha256};

use super::{
    DynamicListQuery, DynamicListResult, DynamicPagination, DynamicPlaylistItem,
    DynamicPlaylistItemThumbnail, DynamicPlaylistProvider, ItemType, MediaProvider, NextPlayItem,
    PlaybackInfo, PlaybackResult, ProviderContext, ProviderCredentialDependency,
    ProviderCredentialPolicy, ProviderError, SourceConfig, SourceCover,
};
use crate::models::{
    MediaSourceConfig, PlayMode, PlaybackDanmaku, PlaybackDanmakuProvider, PlaybackMedia,
    PlaybackMediaMetadata, PlaybackMediaProvider, PlaybackMetadata, PlaybackTwitchDanmaku,
    PlaybackTwitchMedia, PlaylistSourceConfig, ProviderCredential, ProviderTarget,
    TwitchChapterMetadata, TwitchMediaSourceConfig, TwitchPlaybackMetadata,
    TwitchPlaybackResourceKind, TwitchPlaylistContent, TwitchPlaylistSourceConfig,
    TwitchTargetKind, UserId, UserProviderCredential,
};
use crate::repository::UserProviderCredentialRepository;
use synctv_media_providers::twitch::{
    TwitchBrowseItem, TwitchBrowseKind, TwitchClient, TwitchMetadata, TwitchPlayback,
    TwitchResource, TwitchResourceKind, TwitchSession, TwitchSessionIdentity, TwitchStreamItem,
};

const DYNAMIC_PAGE_SIZE: usize = 50;
const DYNAMIC_SHUFFLE_LIMIT: usize = 200;

pub struct TwitchProvider {
    client: TwitchClient,
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
}

#[derive(Debug, Clone)]
pub struct TwitchBind {
    pub id: i64,
    pub server_id: String,
    pub login: String,
    pub twitch_user_id: String,
    pub client_id: String,
    pub scopes: Vec<String>,
    pub created_at: i64,
    pub provider_instance_name: Option<String>,
}

pub type TwitchChatStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<TwitchChatEvent, ProviderError>> + Send + 'static>,
>;

#[derive(Debug, Clone)]
pub struct TwitchChatEvent {
    pub id: String,
    pub user_name: String,
    pub text: String,
    pub color: Option<String>,
    pub badges: Vec<String>,
    pub sent_at_ms: Option<u64>,
}

impl Default for TwitchProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl TwitchProvider {
    pub const NAME: &'static str = "twitch";

    #[must_use]
    pub fn new() -> Self {
        Self {
            client: TwitchClient::new().expect("Twitch HTTP client should build"),
            credential_repo: None,
        }
    }

    #[must_use]
    pub fn with_http_client(client: reqwest::Client) -> Self {
        Self {
            client: TwitchClient::with_http_client(client),
            credential_repo: None,
        }
    }

    #[must_use]
    pub fn with_credential_repo(
        &self,
        credential_repo: Arc<UserProviderCredentialRepository>,
    ) -> Self {
        Self {
            client: self.client.clone(),
            credential_repo: Some(credential_repo),
        }
    }

    fn credential_repo_or<'a>(
        &'a self,
        fallback: Option<&'a UserProviderCredentialRepository>,
    ) -> Option<&'a UserProviderCredentialRepository> {
        self.credential_repo.as_deref().or(fallback)
    }

    #[must_use]
    pub fn credential_server_id_for_instance(provider_instance_name: Option<&str>) -> String {
        let instance_name = crate::models::normalize_provider_instance_name(provider_instance_name)
            .unwrap_or_default();
        hex::encode(Sha256::digest(
            format!("twitch\n{instance_name}").as_bytes(),
        ))
    }

    async fn session(
        &self,
        ctx: &ProviderContext<'_>,
        credential_policy: ProviderCredentialPolicy,
    ) -> Result<TwitchSession, ProviderError> {
        let Some(repo) = self.credential_repo_or(ctx.credential_repo) else {
            return Ok(TwitchSession::default());
        };
        let owner_id = ctx.selected_credential_user_id(credential_policy);
        let Some(owner_id) = owner_id else {
            return Ok(TwitchSession::default());
        };
        let server_id =
            Self::credential_server_id_for_instance(super::bound_provider_instance_name(ctx));
        self.session_for_owner(repo, owner_id, &server_id).await
    }

    async fn session_for_owner(
        &self,
        repo: &UserProviderCredentialRepository,
        owner_id: UserId,
        server_id: &str,
    ) -> Result<TwitchSession, ProviderError> {
        let Some(credential) = repo
            .get_by_provider_and_server(owner_id, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
        else {
            return Ok(TwitchSession::default());
        };
        match credential.credential_data {
            ProviderCredential::Twitch {
                login,
                twitch_user_id,
                client_id,
                scopes,
                auth_token,
                device_id,
                client_integrity,
                ..
            } => Ok(TwitchSession {
                login: Some(login),
                user_id: Some(twitch_user_id),
                client_id: Some(client_id),
                scopes,
                auth_token: Some(auth_token),
                device_id,
                client_integrity,
            }),
            _ => Err(ProviderError::InvalidCredentialType),
        }
    }

    async fn stored_session(
        &self,
        owner_id: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<TwitchSession, ProviderError> {
        let Some(repo) = self.credential_repo.as_deref() else {
            return Ok(TwitchSession::default());
        };
        let server_id = Self::credential_server_id_for_instance(provider_instance_name);
        self.session_for_owner(repo, owner_id, &server_id).await
    }

    pub async fn persist_session(
        &self,
        user_id: UserId,
        session: TwitchSession,
        provider_instance_name: Option<String>,
    ) -> Result<(String, TwitchSessionIdentity), ProviderError> {
        let identity = self.client.validate_session(&session).await?;
        let auth_token = session.auth_token.ok_or_else(|| {
            ProviderError::InvalidConfig("Twitch auth token is required".to_string())
        })?;
        let provider_instance_name =
            crate::models::normalize_provider_instance_name_owned(provider_instance_name);
        let server_id = Self::credential_server_id_for_instance(provider_instance_name.as_deref());
        let now = Utc::now();
        let credential = UserProviderCredential {
            id: 0,
            user_id,
            provider: Self::NAME.to_string(),
            server_id: server_id.clone(),
            provider_instance_name,
            credential_data: ProviderCredential::Twitch {
                login: identity.login.clone(),
                twitch_user_id: identity.user_id.clone(),
                client_id: identity.client_id.clone(),
                scopes: identity.scopes.clone(),
                auth_token,
                device_id: session.device_id,
                client_integrity: session.client_integrity,
            },
            expires_at: identity
                .expires_in
                .and_then(|seconds| i64::try_from(seconds).ok())
                .map(|seconds| now + chrono::Duration::seconds(seconds)),
            created_at: now,
            updated_at: now,
        };
        self.credential_repo
            .as_deref()
            .ok_or_else(|| {
                ProviderError::Internal("Twitch credential repository is unavailable".to_string())
            })?
            .upsert_by_user_provider_server(&credential)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok((server_id, identity))
    }

    pub async fn list_binds(
        &self,
        user_id: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<Vec<TwitchBind>, ProviderError> {
        let requested = crate::models::normalize_provider_instance_name(provider_instance_name);
        self.credential_repo
            .as_deref()
            .ok_or_else(|| {
                ProviderError::Internal("Twitch credential repository is unavailable".to_string())
            })?
            .get_readable_by_provider(user_id, Self::NAME)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .into_iter()
            .filter(|credential| {
                requested
                    .is_none_or(|name| credential.provider_instance_name.as_deref() == Some(name))
            })
            .map(|credential| {
                let ProviderCredential::Twitch {
                    login,
                    twitch_user_id,
                    client_id,
                    scopes,
                    ..
                } = credential.credential_data
                else {
                    return Err(ProviderError::InvalidCredentialType);
                };
                Ok(TwitchBind {
                    id: credential.id,
                    server_id: credential.server_id,
                    login,
                    twitch_user_id,
                    client_id,
                    scopes,
                    created_at: credential.created_at.timestamp(),
                    provider_instance_name: credential.provider_instance_name,
                })
            })
            .collect()
    }

    pub async fn delete_credential(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<bool, ProviderError> {
        let repo = self.credential_repo.as_deref().ok_or_else(|| {
            ProviderError::Internal("Twitch credential repository is unavailable".to_string())
        })?;
        let Some(credential) = repo
            .get_by_provider_and_server(user_id, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
        else {
            return Ok(false);
        };
        repo.delete(credential.id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok(true)
    }

    pub async fn resolve_for_user(
        &self,
        user_id: UserId,
        resource: &str,
        provider_instance_name: Option<&str>,
    ) -> Result<(TwitchPlayback, TwitchMetadata), ProviderError> {
        let resource = TwitchClient::parse_resource(resource)?;
        let session = self.stored_session(user_id, provider_instance_name).await?;
        Ok(tokio::try_join!(
            self.client.playback(&resource, Some(&session)),
            self.client.metadata(&resource, Some(&session)),
        )?)
    }

    pub async fn list_channel_items_for_user(
        &self,
        user_id: UserId,
        channel: &str,
        kind: TwitchBrowseKind,
        cursor: Option<&str>,
        page_size: u32,
        provider_instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::twitch::TwitchBrowsePage, ProviderError> {
        let session = self.stored_session(user_id, provider_instance_name).await?;
        Ok(self
            .client
            .browse_channel(channel, kind, cursor, page_size, Some(&session))
            .await?)
    }

    pub async fn list_followed_live_for_user(
        &self,
        user_id: UserId,
        cursor: Option<&str>,
        page_size: u32,
        provider_instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::twitch::TwitchStreamPage, ProviderError> {
        let session = self.stored_session(user_id, provider_instance_name).await?;
        Self::require_helix_session(&session)?;
        Self::require_scope(&session, "user:read:follows")?;
        Ok(self
            .client
            .followed_live(cursor, page_size, &session)
            .await?)
    }

    pub async fn list_category_streams_for_user(
        &self,
        user_id: UserId,
        category_id: &str,
        cursor: Option<&str>,
        page_size: u32,
        provider_instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::twitch::TwitchStreamPage, ProviderError> {
        let session = self.stored_session(user_id, provider_instance_name).await?;
        Self::require_helix_session(&session)?;
        Ok(self
            .client
            .category_streams(category_id, cursor, page_size, &session)
            .await?)
    }

    pub async fn list_top_categories_for_user(
        &self,
        user_id: UserId,
        cursor: Option<&str>,
        page_size: u32,
        provider_instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::twitch::TwitchCategoryPage, ProviderError> {
        let session = self.stored_session(user_id, provider_instance_name).await?;
        Self::require_helix_session(&session)?;
        Ok(self
            .client
            .top_categories(cursor, page_size, &session)
            .await?)
    }

    pub async fn search_live_channels_for_user(
        &self,
        user_id: UserId,
        query: &str,
        cursor: Option<&str>,
        page_size: u32,
        provider_instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::twitch::TwitchChannelSearchPage, ProviderError> {
        let session = self.stored_session(user_id, provider_instance_name).await?;
        Self::require_helix_session(&session)?;
        Ok(self
            .client
            .search_live_channels(query, cursor, page_size, &session)
            .await?)
    }

    pub async fn schedule_for_user(
        &self,
        user_id: UserId,
        broadcaster_id: &str,
        cursor: Option<&str>,
        page_size: u32,
        provider_instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::twitch::TwitchSchedulePage, ProviderError> {
        let session = self.stored_session(user_id, provider_instance_name).await?;
        Self::require_helix_session(&session)?;
        Ok(self
            .client
            .schedule(broadcaster_id, cursor, page_size, &session)
            .await?)
    }

    pub async fn get_resource(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        version: &str,
        mode_name: &str,
        media_index: usize,
        request_context: Option<&super::ExecutionControl>,
        range_header: Option<&str>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let media = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.medias.get(media_index))
            .ok_or(ProviderError::NotFound)?;
        let PlaybackMediaProvider::Twitch(PlaybackTwitchMedia::Refresh {
            resource_kind,
            resource_id,
            quality_name,
            credential_owner_id,
            provider_instance_name,
        }) = &media.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "Twitch cached playback resource is invalid".to_string(),
            ));
        };
        let server_id = Self::credential_server_id_for_instance(provider_instance_name.as_deref());
        let session = match credential_owner_id {
            Some(owner_id) => {
                let repo = self.credential_repo.as_deref().ok_or_else(|| {
                    ProviderError::Internal(
                        "Twitch credential repository is unavailable".to_string(),
                    )
                })?;
                self.session_for_owner(repo, *owner_id, &server_id).await?
            }
            None => TwitchSession::default(),
        };
        let resource = TwitchResource {
            kind: match resource_kind {
                TwitchPlaybackResourceKind::Channel => TwitchResourceKind::Channel,
                TwitchPlaybackResourceKind::Video => TwitchResourceKind::Video,
                TwitchPlaybackResourceKind::Clip => TwitchResourceKind::Clip,
            },
            id: resource_id.clone(),
        };
        let playback = self.client.playback(&resource, Some(&session)).await?;
        let quality = playback
            .qualities
            .into_iter()
            .find(|quality| quality.name == *quality_name)
            .ok_or(ProviderError::NotFound)?;
        super::playback_transport::transport_action_for_target_url(
            quality.url,
            HashMap::new(),
            range_header,
        )
    }

    pub fn get_segment(
        &self,
        target_url: String,
        range_header: Option<&str>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        super::playback_transport::transport_action_for_target_url(
            target_url,
            HashMap::new(),
            range_header,
        )
    }

    pub async fn watch_chat(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        version: &str,
        mode_name: &str,
        media_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<TwitchChatStream, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let media = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.medias.get(media_index))
            .ok_or(ProviderError::NotFound)?;
        let PlaybackMediaProvider::Twitch(PlaybackTwitchMedia::Refresh {
            resource_kind: TwitchPlaybackResourceKind::Channel,
            resource_id,
            credential_owner_id,
            provider_instance_name,
            ..
        }) = &media.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "Twitch chat requires a live channel resource".to_string(),
            ));
        };
        let server_id = Self::credential_server_id_for_instance(provider_instance_name.as_deref());
        let session = match credential_owner_id {
            Some(owner_id) => {
                let repo = self.credential_repo.as_deref().ok_or_else(|| {
                    ProviderError::Internal(
                        "Twitch credential repository is unavailable".to_string(),
                    )
                })?;
                self.session_for_owner(repo, *owner_id, &server_id).await?
            }
            None => TwitchSession::default(),
        };
        let stream =
            synctv_media_providers::twitch::watch_chat(resource_id, Some(&session)).await?;
        Ok(Box::pin(stream.map(|event| {
            event
                .map(|event| TwitchChatEvent {
                    id: event.id,
                    user_name: event.user_name,
                    text: event.text,
                    color: event.color,
                    badges: event.badges,
                    sent_at_ms: event.sent_at_ms,
                })
                .map_err(ProviderError::from)
        })))
    }

    fn resource(config: &TwitchMediaSourceConfig) -> Result<TwitchResource, ProviderError> {
        let (kind, id) = match config {
            TwitchMediaSourceConfig::Live { channel, .. } => (TwitchResourceKind::Channel, channel),
            TwitchMediaSourceConfig::Video { video_id, .. } => {
                (TwitchResourceKind::Video, video_id)
            }
            TwitchMediaSourceConfig::Clip { slug, .. } => (TwitchResourceKind::Clip, slug),
        };
        if id.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Twitch resource id is required".to_string(),
            ));
        }
        Ok(TwitchResource {
            kind,
            id: id.trim().to_string(),
        })
    }

    fn twitch_config(
        source_config: &MediaSourceConfig,
    ) -> Result<&TwitchMediaSourceConfig, ProviderError> {
        match source_config {
            MediaSourceConfig::Twitch(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Twitch provider requires Twitch media source_config".to_string(),
            )),
        }
    }

    const fn media_credential_policy(config: &TwitchMediaSourceConfig) -> ProviderCredentialPolicy {
        ProviderCredentialPolicy::from_shared(match config {
            TwitchMediaSourceConfig::Live { shared, .. }
            | TwitchMediaSourceConfig::Video { shared, .. }
            | TwitchMediaSourceConfig::Clip { shared, .. } => *shared,
        })
    }

    fn playlist_config(
        source_config: &PlaylistSourceConfig,
    ) -> Result<&TwitchPlaylistSourceConfig, ProviderError> {
        match source_config {
            PlaylistSourceConfig::Twitch(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Twitch provider requires Twitch playlist source_config".to_string(),
            )),
        }
    }

    fn browse_kind(content: TwitchPlaylistContent) -> TwitchBrowseKind {
        match content {
            TwitchPlaylistContent::Videos => TwitchBrowseKind::Videos,
            TwitchPlaylistContent::Highlights => TwitchBrowseKind::Highlights,
            TwitchPlaylistContent::Uploads => TwitchBrowseKind::Uploads,
            TwitchPlaylistContent::Clips => TwitchBrowseKind::Clips,
        }
    }

    fn require_helix_session(session: &TwitchSession) -> Result<(), ProviderError> {
        if session.auth_token.as_deref().is_none_or(str::is_empty)
            || session.client_id.as_deref().is_none_or(str::is_empty)
        {
            return Err(ProviderError::CredentialRequired);
        }
        Ok(())
    }

    fn require_scope(session: &TwitchSession, scope: &str) -> Result<(), ProviderError> {
        if session.scopes.iter().any(|value| value == scope) {
            Ok(())
        } else {
            Err(ProviderError::Authentication(format!(
                "Twitch OAuth scope {scope} is required"
            )))
        }
    }

    fn require_playlist_session(
        config: &TwitchPlaylistSourceConfig,
        session: &TwitchSession,
    ) -> Result<(), ProviderError> {
        match config {
            TwitchPlaylistSourceConfig::Channel { .. } => Ok(()),
            TwitchPlaylistSourceConfig::FollowedLive { .. } => {
                Self::require_helix_session(session)?;
                Self::require_scope(session, "user:read:follows")
            }
            TwitchPlaylistSourceConfig::CategoryLive { .. }
            | TwitchPlaylistSourceConfig::SearchLive { .. } => Self::require_helix_session(session),
        }
    }

    const fn playlist_credential_policy(
        config: &TwitchPlaylistSourceConfig,
    ) -> ProviderCredentialPolicy {
        ProviderCredentialPolicy::from_shared(match config {
            TwitchPlaylistSourceConfig::Channel { shared, .. }
            | TwitchPlaylistSourceConfig::FollowedLive { shared }
            | TwitchPlaylistSourceConfig::CategoryLive { shared, .. }
            | TwitchPlaylistSourceConfig::SearchLive { shared, .. } => *shared,
        })
    }

    fn encode_target(resource: &TwitchResource) -> Result<ProviderTarget, ProviderError> {
        if resource.id.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Twitch target id is required".to_string(),
            ));
        }
        let kind = match resource.kind {
            TwitchResourceKind::Video => TwitchTargetKind::Video,
            TwitchResourceKind::Clip => TwitchTargetKind::Clip,
            TwitchResourceKind::Channel => TwitchTargetKind::Live,
        };
        Ok(ProviderTarget::twitch(kind, resource.id.clone()))
    }

    fn decode_target(target: &ProviderTarget) -> Result<TwitchResource, ProviderError> {
        let ProviderTarget::Twitch(target) = target else {
            return Err(ProviderError::InvalidConfig(
                "Twitch playlist requires a Twitch target".to_string(),
            ));
        };
        if target.id.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Twitch target id is required".to_string(),
            ));
        }
        Ok(TwitchResource {
            kind: match target.kind {
                TwitchTargetKind::Video => TwitchResourceKind::Video,
                TwitchTargetKind::Clip => TwitchResourceKind::Clip,
                TwitchTargetKind::Live => TwitchResourceKind::Channel,
            },
            id: target.id.clone(),
        })
    }

    fn directory_item(item: TwitchBrowseItem) -> Result<DynamicPlaylistItem, ProviderError> {
        let resource = item.resource.clone();
        let metadata = PlaybackMetadata::Twitch(TwitchPlaybackMetadata {
            resource_id: resource.id.clone(),
            title: item.title.clone(),
            author: String::new(),
            category: None,
            thumbnail_url: item.thumbnail_url.clone(),
            description: None,
            view_count: item.view_count,
            published_at: item.published_at.clone(),
            chapters: Vec::new(),
            storyboard_url: None,
            is_live: false,
            is_currently_live: None,
        });
        let description = item.view_count.map(|views| format!("{views} views"));
        Ok(DynamicPlaylistItem {
            name: item.title,
            item_type: ItemType::Media,
            target: Self::encode_target(&item.resource)?,
            size: None,
            thumbnail: item.thumbnail_url.map(DynamicPlaylistItemThumbnail::Url),
            description,
            modified_at: item
                .published_at
                .as_deref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.timestamp()),
            source_config: None,
            metadata: Some(metadata),
        })
    }

    fn stream_directory_item(item: TwitchStreamItem) -> Result<DynamicPlaylistItem, ProviderError> {
        let channel = item.channel.clone();
        let metadata = PlaybackMetadata::Twitch(TwitchPlaybackMetadata {
            resource_id: channel.clone(),
            title: item.title.clone(),
            author: item.display_name.clone(),
            category: (!item.category_name.is_empty()).then_some(item.category_name.clone()),
            thumbnail_url: (!item.thumbnail_url.is_empty()).then_some(item.thumbnail_url.clone()),
            description: None,
            view_count: Some(item.viewer_count),
            published_at: Some(item.started_at.clone()),
            chapters: Vec::new(),
            storyboard_url: None,
            is_live: true,
            is_currently_live: Some(true),
        });
        let category = (!item.category_name.is_empty()).then_some(item.category_name);
        let description = match category {
            Some(category) => Some(format!("{category} · {} viewers", item.viewer_count)),
            None => Some(format!("{} viewers", item.viewer_count)),
        };
        Ok(DynamicPlaylistItem {
            name: item.title,
            item_type: ItemType::Media,
            target: Self::encode_target(&TwitchResource {
                kind: TwitchResourceKind::Channel,
                id: item.channel,
            })?,
            size: None,
            thumbnail: (!item.thumbnail_url.is_empty())
                .then_some(DynamicPlaylistItemThumbnail::Url(item.thumbnail_url)),
            description,
            modified_at: chrono::DateTime::parse_from_rfc3339(&item.started_at)
                .ok()
                .map(|value| value.timestamp()),
            source_config: None,
            metadata: Some(metadata),
        })
    }

    fn next_item(
        base: &TwitchPlaylistSourceConfig,
        item: &DynamicPlaylistItem,
    ) -> Result<NextPlayItem, ProviderError> {
        let resource = Self::decode_target(&item.target)?;
        let source_config = match resource.kind {
            TwitchResourceKind::Video => TwitchMediaSourceConfig::Video {
                video_id: resource.id,
                shared: Self::playlist_credential_policy(base).uses_resource_owner(),
            },
            TwitchResourceKind::Clip => TwitchMediaSourceConfig::Clip {
                slug: resource.id,
                shared: Self::playlist_credential_policy(base).uses_resource_owner(),
            },
            TwitchResourceKind::Channel => TwitchMediaSourceConfig::Live {
                channel: resource.id,
                shared: Self::playlist_credential_policy(base).uses_resource_owner(),
            },
        };
        Ok(NextPlayItem {
            name: item.name.clone(),
            item_type: ItemType::Media,
            source_config: MediaSourceConfig::Twitch(source_config),
            target: item.target.clone(),
        })
    }

    fn scan_ordered_page(
        items: Vec<DynamicPlaylistItem>,
        target: &ProviderTarget,
        first: &mut Option<DynamicPlaylistItem>,
        found_current: &mut bool,
    ) -> Option<DynamicPlaylistItem> {
        for item in items {
            first.get_or_insert_with(|| item.clone());
            if *found_current {
                return Some(item);
            }
            *found_current = item.target == *target;
        }
        None
    }

    fn metadata_model(metadata: TwitchMetadata, is_live_resource: bool) -> TwitchPlaybackMetadata {
        TwitchPlaybackMetadata {
            resource_id: metadata.id,
            title: metadata.title,
            author: metadata.author,
            category: metadata.game,
            thumbnail_url: metadata.thumbnail_url,
            description: metadata.description,
            view_count: metadata.view_count,
            published_at: metadata.published_at,
            chapters: metadata
                .chapters
                .into_iter()
                .map(|chapter| TwitchChapterMetadata {
                    title: chapter.title,
                    start_seconds: chapter.start_seconds,
                    end_seconds: chapter.end_seconds,
                })
                .collect(),
            storyboard_url: metadata.storyboard_url,
            is_live: is_live_resource,
            is_currently_live: is_live_resource.then_some(metadata.is_live),
        }
    }

    fn playback_result(
        playback: TwitchPlayback,
        metadata: TwitchMetadata,
        credential_owner_id: Option<UserId>,
        provider_instance_name: Option<&str>,
    ) -> Result<PlaybackResult, ProviderError> {
        let mut infos = HashMap::new();
        for quality in playback.qualities {
            let format = if quality.url.contains(".m3u8") {
                "m3u8"
            } else {
                "mp4"
            };
            let mode = if format == "m3u8" { "hls" } else { format };
            let info = infos
                .entry(mode.to_string())
                .or_insert_with(|| PlaybackInfo {
                    thumbnail: metadata.thumbnail_url.clone(),
                    medias: Vec::new(),
                    default_media_index: None,
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: (playback.resource.kind == TwitchResourceKind::Channel)
                        .then(|| PlaybackDanmaku {
                            name: "Twitch Chat".to_string(),
                            format: Some("synctv-twitch-live".to_string()),
                            p2p_swarm_id: None,
                            provider: PlaybackDanmakuProvider::Twitch(
                                PlaybackTwitchDanmaku::Refresh { media_index: 0 },
                            ),
                        })
                        .into_iter()
                        .collect(),
                    default_danmaku_index: (playback.resource.kind == TwitchResourceKind::Channel)
                        .then_some(0),
                });
            let media_index = info.medias.len();
            let is_source = quality.name.eq_ignore_ascii_case("chunked")
                || quality.name.to_ascii_lowercase().contains("source");
            let resource_kind = match playback.resource.kind {
                TwitchResourceKind::Channel => TwitchPlaybackResourceKind::Channel,
                TwitchResourceKind::Video => TwitchPlaybackResourceKind::Video,
                TwitchResourceKind::Clip => TwitchPlaybackResourceKind::Clip,
            };
            let p2p_swarm_id = (resource_kind != TwitchPlaybackResourceKind::Channel).then(|| {
                super::provider_p2p_swarm_id(
                    Self::NAME,
                    provider_instance_name,
                    "media",
                    &format!(
                        "{}:{}:quality:{}",
                        twitch_resource_kind_name(resource_kind),
                        playback.resource.id,
                        quality.name
                    ),
                )
            });
            info.medias.push(PlaybackMedia {
                name: quality.name.clone(),
                format: format.to_string(),
                expire_at: None,
                metadata: Some(PlaybackMediaMetadata {
                    resolution: quality
                        .width
                        .zip(quality.height)
                        .map(|(width, height)| format!("{width}x{height}")),
                    bitrate: quality
                        .bandwidth
                        .and_then(|value| i64::try_from(value).ok()),
                    codec: quality.codecs,
                    fps: quality
                        .frame_rate
                        .as_deref()
                        .and_then(|value| value.parse::<f64>().ok())
                        .and_then(rounded_i32),
                }),
                p2p_swarm_id,
                provider: PlaybackMediaProvider::Twitch(PlaybackTwitchMedia::Refresh {
                    resource_kind,
                    resource_id: playback.resource.id.clone(),
                    quality_name: quality.name.clone(),
                    credential_owner_id,
                    provider_instance_name: provider_instance_name.map(str::to_owned),
                }),
            });
            if is_source || info.default_media_index.is_none() {
                info.default_media_index = Some(media_index);
            }
        }
        if infos.is_empty() {
            return Err(ProviderError::ApiError(
                "Twitch returned no playable qualities".to_string(),
            ));
        }
        let default_mode = infos
            .contains_key("hls")
            .then(|| "hls".to_string())
            .or_else(|| infos.keys().min().cloned())
            .ok_or_else(|| ProviderError::ApiError("Twitch playback is empty".to_string()))?;
        let duration_seconds = metadata
            .duration_seconds
            .map(|value| std::time::Duration::from_secs(value).as_secs_f64());
        let is_live_resource = playback.resource.kind == TwitchResourceKind::Channel;
        Ok(PlaybackResult {
            playback_infos: infos,
            default_mode,
            provider: crate::models::SourceProvider::Twitch,
            provider_instance_name: None,
            duration_seconds,
            playback_kind: Some(if metadata.is_live {
                crate::models::PlaybackKind::Live
            } else {
                crate::models::PlaybackKind::Regular
            }),
            metadata: Some(PlaybackMetadata::Twitch(Self::metadata_model(
                metadata,
                is_live_resource,
            ))),
        })
    }
}

const fn twitch_resource_kind_name(kind: TwitchPlaybackResourceKind) -> &'static str {
    match kind {
        TwitchPlaybackResourceKind::Channel => "channel",
        TwitchPlaybackResourceKind::Video => "video",
        TwitchPlaybackResourceKind::Clip => "clip",
    }
}

fn mark_twitch_playback_resources(
    result: &mut PlaybackResult,
    version: &str,
    expires_at: i64,
    client_profile: Option<&super::PlaybackClientProfile>,
) {
    let original_default = result.default_mode.clone();
    let original_modes = std::mem::take(&mut result.playback_infos);
    for (mode_name, mut info) in original_modes {
        let source_medias = std::mem::take(&mut info.medias);
        let supported_indices = source_medias
            .iter()
            .enumerate()
            .filter_map(|(media_index, media)| {
                super::proxy_playback_media_supported_by_client(client_profile, &mode_name, media)
                    .then_some(media_index)
            })
            .collect::<std::collections::HashSet<_>>();
        let (medias, default_media_index) = super::map_playback_resources(
            &source_medias,
            info.default_media_index,
            |media_index, media| {
                if !supported_indices.contains(&media_index) {
                    return None;
                }
                let mut media = media.clone();
                if matches!(
                    media.provider,
                    PlaybackMediaProvider::Twitch(PlaybackTwitchMedia::Refresh { .. })
                ) {
                    media.provider = PlaybackMediaProvider::Twitch(PlaybackTwitchMedia::Proxy {
                        version: version.to_string(),
                        expires_at,
                        mode_name: mode_name.clone(),
                        media_index,
                    });
                }
                Some(media)
            },
        );
        if medias.is_empty() {
            continue;
        }
        info.medias = medias;
        info.default_media_index = default_media_index;
        let (danmakus, default_danmaku_index) = super::map_playback_resources(
            &info.danmakus,
            info.default_danmaku_index,
            |_, danmaku| {
                (!matches!(
                    &danmaku.provider,
                    PlaybackDanmakuProvider::Twitch(PlaybackTwitchDanmaku::Refresh { media_index })
                        if !supported_indices.contains(media_index)
                ))
                .then(|| danmaku.clone())
            },
        );
        info.danmakus = danmakus;
        info.default_danmaku_index = default_danmaku_index;
        for danmaku in &mut info.danmakus {
            let PlaybackDanmakuProvider::Twitch(PlaybackTwitchDanmaku::Refresh { media_index }) =
                &danmaku.provider
            else {
                continue;
            };
            danmaku.provider = PlaybackDanmakuProvider::Twitch(PlaybackTwitchDanmaku::Proxy {
                version: version.to_string(),
                expires_at,
                mode_name: mode_name.clone(),
                media_index: *media_index,
            });
        }
        result.playback_infos.insert(mode_name, info);
    }
    super::select_generated_playback_default(result, &original_default, true);
}

#[allow(clippy::cast_possible_truncation)]
fn rounded_i32(value: f64) -> Option<i32> {
    let rounded = value.round();
    (rounded.is_finite() && rounded >= f64::from(i32::MIN) && rounded <= f64::from(i32::MAX))
        .then_some(rounded as i32)
}

#[async_trait]
impl MediaProvider for TwitchProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn as_dynamic_playlist_provider(&self) -> Option<&dyn DynamicPlaylistProvider> {
        Some(self)
    }

    async fn media_metadata(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<Option<PlaybackMetadata>, ProviderError> {
        ctx.check_active()
            .map_err(|error| ProviderError::NetworkError(error.to_string()))?;
        let config = Self::twitch_config(source_config)?;
        let resource = Self::resource(config)?;
        let cache_key = serde_json::to_string(source_config)
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        let credential_policy = Self::media_credential_policy(config);
        super::cached_provider_metadata_or_fill(
            Self::NAME,
            &cache_key,
            if resource.kind == TwitchResourceKind::Channel {
                Duration::from_secs(15)
            } else {
                Duration::from_secs(300)
            },
            ctx,
            || async {
                let session = self.session(ctx, credential_policy).await?;
                let metadata = self.client.metadata(&resource, Some(&session)).await?;
                Ok(Some(PlaybackMetadata::Twitch(Self::metadata_model(
                    metadata,
                    resource.kind == TwitchResourceKind::Channel,
                ))))
            },
        )
        .await
    }

    async fn generate_playback(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        ctx.check_active()
            .map_err(|error| ProviderError::NetworkError(error.to_string()))?;
        let config = Self::twitch_config(source_config)?;
        let resource = Self::resource(config)?;
        let credential_policy = Self::media_credential_policy(config);
        let credential_owner_id = ctx.selected_credential_user_id(credential_policy);
        if credential_policy.uses_resource_owner() && credential_owner_id.is_none() {
            return Err(ProviderError::Internal(
                "Twitch credential owner is unavailable".to_string(),
            ));
        }
        let session = self.session(ctx, credential_policy).await?;
        let provider_instance_name =
            super::bound_provider_instance_name(ctx).map(ToString::to_string);
        let credential_server_id =
            Self::credential_server_id_for_instance(provider_instance_name.as_deref());
        let cache_key = format!(
            "playback:{}:{}:{}:{}",
            match resource.kind {
                TwitchResourceKind::Channel => "channel",
                TwitchResourceKind::Video => "video",
                TwitchResourceKind::Clip => "clip",
            },
            resource.id,
            credential_owner_id.map_or_else(|| "anonymous".to_string(), |id| id.to_string()),
            credential_server_id,
        );
        let client_profile = ctx.playback_client_profile();
        let result = Box::pin(super::cached_versioned_playback_or_fill(
            Self::NAME,
            &cache_key,
            Duration::from_hours(6),
            ctx,
            |result, version, expires_at| {
                mark_twitch_playback_resources(result, version, expires_at, client_profile);
            },
            || async {
                let (playback, metadata) = tokio::try_join!(
                    self.client.playback(&resource, Some(&session)),
                    self.client.metadata(&resource, Some(&session)),
                )?;
                Self::playback_result(
                    playback,
                    metadata,
                    credential_owner_id,
                    provider_instance_name.as_deref(),
                )
            },
        ))
        .await?;
        super::require_compatible_playback_route(
            result,
            crate::models::PlaybackProxyMode::Only,
            client_profile,
        )
    }

    async fn validate_source_config(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        ctx.check_active()
            .map_err(|error| ProviderError::NetworkError(error.to_string()))?;
        match source_config {
            SourceConfig::Media(source_config) => {
                let config = Self::twitch_config(source_config)?;
                let resource = Self::resource(config)?;
                let session = self
                    .session(ctx, Self::media_credential_policy(config))
                    .await?;
                if resource.kind == TwitchResourceKind::Channel {
                    self.client.metadata(&resource, Some(&session)).await?;
                } else {
                    self.client.playback(&resource, Some(&session)).await?;
                }
            }
            SourceConfig::DynamicPlaylist(source_config) => {
                let config = Self::playlist_config(source_config)?;
                let session = self
                    .session(ctx, Self::playlist_credential_policy(config))
                    .await?;
                Self::require_playlist_session(config, &session)?;
                match config {
                    TwitchPlaylistSourceConfig::Channel {
                        channel, content, ..
                    } => {
                        self.client
                            .browse_channel(
                                channel,
                                Self::browse_kind(*content),
                                None,
                                1,
                                Some(&session),
                            )
                            .await?;
                    }
                    TwitchPlaylistSourceConfig::FollowedLive { .. } => {
                        self.client.followed_live(None, 1, &session).await?;
                    }
                    TwitchPlaylistSourceConfig::CategoryLive { category_id, .. } => {
                        self.client
                            .category_streams(category_id, None, 1, &session)
                            .await?;
                    }
                    TwitchPlaylistSourceConfig::SearchLive { query, .. } => {
                        self.client
                            .search_live_channels(query, None, 1, &session)
                            .await?;
                    }
                }
            }
        }
        Ok(())
    }

    fn credential_dependencies(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Vec<ProviderCredentialDependency>, ProviderError> {
        let (credential_policy, credential_required) = match source_config {
            SourceConfig::Media(config) => {
                let policy = Self::media_credential_policy(Self::twitch_config(config)?);
                (policy, policy.uses_resource_owner())
            }
            SourceConfig::DynamicPlaylist(config) => {
                let config = Self::playlist_config(config)?;
                (
                    Self::playlist_credential_policy(config),
                    !matches!(config, TwitchPlaylistSourceConfig::Channel { .. }),
                )
            }
        };
        let Some(owner_id) = ctx.selected_credential_user_id(credential_policy) else {
            return Ok(Vec::new());
        };
        let server_id =
            Self::credential_server_id_for_instance(super::bound_provider_instance_name(ctx));
        Ok(vec![
            if credential_policy.uses_resource_owner() || credential_required {
                ProviderCredentialDependency::new(
                    crate::models::SourceProvider::Twitch,
                    owner_id,
                    server_id,
                )
            } else {
                ProviderCredentialDependency::optional(
                    crate::models::SourceProvider::Twitch,
                    owner_id,
                    server_id,
                )
            },
        ])
    }

    async fn source_cover(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Option<SourceCover>, ProviderError> {
        ctx.check_active()
            .map_err(|error| ProviderError::NetworkError(error.to_string()))?;
        let (resource, credential_policy) = match source_config {
            SourceConfig::Media(config) => {
                let config = Self::twitch_config(config)?;
                (
                    Some(Self::resource(config)?),
                    Self::media_credential_policy(config),
                )
            }
            SourceConfig::DynamicPlaylist(config) => {
                let config = Self::playlist_config(config)?;
                let resource = match config {
                    TwitchPlaylistSourceConfig::Channel { channel, .. } => Some(TwitchResource {
                        kind: TwitchResourceKind::Channel,
                        id: channel.clone(),
                    }),
                    _ => None,
                };
                (resource, Self::playlist_credential_policy(config))
            }
        };
        let session = self.session(ctx, credential_policy).await?;
        if let SourceConfig::DynamicPlaylist(config) = source_config {
            Self::require_playlist_session(Self::playlist_config(config)?, &session)?;
        }
        if let Some(resource) = resource {
            return Ok(self
                .client
                .metadata(&resource, Some(&session))
                .await?
                .thumbnail_url
                .map(|url| SourceCover::Url { url }));
        }
        let config = match source_config {
            SourceConfig::DynamicPlaylist(config) => Self::playlist_config(config)?,
            SourceConfig::Media(_) => unreachable!(),
        };
        let cover = match config {
            TwitchPlaylistSourceConfig::FollowedLive { .. } => self
                .client
                .followed_live(None, 1, &session)
                .await?
                .items
                .into_iter()
                .next()
                .map(|item| item.thumbnail_url),
            TwitchPlaylistSourceConfig::CategoryLive { category_id, .. } => self
                .client
                .category_streams(category_id, None, 1, &session)
                .await?
                .items
                .into_iter()
                .next()
                .map(|item| item.thumbnail_url),
            TwitchPlaylistSourceConfig::SearchLive { query, .. } => self
                .client
                .search_live_channels(query, None, 1, &session)
                .await?
                .items
                .into_iter()
                .next()
                .map(|item| item.thumbnail_url),
            TwitchPlaylistSourceConfig::Channel { .. } => None,
        };
        Ok(cover.map(|url| SourceCover::Url { url }))
    }
}

#[async_trait]
impl DynamicPlaylistProvider for TwitchProvider {
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: Option<&ProviderTarget>,
        query: DynamicListQuery,
    ) -> Result<DynamicListResult, ProviderError> {
        ctx.check_active()
            .map_err(|error| ProviderError::NetworkError(error.to_string()))?;
        if target.is_some() {
            return Err(ProviderError::InvalidConfig(
                "Twitch channel playlists have a single browse level".to_string(),
            ));
        }
        if query
            .search
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Err(ProviderError::InvalidConfig(
                "Twitch channel playlists do not expose server-side search".to_string(),
            ));
        }
        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let config = Self::playlist_config(config)?;
        let session = self
            .session(ctx, Self::playlist_credential_policy(config))
            .await?;
        Self::require_playlist_session(config, &session)?;
        let cursor = match &query.pagination {
            DynamicPagination::Cursor { cursor } => cursor.as_deref(),
            DynamicPagination::Page { page: 1 } => None,
            DynamicPagination::Page { .. } => {
                return Err(ProviderError::InvalidConfig(
                    "Twitch requires cursor pagination after the first page".to_string(),
                ));
            }
        };
        let page_size = u32::try_from(query.page_size.max(1)).map_err(|_| {
            ProviderError::InvalidConfig("Twitch page size exceeds u32::MAX".to_string())
        })?;
        let (items, next_cursor) = match config {
            TwitchPlaylistSourceConfig::Channel {
                channel, content, ..
            } => {
                let page = self
                    .client
                    .browse_channel(
                        channel,
                        Self::browse_kind(*content),
                        cursor,
                        page_size,
                        Some(&session),
                    )
                    .await?;
                (
                    page.items
                        .into_iter()
                        .map(Self::directory_item)
                        .collect::<Result<Vec<_>, _>>()?,
                    page.next_cursor,
                )
            }
            TwitchPlaylistSourceConfig::FollowedLive { .. } => {
                let page = self
                    .client
                    .followed_live(cursor, page_size, &session)
                    .await?;
                (
                    page.items
                        .into_iter()
                        .map(Self::stream_directory_item)
                        .collect::<Result<Vec<_>, _>>()?,
                    page.next_cursor,
                )
            }
            TwitchPlaylistSourceConfig::CategoryLive { category_id, .. } => {
                let page = self
                    .client
                    .category_streams(category_id, cursor, page_size, &session)
                    .await?;
                (
                    page.items
                        .into_iter()
                        .map(Self::stream_directory_item)
                        .collect::<Result<Vec<_>, _>>()?,
                    page.next_cursor,
                )
            }
            TwitchPlaylistSourceConfig::SearchLive { query, .. } => {
                let page = self
                    .client
                    .search_live_channels(query, cursor, page_size, &session)
                    .await?;
                let items = page
                    .items
                    .into_iter()
                    .filter(|item| item.is_live)
                    .map(|item| {
                        Self::stream_directory_item(TwitchStreamItem {
                            stream_id: String::new(),
                            user_id: item.user_id,
                            channel: item.channel,
                            display_name: item.display_name,
                            title: item.title,
                            category_id: item.category_id,
                            category_name: item.category_name,
                            thumbnail_url: item.thumbnail_url,
                            viewer_count: 0,
                            started_at: item.started_at,
                            language: item.language,
                            tags: item.tags,
                            is_mature: false,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                (items, page.next_cursor)
            }
        };
        let has_more = next_cursor.is_some();
        Ok(DynamicListResult {
            items,
            pagination: DynamicPagination::Cursor {
                cursor: next_cursor,
            },
            has_more,
            supports_search: false,
        })
    }

    async fn resolve_item(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &ProviderTarget,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        Self::decode_target(target)?;
        let base =
            Self::playlist_config(playlist.source_config.as_ref().ok_or_else(|| {
                ProviderError::InvalidConfig("Missing source_config".to_string())
            })?)?;
        let mut cursor = None;
        loop {
            let result = self
                .list_playlist(
                    ctx,
                    playlist,
                    None,
                    DynamicListQuery {
                        pagination: DynamicPagination::Cursor { cursor },
                        page_size: DYNAMIC_PAGE_SIZE,
                        ..DynamicListQuery::default()
                    },
                )
                .await?;
            if let Some(item) = result.items.iter().find(|item| &item.target == target) {
                return Self::next_item(base, item).map(Some);
            }
            match result.pagination {
                DynamicPagination::Cursor { cursor: Some(next) } if result.has_more => {
                    cursor = Some(next);
                }
                _ => return Ok(None),
            }
        }
    }

    async fn next(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &ProviderTarget,
        play_mode: PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        if play_mode == PlayMode::RepeatOne {
            return Ok(None);
        }
        Self::decode_target(target)?;
        let base =
            Self::playlist_config(playlist.source_config.as_ref().ok_or_else(|| {
                ProviderError::InvalidConfig("Missing source_config".to_string())
            })?)?;
        let mut cursor = None;
        let mut first = None;
        let mut found_current = false;
        let mut shuffle = Vec::new();
        loop {
            let result = self
                .list_playlist(
                    ctx,
                    playlist,
                    None,
                    DynamicListQuery {
                        pagination: DynamicPagination::Cursor { cursor },
                        page_size: DYNAMIC_PAGE_SIZE,
                        ..DynamicListQuery::default()
                    },
                )
                .await?;
            let next_cursor = match &result.pagination {
                DynamicPagination::Cursor { cursor } if result.has_more => cursor.clone(),
                _ => None,
            };
            if play_mode == PlayMode::Shuffle {
                for item in result.items {
                    first.get_or_insert_with(|| item.clone());
                    if item.target != *target && shuffle.len() < DYNAMIC_SHUFFLE_LIMIT {
                        shuffle.push(item);
                    }
                }
            } else if let Some(item) =
                Self::scan_ordered_page(result.items, target, &mut first, &mut found_current)
            {
                return Self::next_item(base, &item).map(Some);
            }
            let Some(next) = next_cursor else {
                break;
            };
            if play_mode == PlayMode::Shuffle && shuffle.len() >= DYNAMIC_SHUFFLE_LIMIT {
                break;
            }
            cursor = Some(next);
        }
        let selected = match play_mode {
            PlayMode::RepeatAll if found_current => first.as_ref(),
            PlayMode::Shuffle => shuffle
                .get(rand::random_range(0..shuffle.len().max(1)))
                .or(first.as_ref()),
            PlayMode::Sequential | PlayMode::RepeatAll | PlayMode::RepeatOne => None,
        };
        selected.map(|item| Self::next_item(base, item)).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_media_providers::twitch::{TwitchQuality, TwitchResource};

    #[test]
    fn provider_exposes_dynamic_playlist_capability() {
        crate::install_process_crypto_provider();
        let provider = TwitchProvider::new();

        assert!(provider.as_dynamic_playlist_provider().is_some());
    }

    fn item(kind: TwitchTargetKind, id: &str) -> DynamicPlaylistItem {
        DynamicPlaylistItem {
            name: id.to_string(),
            item_type: ItemType::Media,
            target: ProviderTarget::twitch(kind, id.to_string()),
            size: None,
            thumbnail: None,
            description: None,
            modified_at: None,
            source_config: None,
            metadata: None,
        }
    }

    #[test]
    fn sequential_scan_advances_across_cursor_page_boundary() {
        let target = ProviderTarget::twitch(TwitchTargetKind::Video, "2".to_string());
        let mut first = None;
        let mut found_current = false;

        assert!(TwitchProvider::scan_ordered_page(
            vec![
                item(TwitchTargetKind::Video, "1"),
                item(TwitchTargetKind::Video, "2"),
            ],
            &target,
            &mut first,
            &mut found_current,
        )
        .is_none());
        assert!(found_current);

        let next = TwitchProvider::scan_ordered_page(
            vec![
                item(TwitchTargetKind::Video, "3"),
                item(TwitchTargetKind::Video, "4"),
            ],
            &target,
            &mut first,
            &mut found_current,
        )
        .expect("the first item on the next cursor page should be selected");
        assert_eq!(
            next.target,
            ProviderTarget::twitch(TwitchTargetKind::Video, "3".to_string())
        );
    }

    #[test]
    fn repeat_all_scan_preserves_first_item_for_cursor_wrap() {
        let target = ProviderTarget::twitch(TwitchTargetKind::Clip, "4".to_string());
        let mut first = None;
        let mut found_current = false;

        assert!(TwitchProvider::scan_ordered_page(
            vec![
                item(TwitchTargetKind::Clip, "1"),
                item(TwitchTargetKind::Clip, "2"),
            ],
            &target,
            &mut first,
            &mut found_current,
        )
        .is_none());
        assert!(TwitchProvider::scan_ordered_page(
            vec![
                item(TwitchTargetKind::Clip, "3"),
                item(TwitchTargetKind::Clip, "4"),
            ],
            &target,
            &mut first,
            &mut found_current,
        )
        .is_none());
        assert!(found_current);
        assert_eq!(
            first
                .expect("repeat-all should retain the first item")
                .target,
            ProviderTarget::twitch(TwitchTargetKind::Clip, "1".to_string())
        );
    }

    #[test]
    fn playlist_target_preserves_twitch_resource_kind() {
        let video = TwitchResource {
            kind: TwitchResourceKind::Video,
            id: "1234".to_string(),
        };
        let clip = TwitchResource {
            kind: TwitchResourceKind::Clip,
            id: "1234".to_string(),
        };

        let video_target =
            TwitchProvider::encode_target(&video).expect("test operation should succeed");
        let clip_target =
            TwitchProvider::encode_target(&clip).expect("test operation should succeed");
        assert_ne!(video_target, clip_target);
        assert_eq!(
            TwitchProvider::decode_target(&video_target).expect("test operation should succeed"),
            video
        );
        assert_eq!(
            TwitchProvider::decode_target(&clip_target).expect("test operation should succeed"),
            clip
        );
    }

    #[test]
    fn playlist_item_builds_provider_specific_media_source() {
        let base = TwitchPlaylistSourceConfig::Channel {
            channel: "synctv".to_string(),
            content: TwitchPlaylistContent::Clips,
            shared: true,
        };
        let item = DynamicPlaylistItem {
            name: "A clip".to_string(),
            item_type: ItemType::Media,
            target: ProviderTarget::twitch(TwitchTargetKind::Clip, "ClipSlug".to_string()),
            size: None,
            thumbnail: None,
            description: None,
            modified_at: None,
            source_config: None,
            metadata: None,
        };

        let next = TwitchProvider::next_item(&base, &item).expect("test operation should succeed");
        assert_eq!(next.target, item.target);
        assert!(matches!(
            next.source_config,
            MediaSourceConfig::Twitch(TwitchMediaSourceConfig::Clip {
                slug,
                shared: true,
            }) if slug == "ClipSlug"
        ));
    }

    #[test]
    fn live_playlist_item_builds_live_media_source() {
        let base = TwitchPlaylistSourceConfig::CategoryLive {
            category_id: "game-1".to_string(),
            category_name: "Development".to_string(),
            shared: true,
        };
        let item = DynamicPlaylistItem {
            name: "Live channel".to_string(),
            item_type: ItemType::Media,
            target: ProviderTarget::twitch(TwitchTargetKind::Live, "synctv".to_string()),
            size: None,
            thumbnail: None,
            description: None,
            modified_at: None,
            source_config: None,
            metadata: None,
        };

        let next = TwitchProvider::next_item(&base, &item).expect("test operation should succeed");
        assert!(matches!(
            next.source_config,
            MediaSourceConfig::Twitch(TwitchMediaSourceConfig::Live {
                channel,
                shared: true,
            }) if channel == "synctv"
        ));
    }

    #[test]
    fn playback_groups_twitch_qualities_into_one_hls_mode() {
        let mut result = TwitchProvider::playback_result(
            TwitchPlayback {
                resource: TwitchResource {
                    kind: TwitchResourceKind::Channel,
                    id: "synctv".to_string(),
                },
                master_url: Some("https://usher.example.test/master.m3u8".to_string()),
                qualities: vec![
                    TwitchQuality {
                        name: "chunked".to_string(),
                        url: "https://video.example.test/source.m3u8".to_string(),
                        bandwidth: Some(6_000_000),
                        width: Some(1920),
                        height: Some(1080),
                        frame_rate: Some("60".to_string()),
                        codecs: Some("avc1.64002A".to_string()),
                    },
                    TwitchQuality {
                        name: "720p60".to_string(),
                        url: "https://video.example.test/720.m3u8".to_string(),
                        bandwidth: Some(3_000_000),
                        width: Some(1280),
                        height: Some(720),
                        frame_rate: Some("60".to_string()),
                        codecs: None,
                    },
                ],
                token: None,
            },
            TwitchMetadata {
                id: "synctv".to_string(),
                title: "SyncTV Live".to_string(),
                author: "SyncTV".to_string(),
                game: Some("Software and Game Development".to_string()),
                thumbnail_url: Some("https://image.example.test/live.jpg".to_string()),
                is_live: true,
                description: None,
                duration_seconds: None,
                view_count: Some(42),
                published_at: None,
                chapters: Vec::new(),
                storyboard_url: None,
            },
            Some(UserId::expect_positive(1)),
            None,
        )
        .expect("playback should map");
        assert_eq!(result.default_mode, "hls");
        assert_eq!(result.playback_infos.len(), 1);
        assert_eq!(
            result.playback_kind,
            Some(crate::models::PlaybackKind::Live)
        );
        let source = result
            .playback_infos
            .get("hls")
            .expect("test operation should succeed");
        assert_eq!(source.medias.len(), 2);
        assert_eq!(source.default_media_index, Some(0));
        assert!(source
            .medias
            .iter()
            .all(|media| media.p2p_swarm_id.is_none()));
        assert!(source.danmakus[0].p2p_swarm_id.is_none());
        assert!(matches!(
            source.medias[0].provider,
            PlaybackMediaProvider::Twitch(PlaybackTwitchMedia::Refresh { .. })
        ));
        assert!(matches!(
            source.danmakus[0].provider,
            PlaybackDanmakuProvider::Twitch(PlaybackTwitchDanmaku::Refresh { .. })
        ));

        mark_twitch_playback_resources(&mut result, "version-1", 12345, None);
        let source = result
            .playback_infos
            .get("hls")
            .expect("test operation should succeed");
        assert!(source
            .medias
            .iter()
            .all(|media| media.p2p_swarm_id.is_none()));
        assert!(source.danmakus[0].p2p_swarm_id.is_none());
        assert!(matches!(
            source.medias[0].provider,
            PlaybackMediaProvider::Twitch(PlaybackTwitchMedia::Proxy {
                ref version,
                expires_at: 12345,
                ..
            }) if version == "version-1"
        ));
        assert!(matches!(
            source.danmakus[0].provider,
            PlaybackDanmakuProvider::Twitch(PlaybackTwitchDanmaku::Proxy { .. })
        ));
    }
}
