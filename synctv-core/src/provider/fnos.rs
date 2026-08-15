//! FNOS media provider adapter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use chrono::Utc;
use rand::seq::IndexedRandom;
use sha2::{Digest, Sha256};

use super::{
    DynamicBrowsePathSegment, DynamicListQuery, DynamicListResult, DynamicPagination,
    DynamicPlaylistItem, DynamicPlaylistProvider, ItemType, MediaProvider, NextPlayItem,
    PlaybackInfo, PlaybackProxyAutoPolicy, PlaybackProxyAutoReason, PlaybackProxyPolicy,
    PlaybackResult, ProviderContext, ProviderCredentialDependency, ProviderError,
    ProviderPlaybackSessionLifecycle, SourceConfig, SourceCover,
};
use crate::models::{
    detect_direct_url_format, normalize_provider_instance_name,
    normalize_provider_instance_name_owned, FnosAudioTrackMetadata, FnosFilePlaybackMetadata,
    FnosMediaPlaybackMetadata, FnosMediaSource, FnosMediaSourceConfig, FnosPlaybackMetadata,
    FnosPlaybackSession, FnosPlaylistSource, FnosPlaylistSourceConfig, FnosProxyResource,
    FnosSubtitleTrackMetadata, FnosTargetKind, FnosTranscodeResource, MediaSourceConfig, PlayMode,
    PlaybackFnosMedia, PlaybackFnosSubtitle, PlaybackMedia, PlaybackMediaProvider,
    PlaybackMetadata, PlaybackSubtitle, PlaybackSubtitleProvider, PlaylistSourceConfig,
    ProviderCredential, ProviderPlaybackSession, ProviderTarget, UserId, UserProviderCredential,
};
use crate::repository::{
    NewProviderPlaybackSession, ProviderPlaybackSessionRepository, UserProviderCredentialRepository,
};
use synctv_media_providers::fnos::{
    FnosClient, FnosCredential, FnosEndpoints, FnosFile, FnosFileList, FnosLogin, FnosMediaClient,
    FnosMediaCommandRequest, FnosMediaListRequest, FnosMediaTags, FnosPlayRecordRequest,
    FnosPlayRequest, FnosServerInfo,
};

const DYNAMIC_MAX_SHUFFLE_ITEMS: usize = 200;
fn non_empty_string(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[derive(Debug, Clone)]
pub struct FnosBind {
    pub id: i64,
    pub server_id: String,
    pub endpoint: String,
    pub webdav_endpoint: Option<String>,
    pub media_endpoint: Option<String>,
    pub media_available: bool,
    pub username: String,
    pub created_at: i64,
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Clone)]
pub enum FnosLoginResult {
    Authenticated {
        server_id: String,
        server: FnosServerInfo,
        media_available: bool,
    },
    TwoFactorRequired {
        setup_required: bool,
    },
}

pub struct FnosProvider {
    client: FnosClient,
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
}

struct FnosTranscodeActionRequest<'a> {
    store: Option<&'a Arc<dyn super::ProviderStore>>,
    session_repo: &'a ProviderPlaybackSessionRepository,
    versioned: &'a super::VersionedPlayback,
    credential_owner_id: &'a str,
    server_id: &'a str,
    spec: &'a FnosTranscodeResource,
    range_header: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FnosThumbnailCredentialKind {
    WebDav,
    Media,
}

impl FnosProvider {
    pub const NAME: &'static str = "fnos";

    #[must_use]
    pub fn new(ssrf_guard: synctv_common::ssrf::SsrfGuard) -> Self {
        Self {
            client: FnosClient::new().with_ssrf_guard(ssrf_guard),
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

    fn credential_repo(&self) -> Result<&UserProviderCredentialRepository, ProviderError> {
        self.credential_repo.as_deref().ok_or_else(|| {
            ProviderError::Internal("FNOS credential repository is unavailable".to_string())
        })
    }

    fn credential_repo_or<'a>(
        &'a self,
        fallback: Option<&'a UserProviderCredentialRepository>,
    ) -> Result<&'a UserProviderCredentialRepository, ProviderError> {
        self.credential_repo.as_deref().or(fallback).ok_or_else(|| {
            ProviderError::Internal("FNOS credential repository is unavailable".to_string())
        })
    }

    #[must_use]
    pub fn credential_server_id_for_instance(
        endpoint: &str,
        provider_instance_name: Option<&str>,
    ) -> String {
        let instance_name =
            normalize_provider_instance_name(provider_instance_name).unwrap_or_default();
        hex::encode(Sha256::digest(
            format!("{}\n{instance_name}", endpoint.trim().trim_end_matches('/')).as_bytes(),
        ))
    }

    async fn credential_with_repo(
        &self,
        repo: &UserProviderCredentialRepository,
        user_id: UserId,
        server_id: &str,
    ) -> Result<(FnosEndpoints, FnosCredential, Option<String>), ProviderError> {
        let credential = repo
            .get_by_provider_and_server(user_id, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .ok_or(ProviderError::CredentialRequired)?;
        let instance_name = credential.provider_instance_name;
        match credential.credential_data {
            ProviderCredential::Fnos {
                endpoint,
                webdav_endpoint,
                username,
                password,
                token,
                long_token,
                secret,
                ..
            } => {
                let mut endpoints = FnosEndpoints::parse(&endpoint)?;
                endpoints.webdav = webdav_endpoint;
                Ok((
                    endpoints,
                    FnosCredential {
                        username,
                        password,
                        token,
                        long_token,
                        secret,
                    },
                    instance_name,
                ))
            }
            _ => Err(ProviderError::InvalidCredentialType),
        }
    }

    async fn credential(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<(FnosEndpoints, FnosCredential, Option<String>), ProviderError> {
        self.credential_with_repo(self.credential_repo()?, user_id, server_id)
            .await
    }

    async fn media_credential_with_repo(
        &self,
        repo: &UserProviderCredentialRepository,
        user_id: UserId,
        server_id: &str,
    ) -> Result<(FnosMediaClient, String, Option<String>), ProviderError> {
        let credential = repo
            .get_by_provider_and_server(user_id, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .ok_or(ProviderError::CredentialRequired)?;
        let instance_name = credential.provider_instance_name;
        let ProviderCredential::Fnos {
            endpoint,
            media_endpoint,
            media_token,
            ..
        } = credential.credential_data
        else {
            return Err(ProviderError::InvalidCredentialType);
        };
        let token = media_token.ok_or(ProviderError::CredentialRequired)?;
        Ok((
            FnosMediaClient::new(media_endpoint.as_deref().unwrap_or(&endpoint))?,
            token,
            instance_name,
        ))
    }

    async fn media_credential(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<(FnosMediaClient, String, Option<String>), ProviderError> {
        self.media_credential_with_repo(self.credential_repo()?, user_id, server_id)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn login_and_persist(
        &self,
        user_id: UserId,
        endpoint: String,
        webdav_endpoint: Option<String>,
        media_endpoint: Option<String>,
        username: String,
        password: String,
        twofa_code: Option<String>,
        trust_device: bool,
        provider_instance_name: Option<String>,
    ) -> Result<FnosLoginResult, ProviderError> {
        let mut endpoints = FnosEndpoints::parse(&endpoint)?;
        endpoints.webdav = webdav_endpoint.clone();
        let login = self
            .client
            .login(
                &endpoints,
                &username,
                &password,
                twofa_code.as_deref(),
                trust_device,
            )
            .await?;
        let FnosLogin::Authenticated(credential) = login else {
            let FnosLogin::Challenge(challenge) = login else {
                unreachable!();
            };
            return Ok(FnosLoginResult::TwoFactorRequired {
                setup_required: challenge.setup_required,
            });
        };
        let server = self.client.server_info(&endpoints).await?;
        let media_client = FnosMediaClient::new(media_endpoint.as_deref().unwrap_or(&endpoint));
        let (media_endpoint, media_token) = match media_client {
            Ok(client) => match client.login(&username, &password).await {
                Ok(login) => (Some(client.origin().to_string()), Some(login.token)),
                Err(error) => {
                    tracing::info!(%error, "FNOS media service login unavailable; file service remains available");
                    (media_endpoint, None)
                }
            },
            Err(error) => {
                tracing::info!(%error, "FNOS media endpoint unavailable; file service remains available");
                (media_endpoint, None)
            }
        };
        let media_available = media_token.is_some();
        let provider_instance_name = normalize_provider_instance_name_owned(provider_instance_name);
        let server_id =
            Self::credential_server_id_for_instance(&endpoint, provider_instance_name.as_deref());
        let now = Utc::now();
        self.credential_repo()?
            .upsert_by_user_provider_server(&UserProviderCredential {
                id: 0,
                user_id,
                provider: Self::NAME.to_string(),
                server_id: server_id.clone(),
                provider_instance_name,
                credential_data: ProviderCredential::Fnos {
                    endpoint,
                    webdav_endpoint,
                    username: credential.username,
                    password: credential.password,
                    token: credential.token,
                    long_token: credential.long_token,
                    secret: credential.secret,
                    media_endpoint,
                    media_token,
                },
                expires_at: None,
                created_at: now,
                updated_at: now,
            })
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok(FnosLoginResult::Authenticated {
            server_id,
            server,
            media_available,
        })
    }

    pub async fn list(
        &self,
        user_id: UserId,
        server_id: &str,
        path: &str,
    ) -> Result<(FnosFileList, Option<String>), ProviderError> {
        self.list_with_repo(self.credential_repo()?, user_id, server_id, path)
            .await
    }

    async fn list_with_repo(
        &self,
        repo: &UserProviderCredentialRepository,
        user_id: UserId,
        server_id: &str,
        path: &str,
    ) -> Result<(FnosFileList, Option<String>), ProviderError> {
        validate_path(path)?;
        let (endpoints, credential, instance_name) =
            self.credential_with_repo(repo, user_id, server_id).await?;
        Ok((
            self.client
                .list(
                    &endpoints,
                    &credential,
                    (!path.trim().is_empty()).then_some(path),
                )
                .await?,
            instance_name,
        ))
    }

    pub async fn server_info(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<(FnosServerInfo, Option<String>), ProviderError> {
        let (endpoints, _, instance_name) = self.credential(user_id, server_id).await?;
        Ok((self.client.server_info(&endpoints).await?, instance_name))
    }

    pub async fn media_libraries(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<
        (
            Vec<synctv_media_providers::fnos::FnosMediaLibrary>,
            Option<String>,
        ),
        ProviderError,
    > {
        let (client, token, instance_name) = self.media_credential(user_id, server_id).await?;
        Ok((client.libraries(&token).await?, instance_name))
    }

    pub async fn media_items(
        &self,
        user_id: UserId,
        server_id: &str,
        request: &FnosMediaListRequest,
    ) -> Result<(synctv_media_providers::fnos::FnosMediaList, Option<String>), ProviderError> {
        let (client, token, instance_name) = self.media_credential(user_id, server_id).await?;
        Ok((client.items(&token, request).await?, instance_name))
    }

    pub async fn all_media_items(
        &self,
        user_id: UserId,
        server_id: &str,
        request: &FnosMediaListRequest,
    ) -> Result<
        (
            Vec<synctv_media_providers::fnos::FnosMediaItem>,
            Option<String>,
        ),
        ProviderError,
    > {
        let (client, token, instance_name) = self.media_credential(user_id, server_id).await?;
        Ok((client.all_items(&token, request).await?, instance_name))
    }

    pub async fn favorite_media_items(
        &self,
        user_id: UserId,
        server_id: &str,
        request: &FnosMediaListRequest,
    ) -> Result<(synctv_media_providers::fnos::FnosMediaList, Option<String>), ProviderError> {
        let (client, token, instance_name) = self.media_credential(user_id, server_id).await?;
        Ok((client.favorites(&token, request).await?, instance_name))
    }

    pub async fn media_history(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<
        (
            Vec<synctv_media_providers::fnos::FnosMediaItem>,
            Option<String>,
        ),
        ProviderError,
    > {
        let (client, token, instance_name) = self.media_credential(user_id, server_id).await?;
        Ok((client.history(&token).await?, instance_name))
    }

    pub async fn search_media(
        &self,
        user_id: UserId,
        server_id: &str,
        query: &str,
    ) -> Result<
        (
            Vec<synctv_media_providers::fnos::FnosMediaItem>,
            Option<String>,
        ),
        ProviderError,
    > {
        let query = query.trim();
        if query.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "FNOS media search query is required".to_string(),
            ));
        }
        let (client, token, instance_name) = self.media_credential(user_id, server_id).await?;
        Ok((client.search(&token, query).await?, instance_name))
    }

    pub async fn set_media_favorite(
        &self,
        user_id: UserId,
        server_id: &str,
        item_guid: &str,
        favorite: bool,
    ) -> Result<(bool, Option<String>), ProviderError> {
        if item_guid.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "FNOS item_guid is required".to_string(),
            ));
        }
        let (client, token, instance_name) = self.media_credential(user_id, server_id).await?;
        Ok((
            client.set_favorite(&token, item_guid, favorite).await?,
            instance_name,
        ))
    }

    pub async fn set_media_watched(
        &self,
        user_id: UserId,
        server_id: &str,
        item_guid: &str,
        watched: bool,
    ) -> Result<(bool, Option<String>), ProviderError> {
        if item_guid.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "FNOS item_guid is required".to_string(),
            ));
        }
        let (client, token, instance_name) = self.media_credential(user_id, server_id).await?;
        Ok((
            client.set_watched(&token, item_guid, watched).await?,
            instance_name,
        ))
    }

    pub async fn delete_credential(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<bool, ProviderError> {
        let Some(credential) = self
            .credential_repo()?
            .get_by_provider_and_server(user_id, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
        else {
            return Ok(false);
        };
        self.credential_repo()?
            .delete(credential.id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok(true)
    }

    pub async fn list_binds(
        &self,
        user_id: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<Vec<FnosBind>, ProviderError> {
        let requested = provider_instance_name
            .map(str::trim)
            .filter(|name| !name.is_empty());
        self.credential_repo()?
            .get_readable_by_provider(user_id, Self::NAME)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .into_iter()
            .filter(|credential| {
                requested
                    .is_none_or(|name| credential.provider_instance_name.as_deref() == Some(name))
            })
            .map(|credential| {
                let ProviderCredential::Fnos {
                    endpoint,
                    webdav_endpoint,
                    username,
                    media_endpoint,
                    media_token,
                    ..
                } = credential.credential_data
                else {
                    return Err(ProviderError::InvalidCredentialType);
                };
                Ok(FnosBind {
                    id: credential.id,
                    server_id: credential.server_id,
                    endpoint,
                    webdav_endpoint,
                    media_endpoint,
                    media_available: media_token.is_some(),
                    username,
                    created_at: credential.created_at.timestamp(),
                    provider_instance_name: credential.provider_instance_name,
                })
            })
            .collect()
    }

    pub async fn get_resource(
        &self,
        request: super::StatefulPlaybackResourceRequest<'_>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let super::StatefulPlaybackResourceRequest {
            store,
            session_repo,
            version,
            mode_name,
            media_index,
            request_context,
            range_header,
        } = request;
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let media = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.medias.get(media_index))
            .ok_or(ProviderError::NotFound)?;
        match &media.provider {
            PlaybackMediaProvider::Fnos(
                PlaybackFnosMedia::FileRefresh {
                    credential_owner_id,
                    server_id,
                    path,
                }
                | PlaybackFnosMedia::MediaOriginalRefresh {
                    credential_owner_id,
                    server_id,
                    path,
                    ..
                },
            ) => {
                self.file_resource_action(credential_owner_id, server_id, path, range_header)
                    .await
            }
            PlaybackMediaProvider::Fnos(PlaybackFnosMedia::MediaRefresh {
                credential_owner_id,
                server_id,
                media_guid,
                quality_index,
            }) => {
                self.media_resource_action(
                    credential_owner_id,
                    server_id,
                    media_guid,
                    *quality_index,
                    range_header,
                )
                .await
            }
            PlaybackMediaProvider::Fnos(PlaybackFnosMedia::TranscodeRefresh {
                credential_owner_id,
                server_id,
                spec,
            }) => {
                self.transcode_resource_action(FnosTranscodeActionRequest {
                    store,
                    session_repo,
                    versioned: &versioned,
                    credential_owner_id,
                    server_id,
                    spec,
                    range_header,
                })
                .await
            }
            PlaybackMediaProvider::Fnos(PlaybackFnosMedia::Proxy {
                credential_owner_id,
                server_id,
                resource,
                ..
            }) => match resource {
                FnosProxyResource::File { path }
                | FnosProxyResource::MediaOriginal { path, .. } => {
                    self.file_resource_action(credential_owner_id, server_id, path, range_header)
                        .await
                }
                FnosProxyResource::Media {
                    media_guid,
                    quality_index,
                } => {
                    self.media_resource_action(
                        credential_owner_id,
                        server_id,
                        media_guid,
                        *quality_index,
                        range_header,
                    )
                    .await
                }
                FnosProxyResource::Transcode { spec } => {
                    self.transcode_resource_action(FnosTranscodeActionRequest {
                        store,
                        session_repo,
                        versioned: &versioned,
                        credential_owner_id,
                        server_id,
                        spec,
                        range_header,
                    })
                    .await
                }
            },
            _ => Err(ProviderError::InvalidConfig(
                "FNOS cached playback resource is invalid".to_string(),
            )),
        }
    }

    pub async fn get_segment(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        version: &str,
        target_url: String,
        request_context: Option<&super::ExecutionControl>,
        range_header: Option<&str>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let media = versioned
            .result
            .playback_infos
            .get(&versioned.result.default_mode)
            .and_then(|info| info.medias.first())
            .ok_or(ProviderError::NotFound)?;
        let PlaybackMediaProvider::Fnos(provider) = &media.provider else {
            return Err(ProviderError::InvalidConfig(
                "FNOS cached segment resource is invalid".to_string(),
            ));
        };
        let headers = match provider {
            PlaybackFnosMedia::FileRefresh {
                credential_owner_id,
                server_id,
                ..
            } => {
                let user_id = credential_owner_id
                    .parse::<UserId>()
                    .map_err(ProviderError::InvalidConfig)?;
                let (_, credential, _) = self.credential(user_id, server_id).await?;
                webdav_headers(&credential)
            }
            PlaybackFnosMedia::MediaRefresh {
                credential_owner_id,
                server_id,
                ..
            }
            | PlaybackFnosMedia::MediaOriginalRefresh {
                credential_owner_id,
                server_id,
                ..
            }
            | PlaybackFnosMedia::TranscodeRefresh {
                credential_owner_id,
                server_id,
                ..
            } => {
                let user_id = credential_owner_id
                    .parse::<UserId>()
                    .map_err(ProviderError::InvalidConfig)?;
                let (_, token, _) = self.media_credential(user_id, server_id).await?;
                FnosMediaClient::auth_headers(&token)
            }
            PlaybackFnosMedia::Proxy {
                credential_owner_id,
                server_id,
                resource,
                ..
            } => match resource {
                FnosProxyResource::File { .. } => {
                    let user_id = credential_owner_id
                        .parse::<UserId>()
                        .map_err(ProviderError::InvalidConfig)?;
                    let (_, credential, _) = self.credential(user_id, server_id).await?;
                    webdav_headers(&credential)
                }
                FnosProxyResource::Media { .. }
                | FnosProxyResource::MediaOriginal { .. }
                | FnosProxyResource::Transcode { .. } => {
                    let user_id = credential_owner_id
                        .parse::<UserId>()
                        .map_err(ProviderError::InvalidConfig)?;
                    let (_, token, _) = self.media_credential(user_id, server_id).await?;
                    FnosMediaClient::auth_headers(&token)
                }
            },
            PlaybackFnosMedia::Direct { .. } => {
                return Err(ProviderError::InvalidConfig(
                    "FNOS direct media cannot use the provider proxy resource endpoint".to_string(),
                ));
            }
        };
        super::playback_transport::transport_action_for_target_url(
            target_url,
            headers,
            range_header,
        )
    }

    async fn file_resource_action(
        &self,
        credential_owner_id: &str,
        server_id: &str,
        path: &str,
        range_header: Option<&str>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let user_id = credential_owner_id
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let (endpoints, credential, _) = self.credential(user_id, server_id).await?;
        let webdav = self.client.webdav_config(&endpoints, &credential).await?;
        let url = FnosClient::webdav_file_url(&webdav, path)?;
        super::playback_transport::stream_action_for_target_url(
            url,
            webdav_headers(&credential),
            range_header,
        )
    }

    async fn media_resource_action(
        &self,
        credential_owner_id: &str,
        server_id: &str,
        media_guid: &str,
        quality_index: Option<usize>,
        range_header: Option<&str>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let user_id = credential_owner_id
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let (client, token, _) = self.media_credential(user_id, server_id).await?;
        super::playback_transport::transport_action_for_target_url(
            client.media_url(media_guid, quality_index),
            FnosMediaClient::auth_headers(&token),
            range_header,
        )
    }

    pub async fn image_action(
        &self,
        user_id: UserId,
        server_id: &str,
        image_path: &str,
        width: u32,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        if image_path.trim().is_empty() || image_path.split('/').any(|segment| segment == "..") {
            return Err(ProviderError::InvalidConfig(
                "FNOS image path is invalid".to_string(),
            ));
        }
        let (client, token, _) = self.media_credential(user_id, server_id).await?;
        super::playback_transport::transport_action_for_target_url(
            client.image_url(image_path, width.clamp(1, 1920)),
            FnosMediaClient::auth_headers(&token),
            None,
        )
    }

    async fn transcode_resource_action(
        &self,
        request: FnosTranscodeActionRequest<'_>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let FnosTranscodeActionRequest {
            store,
            session_repo: repo,
            versioned,
            credential_owner_id,
            server_id,
            spec,
            range_header,
        } = request;
        let store = store.ok_or_else(|| {
            ProviderError::Internal("FNOS transcode requires a provider store".to_string())
        })?;
        let user_id = credential_owner_id
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let (client, token, _) = self.media_credential(user_id, server_id).await?;
        let playback_context = versioned.playback_context.as_ref().ok_or_else(|| {
            ProviderError::InvalidConfig(
                "FNOS transcode requires a playback generation context".to_string(),
            )
        })?;
        let bitrate = i64::try_from(spec.bitrate).map_err(|_| {
            ProviderError::InvalidConfig(
                "FNOS transcode bitrate exceeds PostgreSQL BIGINT".to_string(),
            )
        })?;
        let channels = i32::try_from(spec.channels.clamp(1, 6)).map_err(|_| {
            ProviderError::InvalidConfig(
                "FNOS transcode channels exceed PostgreSQL INTEGER".to_string(),
            )
        })?;
        let lock_key = format!("lock:transcode:{}", versioned.version);
        let _guard = store
            .lock(&lock_key, Duration::from_secs(30))
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        let active_resources = repo
            .active_for_generation(
                playback_context.room_id,
                playback_context.playback_generation,
            )
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        if let Some(active_session) = active_resources.iter().find(|session| {
            matches!(
                session.session,
                ProviderPlaybackSession::Fnos(FnosPlaybackSession::Transcode { .. })
            ) && session.resource_version.as_deref() == Some(versioned.version.as_str())
        }) {
            let ProviderPlaybackSession::Fnos(FnosPlaybackSession::Transcode {
                server_id: active_server_id,
                play_link,
                media_guid,
                video_guid,
                video_encoder,
                resolution,
                bitrate: active_bitrate,
                audio_guid,
                subtitle_guid,
                channels: active_channels,
                forced_sdr,
            }) = &active_session.session
            else {
                return Err(ProviderError::Internal(
                    "FNOS transcode session changed during lookup".to_string(),
                ));
            };
            let same_spec = active_server_id == server_id
                && media_guid == &spec.media_guid
                && video_guid == &spec.video_guid
                && video_encoder == &spec.video_encoder
                && resolution == &spec.resolution
                && *active_bitrate == bitrate
                && audio_guid.as_deref() == non_empty_string(&spec.audio_guid).as_deref()
                && subtitle_guid.as_deref() == non_empty_string(&spec.subtitle_guid).as_deref()
                && *active_channels == channels
                && *forced_sdr == spec.forced_sdr;
            if same_spec {
                let status = client
                    .media_command(
                        &token,
                        &FnosMediaCommandRequest {
                            req: "media.transcodeStatis".to_string(),
                            reqid: versioned.version.clone(),
                            play_link: play_link.clone(),
                        },
                    )
                    .await;
                if status.is_ok() {
                    return super::playback_transport::transport_action_for_target_url(
                        client.resolve_media_url(play_link)?,
                        FnosMediaClient::auth_headers(&token),
                        range_header,
                    );
                }
                self.quit_transcode(&client, &token, &versioned.version, play_link)
                    .await?;
            }
            repo.delete_active(active_session.id)
                .await
                .map_err(|error| ProviderError::Internal(error.to_string()))?;
        }
        let response = client
            .play(
                &token,
                &FnosPlayRequest {
                    media_guid: spec.media_guid.clone(),
                    video_guid: spec.video_guid.clone(),
                    video_encoder: spec.video_encoder.clone(),
                    resolution: spec.resolution.clone(),
                    bitrate: spec.bitrate,
                    start_timestamp: 0,
                    audio_encoder: "aac".to_string(),
                    audio_guid: spec.audio_guid.clone(),
                    subtitle_guid: spec.subtitle_guid.clone(),
                    channels: spec.channels.clamp(1, 6),
                    forced_sdr: i32::from(spec.forced_sdr),
                },
            )
            .await?;
        if response.play_link.trim().is_empty() {
            return Err(ProviderError::ApiError(
                "FNOS transcode response has no play link".to_string(),
            ));
        }
        if let Err(error) = repo
            .upsert(NewProviderPlaybackSession {
                room_id: playback_context.room_id,
                playback_generation: playback_context.playback_generation,
                provider_instance_name: versioned.result.provider_instance_name.clone(),
                credential_owner_id: user_id,
                resource_key: format!("transcode:{}", versioned.version),
                resource_version: Some(versioned.version.clone()),
                session: ProviderPlaybackSession::Fnos(FnosPlaybackSession::Transcode {
                    server_id: server_id.to_string(),
                    play_link: response.play_link.clone(),
                    media_guid: spec.media_guid.clone(),
                    video_guid: spec.video_guid.clone(),
                    video_encoder: spec.video_encoder.clone(),
                    resolution: spec.resolution.clone(),
                    bitrate,
                    audio_guid: non_empty_string(&spec.audio_guid),
                    subtitle_guid: non_empty_string(&spec.subtitle_guid),
                    channels,
                    forced_sdr: spec.forced_sdr,
                }),
                paused: !playback_context.is_playing,
            })
            .await
        {
            let persist_error = ProviderError::Internal(error.to_string());
            return match self
                .quit_transcode(&client, &token, &versioned.version, &response.play_link)
                .await
            {
                Ok(()) => Err(persist_error),
                Err(cleanup_error) => Err(ProviderError::ApiError(format!(
                    "Failed to persist FNOS transcode session: {persist_error}; compensation={cleanup_error}"
                ))),
            };
        }
        super::playback_transport::transport_action_for_target_url(
            client.resolve_media_url(&response.play_link)?,
            FnosMediaClient::auth_headers(&token),
            range_header,
        )
    }

    pub async fn get_subtitle(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        version: &str,
        mode_name: &str,
        subtitle_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let subtitle = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.subtitles.get(subtitle_index))
            .ok_or(ProviderError::NotFound)?;
        super::playback_transport::transport_action_for_target_url(
            subtitle.upstream_url().to_string(),
            subtitle.upstream_headers(),
            None,
        )
    }

    pub async fn get_thumbnail(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        version: &str,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let info = versioned
            .result
            .playback_infos
            .get(&versioned.result.default_mode)
            .or_else(|| versioned.result.playback_infos.values().next())
            .ok_or(ProviderError::NotFound)?;
        let url = info
            .thumbnail
            .as_deref()
            .filter(|url| !url.trim().is_empty())
            .ok_or(ProviderError::NotFound)?
            .to_string();
        let provider = info
            .medias
            .first()
            .and_then(|media| match &media.provider {
                PlaybackMediaProvider::Fnos(provider) => Some(provider),
                _ => None,
            })
            .ok_or(ProviderError::NotFound)?;
        let (credential_owner_id, server_id, credential_kind) =
            fnos_thumbnail_credentials(provider)?;
        let user_id = credential_owner_id
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let headers = match credential_kind {
            FnosThumbnailCredentialKind::WebDav => {
                let (_, credential, _) = self.credential(user_id, server_id).await?;
                webdav_headers(&credential)
            }
            FnosThumbnailCredentialKind::Media => {
                let (_, token, _) = self.media_credential(user_id, server_id).await?;
                FnosMediaClient::auth_headers(&token)
            }
        };
        super::playback_transport::transport_action_for_target_url(url, headers, None)
    }

    fn media_config(config: &MediaSourceConfig) -> Result<&FnosMediaSourceConfig, ProviderError> {
        match config {
            MediaSourceConfig::Fnos(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Expected FNOS media source_config".to_string(),
            )),
        }
    }

    async fn quit_transcode(
        &self,
        client: &FnosMediaClient,
        token: &str,
        session_id: &str,
        play_link: &str,
    ) -> Result<(), ProviderError> {
        client
            .media_command(
                token,
                &FnosMediaCommandRequest {
                    req: "media.quit".to_string(),
                    reqid: session_id.to_string(),
                    play_link: play_link.to_string(),
                },
            )
            .await
            .map(|_| ())
            .map_err(ProviderError::from)
    }

    async fn report_media_progress(
        &self,
        ctx: &ProviderContext<'_>,
        session_id: &str,
        source_config: &MediaSourceConfig,
        position: f64,
    ) -> Result<(), ProviderError> {
        let config = Self::media_config(source_config)?;
        let FnosMediaSource::LibraryItem {
            item_guid,
            media_guid,
        } = &config.source
        else {
            return Ok(());
        };
        let owner = *ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        let repo = self.credential_repo_or(ctx.credential_repo)?;
        let (client, token, _) = self
            .media_credential_with_repo(repo, owner, &config.server_id)
            .await?;
        let play = client
            .play_info(&token, item_guid, media_guid.as_deref())
            .await?;
        let stream = client.stream(&token, &play.media_guid, session_id).await?;
        let active_transcode = match (ctx.db, ctx.playback_generation(), ctx.room_id().copied()) {
            (Some(db), Some(generation), Some(room_id)) => {
                ProviderPlaybackSessionRepository::new(db.clone())
                    .active_for_generation(room_id, generation)
                    .await
                    .map_err(|error| ProviderError::Internal(error.to_string()))?
                    .into_iter()
                    .find(|session| {
                        matches!(
                            &session.session,
                            ProviderPlaybackSession::Fnos(FnosPlaybackSession::Transcode {
                                server_id,
                                media_guid,
                                ..
                            }) if server_id == &config.server_id && media_guid == &play.media_guid
                        )
                    })
                    .and_then(|session| match session.session {
                        ProviderPlaybackSession::Fnos(FnosPlaybackSession::Transcode {
                            play_link,
                            video_guid,
                            resolution,
                            bitrate,
                            audio_guid,
                            subtitle_guid,
                            ..
                        }) => Some((
                            play_link,
                            video_guid,
                            resolution,
                            u64::try_from(bitrate).ok().unwrap_or(0),
                            audio_guid,
                            subtitle_guid,
                        )),
                        _ => None,
                    })
            }
            _ => None,
        };
        let video = stream.video_stream.as_ref();
        let audio = stream
            .audio_streams
            .iter()
            .find(|track| track.guid.as_deref() == play.audio_guid.as_deref())
            .or_else(|| {
                stream
                    .audio_streams
                    .iter()
                    .find(|track| track.is_default != 0)
            })
            .or_else(|| stream.audio_streams.first());
        let subtitle_guid = active_transcode
            .as_ref()
            .and_then(|active| active.5.clone())
            .or_else(|| play.subtitle_guid.clone())
            .or_else(|| {
                stream
                    .subtitle_streams
                    .iter()
                    .find(|track| track.is_default != 0)
                    .and_then(|track| track.guid.clone())
            });
        let resolution = active_transcode.as_ref().map_or_else(
            || {
                video
                    .and_then(|video| video.resolution_type.clone())
                    .filter(|value| !value.trim().is_empty())
                    .or_else(|| {
                        video.and_then(|video| {
                            (video.width > 0 && video.height > 0)
                                .then(|| format!("{}x{}", video.width, video.height))
                        })
                    })
                    .unwrap_or_else(|| "original".to_string())
            },
            |active| active.2.clone(),
        );
        let duration = video
            .map(|video| video.duration)
            .or_else(|| stream.file_stream.as_ref().map(|file| file.duration))
            .filter(|value| *value > 0)
            .unwrap_or_else(|| play.item.duration.max(play.item.runtime));
        let position = Duration::try_from_secs_f64(position.max(0.0))
            .map_or(u64::MAX, |value| value.as_secs());
        let request = FnosPlayRecordRequest {
            item_guid: play.item.guid,
            media_guid: play.media_guid,
            video_guid: play
                .video_guid
                .or_else(|| active_transcode.as_ref().map(|active| active.1.clone()))
                .or_else(|| video.and_then(|video| video.guid.clone()))
                .unwrap_or_default(),
            audio_guid: active_transcode
                .as_ref()
                .and_then(|active| active.4.clone())
                .or(play.audio_guid)
                .or_else(|| audio.and_then(|audio| audio.guid.clone()))
                .unwrap_or_default(),
            subtitle_guid,
            resolution,
            bitrate: active_transcode
                .as_ref()
                .map_or_else(|| video.map_or(0, |video| video.bps), |active| active.3),
            ts: if duration > 0 {
                position.min(duration)
            } else {
                position
            },
            duration,
            play_link: active_transcode.map(|active| active.0),
        };
        client.record_playback(&token, &request).await?;
        Ok(())
    }

    fn playlist_config(
        config: &PlaylistSourceConfig,
    ) -> Result<&FnosPlaylistSourceConfig, ProviderError> {
        match config {
            PlaylistSourceConfig::Fnos(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Expected FNOS playlist source_config".to_string(),
            )),
        }
    }

    fn source_server_id(source: SourceConfig<'_>) -> Result<&str, ProviderError> {
        match source {
            SourceConfig::Media(config) => {
                let config = Self::media_config(config)?;
                Ok(&config.server_id)
            }
            SourceConfig::DynamicPlaylist(config) => {
                let config = Self::playlist_config(config)?;
                Ok(&config.server_id)
            }
        }
    }

    fn next_item(
        base: &FnosPlaylistSourceConfig,
        item: &DynamicPlaylistItem,
    ) -> Result<NextPlayItem, ProviderError> {
        let relative = decode_file_target(Some(&item.target))?.ok_or_else(|| {
            ProviderError::InvalidConfig("FNOS item target is required".to_string())
        })?;
        Ok(NextPlayItem {
            name: item.name.clone(),
            item_type: item.item_type,
            source_config: MediaSourceConfig::Fnos(FnosMediaSourceConfig {
                server_id: base.server_id.clone(),
                proxy_mode: base.proxy_mode,
                source: FnosMediaSource::File {
                    path: join_path(file_playlist_path(base)?, &relative),
                },
            }),
            target: item.target.clone(),
        })
    }

    fn next_media_item(
        base: &FnosPlaylistSourceConfig,
        item: &DynamicPlaylistItem,
    ) -> Result<NextPlayItem, ProviderError> {
        let target = decode_media_target(Some(&item.target))?.ok_or_else(|| {
            ProviderError::InvalidConfig("FNOS media target is required".to_string())
        })?;
        Ok(NextPlayItem {
            name: item.name.clone(),
            item_type: item.item_type,
            source_config: MediaSourceConfig::Fnos(FnosMediaSourceConfig {
                server_id: base.server_id.clone(),
                proxy_mode: base.proxy_mode,
                source: FnosMediaSource::LibraryItem {
                    item_guid: target.item_guid,
                    media_guid: target.media_guid,
                },
            }),
            target: item.target.clone(),
        })
    }

    async fn scan_playlist_media(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        parent_target: Option<&ProviderTarget>,
        target: &ProviderTarget,
        page_size: usize,
        play_mode: PlayMode,
    ) -> Result<Vec<DynamicPlaylistItem>, ProviderError> {
        let mut media = Vec::new();
        let mut page = 1;
        loop {
            let result = self
                .list_playlist(
                    ctx,
                    playlist,
                    parent_target,
                    DynamicListQuery {
                        pagination: DynamicPagination::Page { page },
                        page_size,
                        ..DynamicListQuery::default()
                    },
                )
                .await?;
            media.extend(
                result
                    .items
                    .into_iter()
                    .filter(|item| item.item_type == ItemType::Media),
            );
            let found = media.iter().position(|item| &item.target == target);
            let enough = match play_mode {
                PlayMode::Sequential | PlayMode::RepeatAll => {
                    found.is_some_and(|index| media.get(index + 1).is_some())
                }
                PlayMode::Shuffle => media.len() >= DYNAMIC_MAX_SHUFFLE_ITEMS,
                PlayMode::RepeatOne => true,
            };
            if !result.has_more || enough {
                break;
            }
            page = page.saturating_add(1);
        }
        Ok(media)
    }
}

fn webdav_headers(credential: &FnosCredential) -> HashMap<String, String> {
    let value = base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{}", credential.username, credential.password));
    HashMap::from([("Authorization".to_string(), format!("Basic {value}"))])
}

fn fnos_thumbnail_credentials(
    provider: &PlaybackFnosMedia,
) -> Result<(&str, &str, FnosThumbnailCredentialKind), ProviderError> {
    match provider {
        PlaybackFnosMedia::FileRefresh {
            credential_owner_id,
            server_id,
            ..
        } => Ok((
            credential_owner_id,
            server_id,
            FnosThumbnailCredentialKind::WebDav,
        )),
        PlaybackFnosMedia::MediaRefresh {
            credential_owner_id,
            server_id,
            ..
        }
        | PlaybackFnosMedia::MediaOriginalRefresh {
            credential_owner_id,
            server_id,
            ..
        }
        | PlaybackFnosMedia::TranscodeRefresh {
            credential_owner_id,
            server_id,
            ..
        } => Ok((
            credential_owner_id,
            server_id,
            FnosThumbnailCredentialKind::Media,
        )),
        PlaybackFnosMedia::Proxy {
            credential_owner_id,
            server_id,
            resource,
            ..
        } => Ok((
            credential_owner_id,
            server_id,
            match resource {
                FnosProxyResource::File { .. } => FnosThumbnailCredentialKind::WebDav,
                FnosProxyResource::Media { .. }
                | FnosProxyResource::MediaOriginal { .. }
                | FnosProxyResource::Transcode { .. } => FnosThumbnailCredentialKind::Media,
            },
        )),
        PlaybackFnosMedia::Direct { .. } => Err(ProviderError::InvalidConfig(
            "FNOS thumbnail requires credential-backed media context".to_string(),
        )),
    }
}

fn validate_path(path: &str) -> Result<(), ProviderError> {
    if path.split('/').any(|segment| segment == "..") {
        return Err(ProviderError::InvalidConfig(
            "FNOS path must not contain traversal".to_string(),
        ));
    }
    Ok(())
}

fn validate_file_path(path: &str) -> Result<(), ProviderError> {
    validate_path(path)?;
    if path.trim_matches('/').is_empty() {
        return Err(ProviderError::InvalidConfig(
            "FNOS media path must identify a file".to_string(),
        ));
    }
    Ok(())
}

fn item_type(file: &FnosFile) -> Option<ItemType> {
    if file.is_dir {
        return Some(ItemType::Playlist);
    }
    matches!(
        file.name
            .rsplit('.')
            .next()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some(
            "mp4"
                | "mkv"
                | "avi"
                | "mov"
                | "flv"
                | "webm"
                | "m4v"
                | "wmv"
                | "m3u8"
                | "mp3"
                | "flac"
                | "wav"
                | "aac"
                | "m4a"
                | "ogg"
        )
    )
    .then_some(ItemType::Media)
}

fn encode_file_target(relative_path: &str) -> Result<ProviderTarget, ProviderError> {
    validate_path(relative_path)?;
    if relative_path.trim_matches('/').is_empty() {
        return Err(ProviderError::InvalidConfig(
            "FNOS target relative_path cannot be empty".to_string(),
        ));
    }
    Ok(ProviderTarget::fnos(relative_path.to_string()))
}

fn decode_file_target(target: Option<&ProviderTarget>) -> Result<Option<String>, ProviderError> {
    let Some(target) = target else {
        return Ok(None);
    };
    let ProviderTarget::Fnos(target) = target else {
        return Err(ProviderError::InvalidConfig(
            "FNOS target must use fnos session".to_string(),
        ));
    };
    let FnosTargetKind::File { relative_path } = &target.target else {
        return Err(ProviderError::InvalidConfig(
            "FNOS target must identify a file path".to_string(),
        ));
    };
    validate_path(relative_path)?;
    Ok(Some(relative_path.clone()))
}

struct FnosMediaTarget {
    item_guid: String,
    media_guid: Option<String>,
    library_guid: Option<String>,
}

fn decode_media_target(
    target: Option<&ProviderTarget>,
) -> Result<Option<FnosMediaTarget>, ProviderError> {
    let Some(target) = target else {
        return Ok(None);
    };
    let ProviderTarget::Fnos(target) = target else {
        return Err(ProviderError::InvalidConfig(
            "FNOS target must use fnos session".to_string(),
        ));
    };
    let FnosTargetKind::MediaItem {
        item_guid,
        media_guid,
        library_guid,
    } = &target.target
    else {
        return Err(ProviderError::InvalidConfig(
            "FNOS target must identify a media item".to_string(),
        ));
    };
    Ok(Some(FnosMediaTarget {
        item_guid: item_guid.clone(),
        media_guid: media_guid.clone(),
        library_guid: library_guid.clone(),
    }))
}

fn file_playlist_path(config: &FnosPlaylistSourceConfig) -> Result<&str, ProviderError> {
    match &config.source {
        FnosPlaylistSource::Files { path } => Ok(path),
        FnosPlaylistSource::MediaLibrary { .. }
        | FnosPlaylistSource::Favorites { .. }
        | FnosPlaylistSource::History => Err(ProviderError::InvalidConfig(
            "FNOS playlist is a media collection".to_string(),
        )),
    }
}

fn join_path(base: &str, relative: &str) -> String {
    let base = base.trim_end_matches('/');
    let relative = relative.trim_start_matches('/');
    if base.is_empty() {
        relative.to_string()
    } else {
        format!("{base}/{relative}")
    }
}

fn relative_path(base: &str, full_path: &str) -> Option<String> {
    let base = base.trim_end_matches('/');
    if base.is_empty() {
        return Some(full_path.trim_start_matches('/').to_string());
    }
    full_path
        .strip_prefix(base)?
        .strip_prefix('/')
        .map(str::to_string)
}

fn map_directory_item(
    base_path: &str,
    file: FnosFile,
) -> Result<DynamicPlaylistItem, ProviderError> {
    let item_type = item_type(&file).ok_or(ProviderError::NotFound)?;
    let relative = relative_path(base_path, &file.path).ok_or_else(|| {
        ProviderError::ApiError(format!(
            "FNOS item path '{}' is outside playlist path '{base_path}'",
            file.path
        ))
    })?;
    Ok(DynamicPlaylistItem {
        name: file.name,
        item_type,
        target: encode_file_target(&relative)?,
        size: file.size,
        thumbnail: None,
        description: None,
        modified_at: file.modified_at,
        source_config: None,
        metadata: None,
    })
}

fn map_media_item(
    item: synctv_media_providers::fnos::FnosMediaItem,
    credential_owner_id: UserId,
    server_id: &str,
) -> Result<DynamicPlaylistItem, ProviderError> {
    let item_type = if item.is_folder() {
        ItemType::Playlist
    } else if item.is_playable() {
        ItemType::Media
    } else {
        return Err(ProviderError::NotFound);
    };
    let name = item.display_title();
    let thumbnail =
        item.poster
            .clone()
            .map(|image_path| super::DynamicPlaylistItemThumbnail::Fnos {
                server_id: server_id.to_string(),
                credential_owner_id,
                image_path,
            });
    Ok(DynamicPlaylistItem {
        name,
        item_type,
        target: ProviderTarget::fnos_media(item.guid, item.media_guid, item.ancestor_guid),
        size: None,
        thumbnail,
        description: item.overview,
        modified_at: None,
        source_config: None,
        metadata: None,
    })
}

fn paginate_media_items(
    items: Vec<synctv_media_providers::fnos::FnosMediaItem>,
    credential_owner_id: UserId,
    server_id: &str,
    page: usize,
    page_size: usize,
) -> (Vec<DynamicPlaylistItem>, bool) {
    let start = page.saturating_sub(1).saturating_mul(page_size);
    let has_more = start.saturating_add(page_size) < items.len();
    let items = items
        .into_iter()
        .skip(start)
        .take(page_size)
        .filter_map(|item| map_media_item(item, credential_owner_id, server_id).ok())
        .collect();
    (items, has_more)
}

fn fnos_route_selection(
    proxy_mode: crate::models::PlaybackProxyMode,
) -> super::PlaybackRouteSelection {
    use crate::models::PlaybackProxyMode;

    match proxy_mode {
        PlaybackProxyMode::Auto | PlaybackProxyMode::Only => {
            super::PlaybackRouteSelection::PROXY_ONLY
        }
        PlaybackProxyMode::Prefer => super::PlaybackRouteSelection::PROXY_PREFERRED,
        PlaybackProxyMode::DirectPrefer => super::PlaybackRouteSelection::DIRECT_PREFERRED,
        PlaybackProxyMode::DirectOnly => super::PlaybackRouteSelection::DIRECT_ONLY,
    }
}

fn mark_fnos_playback_resources(
    result: &mut PlaybackResult,
    version: &str,
    expires_at: i64,
    selection: super::PlaybackRouteSelection,
) {
    let original_default_mode = result.default_mode.clone();
    let original_modes = result
        .playback_infos
        .iter()
        .map(|(name, info)| (name.clone(), info.clone()))
        .collect::<Vec<_>>();
    let mut generated = std::collections::HashMap::new();
    for (mode_name, original_info) in original_modes {
        if mode_name.starts_with("proxy_") {
            continue;
        }
        if mode_name.starts_with("direct_") {
            if selection.direct {
                generated.insert(mode_name, original_info);
            }
            continue;
        }
        if !selection.proxy {
            continue;
        }
        let mut proxy_info = original_info.clone();
        proxy_info.medias = original_info
            .medias
            .iter()
            .enumerate()
            .filter_map(|(media_index, media)| {
                let (credential_owner_id, server_id, resource) = match &media.provider {
                    PlaybackMediaProvider::Fnos(PlaybackFnosMedia::FileRefresh {
                        credential_owner_id,
                        server_id,
                        path,
                    }) => (
                        credential_owner_id.clone(),
                        server_id.clone(),
                        FnosProxyResource::File { path: path.clone() },
                    ),
                    PlaybackMediaProvider::Fnos(PlaybackFnosMedia::MediaRefresh {
                        credential_owner_id,
                        server_id,
                        media_guid,
                        quality_index,
                    }) => (
                        credential_owner_id.clone(),
                        server_id.clone(),
                        FnosProxyResource::Media {
                            media_guid: media_guid.clone(),
                            quality_index: *quality_index,
                        },
                    ),
                    PlaybackMediaProvider::Fnos(PlaybackFnosMedia::MediaOriginalRefresh {
                        credential_owner_id,
                        server_id,
                        media_guid,
                        path,
                    }) => (
                        credential_owner_id.clone(),
                        server_id.clone(),
                        FnosProxyResource::MediaOriginal {
                            media_guid: media_guid.clone(),
                            path: path.clone(),
                        },
                    ),
                    PlaybackMediaProvider::Fnos(PlaybackFnosMedia::TranscodeRefresh {
                        credential_owner_id,
                        server_id,
                        spec,
                    }) => (
                        credential_owner_id.clone(),
                        server_id.clone(),
                        FnosProxyResource::Transcode { spec: spec.clone() },
                    ),
                    _ => return None,
                };
                let mut proxy = media.clone();
                proxy.provider = PlaybackMediaProvider::Fnos(PlaybackFnosMedia::Proxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    media_index,
                    credential_owner_id,
                    server_id,
                    resource,
                });
                Some(proxy)
            })
            .collect();
        if proxy_info.medias.is_empty() {
            continue;
        }
        proxy_info.subtitles = original_info
            .subtitles
            .iter()
            .enumerate()
            .map(|(subtitle_index, subtitle)| PlaybackSubtitle {
                name: subtitle.name.clone(),
                language: subtitle.language.clone(),
                format: subtitle.format.clone(),
                p2p_swarm_id: subtitle.p2p_swarm_id.clone(),
                provider: PlaybackSubtitleProvider::Fnos(PlaybackFnosSubtitle::Proxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    subtitle_index,
                    url: subtitle.upstream_url().to_string(),
                    headers: subtitle.upstream_headers(),
                }),
            })
            .collect();
        generated.insert(format!("proxy_{mode_name}"), proxy_info);
    }
    result.playback_infos = generated;
    let direct_default = "direct_url".to_string();
    let proxy_default = format!("proxy_{original_default_mode}");
    if selection.prefer_proxy && result.playback_infos.contains_key(&proxy_default) {
        result.default_mode = proxy_default;
    } else if result.playback_infos.contains_key(&direct_default) {
        result.default_mode = direct_default;
    } else if result.playback_infos.contains_key(&proxy_default) {
        result.default_mode = proxy_default;
    } else if let Some(mode_name) = result.playback_infos.keys().min() {
        result.default_mode = mode_name.clone();
    } else {
        result.default_mode.clear();
    }
}

fn remap_filtered_default_index<T>(
    resources: &[(usize, T)],
    default_index: Option<usize>,
) -> Option<usize> {
    default_index
        .and_then(|default_index| {
            resources
                .iter()
                .position(|(source_index, _)| *source_index == default_index)
        })
        .or_else(|| (!resources.is_empty()).then_some(0))
}

#[async_trait]
impl MediaProvider for FnosProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn playback_proxy_policy(
        &self,
        source_config: SourceConfig<'_>,
    ) -> Result<Option<PlaybackProxyPolicy>, ProviderError> {
        let (current_mode, variant) = match source_config {
            SourceConfig::Media(MediaSourceConfig::Fnos(config)) => (
                config.proxy_mode,
                match config.source {
                    FnosMediaSource::File { .. } => "file",
                    FnosMediaSource::LibraryItem { .. } => "library_item",
                },
            ),
            SourceConfig::DynamicPlaylist(PlaylistSourceConfig::Fnos(config)) => (
                config.proxy_mode,
                match config.source {
                    FnosPlaylistSource::Files { .. } => "file",
                    FnosPlaylistSource::MediaLibrary { .. }
                    | FnosPlaylistSource::Favorites { .. }
                    | FnosPlaylistSource::History => "library_item",
                },
            ),
            _ => {
                return Err(ProviderError::InvalidConfig(
                    "FNOS requires FNOS source_config".to_string(),
                ));
            }
        };
        Ok(Some(PlaybackProxyPolicy::all_modes(
            current_mode,
            vec![PlaybackProxyAutoPolicy::new(
                variant,
                crate::models::PlaybackProxyMode::Only,
                PlaybackProxyAutoReason::ProviderSession,
            )],
        )))
    }

    fn set_playback_proxy_mode(
        &self,
        source_config: &mut MediaSourceConfig,
        mode: crate::models::PlaybackProxyMode,
    ) -> Result<(), ProviderError> {
        let MediaSourceConfig::Fnos(config) = source_config else {
            return Err(ProviderError::InvalidConfig(
                "FNOS requires FNOS media source_config".to_string(),
            ));
        };
        config.proxy_mode = mode;
        Ok(())
    }

    async fn generate_playback(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        let config = Self::media_config(source_config)?;
        let selection = fnos_route_selection(config.proxy_mode);
        let owner = *ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        let repo = self.credential_repo_or(ctx.credential_repo)?;
        let (result, cache_key) = match &config.source {
            FnosMediaSource::File { path } => {
                validate_file_path(path)?;
                let (endpoints, credential, instance_name) = self
                    .credential_with_repo(repo, owner, &config.server_id)
                    .await?;
                let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
                let listing = self
                    .client
                    .list(
                        &endpoints,
                        &credential,
                        (!parent.trim().is_empty()).then_some(parent),
                    )
                    .await?;
                let file = listing.files.iter().find(|file| file.path == *path);
                let name = file
                    .as_ref()
                    .map_or_else(
                        || path.rsplit('/').next().unwrap_or("FNOS media"),
                        |file| &file.name,
                    )
                    .to_string();
                let file_revision = file.as_ref().map_or_else(String::new, |file| {
                    format!(
                        ":size:{}:modified:{}:storage:{}",
                        file.size.unwrap_or_default(),
                        file.modified_at.unwrap_or_default(),
                        file.storage_id.unwrap_or_default()
                    )
                });
                let subtitles = discover_file_subtitles(
                    &self.client,
                    &endpoints,
                    &credential,
                    instance_name.as_deref(),
                    &config.server_id,
                    path,
                    &listing.files,
                )
                .await?;
                let mut result = PlaybackResult {
                    playback_infos: HashMap::from([(
                        "direct".to_string(),
                        PlaybackInfo {
                            thumbnail: None,
                            medias: vec![PlaybackMedia {
                                name: name.clone(),
                                format: detect_direct_url_format(path).to_string(),
                                expire_at: None,
                                metadata: None,
                                p2p_swarm_id: Some(fnos_swarm_id(
                                    instance_name.as_deref(),
                                    "media",
                                    &config.server_id,
                                    &format!("file:{path}{file_revision}"),
                                )),
                                provider: PlaybackMediaProvider::Fnos(
                                    PlaybackFnosMedia::FileRefresh {
                                        credential_owner_id: owner.to_string(),
                                        server_id: config.server_id.clone(),
                                        path: path.clone(),
                                    },
                                ),
                            }],
                            default_media_index: Some(0),
                            subtitles,
                            default_subtitle_index: None,
                            danmakus: Vec::new(),
                            default_danmaku_index: None,
                        },
                    )]),
                    default_mode: "direct".to_string(),
                    provider: crate::models::SourceProvider::Fnos,
                    provider_instance_name: instance_name,
                    duration_seconds: None,
                    playback_kind: Some(crate::models::PlaybackKind::Regular),
                    metadata: Some(PlaybackMetadata::Fnos(FnosPlaybackMetadata::File(
                        FnosFilePlaybackMetadata {
                            name,
                            path: path.clone(),
                            size: file.as_ref().and_then(|file| file.size),
                            modified_at: file.as_ref().and_then(|file| file.modified_at),
                        },
                    ))),
                };
                if selection.direct {
                    if let Ok(webdav) = self.client.webdav_config(&endpoints, &credential).await {
                        if let Ok(url) = FnosClient::webdav_file_url(&webdav, path) {
                            let headers = webdav_headers(&credential);
                            if let Some(info) = result.playback_infos.get("direct").cloned() {
                                let medias = info
                                    .medias
                                    .into_iter()
                                    .map(|mut media| {
                                        media.provider = PlaybackMediaProvider::Fnos(
                                            PlaybackFnosMedia::Direct {
                                                url: url.clone(),
                                                headers: headers.clone(),
                                            },
                                        );
                                        media
                                    })
                                    .collect();
                                result.playback_infos.insert(
                                    "direct_url".to_string(),
                                    PlaybackInfo { medias, ..info },
                                );
                            }
                        }
                    }
                }
                (result, format!("file:{path}"))
            }
            FnosMediaSource::LibraryItem {
                item_guid,
                media_guid,
            } => {
                if item_guid.trim().is_empty() {
                    return Err(ProviderError::InvalidConfig(
                        "FNOS media item_guid is required".to_string(),
                    ));
                }
                let (client, token, instance_name) = self
                    .media_credential_with_repo(repo, owner, &config.server_id)
                    .await?;
                let play = client
                    .play_info(&token, item_guid, media_guid.as_deref())
                    .await?;
                let stream = client
                    .stream(&token, &play.media_guid, &owner.to_string())
                    .await?;
                let name = play
                    .item
                    .title
                    .clone()
                    .or(play.item.tv_title.clone())
                    .unwrap_or_else(|| "FNOS media".to_string());
                let media_revision = format!(
                    ":file:{}:size:{}:video:{}",
                    stream
                        .file_stream
                        .as_ref()
                        .and_then(|file| file.path.as_deref())
                        .unwrap_or_default(),
                    stream.file_stream.as_ref().map_or(0, |file| file.size),
                    stream
                        .video_stream
                        .as_ref()
                        .and_then(|video| video.guid.as_deref())
                        .unwrap_or_default()
                );
                let original_provider = stream
                    .file_stream
                    .as_ref()
                    .and_then(|file| file.path.as_deref())
                    .and_then(non_empty_string)
                    .map_or_else(
                        || PlaybackFnosMedia::MediaRefresh {
                            credential_owner_id: owner.to_string(),
                            server_id: config.server_id.clone(),
                            media_guid: play.media_guid.clone(),
                            quality_index: None,
                        },
                        |path| PlaybackFnosMedia::MediaOriginalRefresh {
                            credential_owner_id: owner.to_string(),
                            server_id: config.server_id.clone(),
                            media_guid: play.media_guid.clone(),
                            path,
                        },
                    );
                let mut medias = vec![PlaybackMedia {
                    name: "Original".to_string(),
                    format: stream
                        .file_stream
                        .as_ref()
                        .and_then(|file| file.file_name.as_deref())
                        .map_or("video", detect_direct_url_format)
                        .to_string(),
                    expire_at: None,
                    metadata: None,
                    p2p_swarm_id: Some(fnos_swarm_id(
                        instance_name.as_deref(),
                        "media",
                        &config.server_id,
                        &format!("media:{}{media_revision}:quality:original", play.media_guid),
                    )),
                    provider: PlaybackMediaProvider::Fnos(original_provider),
                }];
                medias.extend(stream.direct_link_qualities.iter().enumerate().map(
                    |(quality_index, quality)| {
                        PlaybackMedia {
                            name: quality
                                .resolution
                                .clone()
                                .unwrap_or_else(|| format!("Quality {}", quality_index + 1)),
                            format: if quality.is_m3u8 { "hls" } else { "video" }.to_string(),
                            expire_at: None,
                            metadata: None,
                            p2p_swarm_id: Some(fnos_swarm_id(
                                instance_name.as_deref(),
                                "media",
                                &config.server_id,
                                &format!(
                                    "media:{}{media_revision}:quality:{quality_index}:bitrate:{}:resolution:{}",
                                    play.media_guid,
                                    quality.bitrate,
                                    quality.resolution.as_deref().unwrap_or_default()
                                ),
                            )),
                            provider: PlaybackMediaProvider::Fnos(
                                PlaybackFnosMedia::MediaRefresh {
                                    credential_owner_id: owner.to_string(),
                                    server_id: config.server_id.clone(),
                                    media_guid: play.media_guid.clone(),
                                    quality_index: Some(quality_index),
                                },
                            ),
                        }
                    },
                ));
                if let Some(video) = stream.video_stream.as_ref() {
                    let video_guid = video.guid.as_deref().unwrap_or_default().trim();
                    if !video_guid.is_empty() {
                        let mut transcode_audios =
                            stream.audio_streams.iter().map(Some).collect::<Vec<_>>();
                        if transcode_audios.is_empty() {
                            transcode_audios.push(None);
                        } else {
                            transcode_audios.sort_by_key(|audio| {
                                audio.is_some_and(|audio| audio.is_default == 0)
                            });
                        }
                        let subtitle_guid = stream
                            .subtitle_streams
                            .iter()
                            .find(|subtitle| subtitle.is_default != 0)
                            .and_then(|subtitle| subtitle.guid.clone())
                            .unwrap_or_default();
                        let fallback_resolution = video
                            .resolution_type
                            .clone()
                            .filter(|value| !value.trim().is_empty())
                            .unwrap_or_else(|| format!("{}x{}", video.width, video.height));
                        let video_encoder = video
                            .codec_name
                            .clone()
                            .unwrap_or_else(|| "h264".to_string());
                        let forced_sdr = video
                            .color_range_type
                            .as_deref()
                            .is_some_and(|range| !range.eq_ignore_ascii_case("sdr"));
                        for quality in &stream.qualities {
                            let resolution = quality
                                .resolution
                                .clone()
                                .filter(|value| !value.trim().is_empty())
                                .unwrap_or_else(|| fallback_resolution.clone());
                            for audio in &transcode_audios {
                                let audio_label = audio.and_then(|audio| {
                                    audio
                                        .title
                                        .as_deref()
                                        .filter(|value| !value.trim().is_empty())
                                        .or_else(|| {
                                            audio
                                                .language
                                                .as_deref()
                                                .filter(|value| !value.trim().is_empty())
                                        })
                                });
                                let spec = FnosTranscodeResource {
                                    media_guid: play.media_guid.clone(),
                                    video_guid: video_guid.to_string(),
                                    video_encoder: video_encoder.clone(),
                                    resolution: resolution.clone(),
                                    bitrate: if quality.bitrate > 0 {
                                        quality.bitrate
                                    } else {
                                        video.bps
                                    },
                                    audio_guid: audio
                                        .and_then(|audio| audio.guid.clone())
                                        .unwrap_or_default(),
                                    subtitle_guid: subtitle_guid.clone(),
                                    channels: audio.map_or(2, |audio| audio.channels.max(1)),
                                    forced_sdr,
                                };
                                let descriptor = serde_json::to_string(&spec)
                                    .expect("FNOS transcode resource is JSON serializable");
                                medias.push(PlaybackMedia {
                                    name: audio_label.map_or_else(
                                        || format!("Transcode {resolution}"),
                                        |label| format!("Transcode {resolution} - {label}"),
                                    ),
                                    format: "hls".to_string(),
                                    expire_at: None,
                                    metadata: None,
                                    p2p_swarm_id: Some(fnos_swarm_id(
                                        instance_name.as_deref(),
                                        "media",
                                        &config.server_id,
                                        &format!("transcode:{descriptor}"),
                                    )),
                                    provider: PlaybackMediaProvider::Fnos(
                                        PlaybackFnosMedia::TranscodeRefresh {
                                            credential_owner_id: owner.to_string(),
                                            server_id: config.server_id.clone(),
                                            spec,
                                        },
                                    ),
                                });
                            }
                        }
                    }
                }
                let duration_seconds = stream
                    .video_stream
                    .as_ref()
                    .map(|video| video.duration)
                    .or_else(|| stream.file_stream.as_ref().map(|file| file.duration))
                    .filter(|value| *value > 0);
                let duration = duration_seconds
                    .map(|value| std::time::Duration::from_secs(value).as_secs_f64());
                let subtitle_streams = stream
                    .subtitle_streams
                    .iter()
                    .filter(|subtitle| {
                        subtitle
                            .guid
                            .as_deref()
                            .is_some_and(|guid| !guid.trim().is_empty())
                    })
                    .collect::<Vec<_>>();
                let default_subtitle_index = subtitle_streams
                    .iter()
                    .position(|subtitle| subtitle.is_default != 0);
                let subtitles = subtitle_streams
                    .into_iter()
                    .map(|subtitle| {
                        let guid = subtitle.guid.as_deref().expect("filtered subtitle guid");
                        PlaybackSubtitle {
                            name: subtitle
                                .title
                                .clone()
                                .filter(|value| !value.trim().is_empty())
                                .unwrap_or_else(|| "Subtitle".to_string()),
                            language: subtitle.language.clone().unwrap_or_default(),
                            format: subtitle
                                .format
                                .clone()
                                .or_else(|| subtitle.codec_name.clone())
                                .unwrap_or_default(),
                            p2p_swarm_id: Some(fnos_swarm_id(
                                instance_name.as_deref(),
                                "subtitle",
                                &config.server_id,
                                &format!("media:{}:subtitle:{guid}", play.media_guid),
                            )),
                            provider: PlaybackSubtitleProvider::Fnos(
                                PlaybackFnosSubtitle::Direct {
                                    url: client.subtitle_url(guid),
                                    headers: FnosMediaClient::auth_headers(&token),
                                    expire_at: None,
                                },
                            ),
                        }
                    })
                    .collect();
                let prefer_transcode = super::playback_profile_prefers_transcode(
                    ctx.playback_client_profile(),
                    medias
                        .first()
                        .map_or("video", |media| media.format.as_str()),
                );
                let max_bitrate = ctx
                    .playback_client_profile()
                    .and_then(|profile| profile.max_streaming_bitrate)
                    .and_then(|bitrate| u64::try_from(bitrate).ok());
                let default_media_index = if prefer_transcode {
                    medias
                        .iter()
                        .enumerate()
                        .filter_map(|(index, media)| match &media.provider {
                            PlaybackMediaProvider::Fnos(PlaybackFnosMedia::TranscodeRefresh {
                                spec,
                                ..
                            }) if max_bitrate.is_none_or(|limit| spec.bitrate <= limit) => {
                                Some((index, spec.bitrate))
                            }
                            _ => None,
                        })
                        .max_by_key(|(_, bitrate)| *bitrate)
                        .map_or(0, |(index, _)| index)
                } else {
                    0
                };
                let mut result = PlaybackResult {
                    playback_infos: HashMap::from([(
                        "direct".to_string(),
                        PlaybackInfo {
                            thumbnail: play
                                .item
                                .posters
                                .as_deref()
                                .or(play.item.poster.as_deref())
                                .map(|path| client.image_url(path, 800)),
                            medias,
                            default_media_index: Some(default_media_index),
                            subtitles,
                            default_subtitle_index,
                            danmakus: Vec::new(),
                            default_danmaku_index: None,
                        },
                    )]),
                    default_mode: "direct".to_string(),
                    provider: crate::models::SourceProvider::Fnos,
                    provider_instance_name: instance_name,
                    duration_seconds: duration,
                    playback_kind: Some(crate::models::PlaybackKind::Regular),
                    metadata: Some(PlaybackMetadata::Fnos(FnosPlaybackMetadata::Media(
                        FnosMediaPlaybackMetadata {
                            item_guid: play.guid.clone(),
                            media_guid: play.media_guid.clone(),
                            title: name,
                            overview: play.item.overview.clone(),
                            poster_url: play
                                .item
                                .posters
                                .as_deref()
                                .or(play.item.poster.as_deref())
                                .map(|path| client.image_url(path, 1200)),
                            backdrop_url: play
                                .item
                                .backdrops
                                .as_deref()
                                .map(|path| client.image_url(path, 1920)),
                            width: stream
                                .video_stream
                                .as_ref()
                                .and_then(|video| (video.width > 0).then_some(video.width)),
                            height: stream
                                .video_stream
                                .as_ref()
                                .and_then(|video| (video.height > 0).then_some(video.height)),
                            video_codec: stream
                                .video_stream
                                .as_ref()
                                .and_then(|video| video.codec_name.clone()),
                            video_profile: stream
                                .video_stream
                                .as_ref()
                                .and_then(|video| video.profile.clone()),
                            bit_depth: stream
                                .video_stream
                                .as_ref()
                                .and_then(|video| (video.bit_depth > 0).then_some(video.bit_depth)),
                            dolby_vision_profile: stream.video_stream.as_ref().and_then(|video| {
                                (video.dv_profile >= 0).then_some(video.dv_profile)
                            }),
                            frame_rate: stream
                                .video_stream
                                .as_ref()
                                .and_then(|video| video.r_frame_rate.clone()),
                            season_number: (play.item.season_number > 0)
                                .then_some(play.item.season_number),
                            episode_number: (play.item.episode_number > 0)
                                .then_some(play.item.episode_number),
                            progress_seconds: play.ts,
                            duration_seconds: duration_seconds.unwrap_or_default(),
                            watched: duration_seconds.is_some_and(|duration| {
                                duration > 0 && u128::from(play.ts) * 10 >= u128::from(duration) * 9
                            }),
                            audio_tracks: stream
                                .audio_streams
                                .iter()
                                .map(|track| FnosAudioTrackMetadata {
                                    guid: track.guid.clone(),
                                    title: track.title.clone(),
                                    language: track.language.clone(),
                                    codec: track.codec_name.clone(),
                                    channels: track.channels,
                                    bitrate: track.bps,
                                    default: track.is_default != 0,
                                })
                                .collect(),
                            subtitle_tracks: stream
                                .subtitle_streams
                                .iter()
                                .map(|track| FnosSubtitleTrackMetadata {
                                    guid: track.guid.clone(),
                                    title: track.title.clone(),
                                    language: track.language.clone(),
                                    codec: track.codec_name.clone(),
                                    format: track.format.clone(),
                                    external: track.is_external != 0,
                                    default: track.is_default != 0,
                                    forced: track.forced != 0,
                                })
                                .collect(),
                        },
                    ))),
                };
                if selection.direct {
                    let webdav = match self
                        .credential_with_repo(repo, owner, &config.server_id)
                        .await
                    {
                        Ok((endpoints, credential, _)) => self
                            .client
                            .webdav_config(&endpoints, &credential)
                            .await
                            .ok()
                            .map(|config| (config, credential)),
                        Err(_) => None,
                    };
                    let direct_medias =
                        result
                            .playback_infos
                            .get("direct")
                            .cloned()
                            .map(|mut info| {
                                let default_media_index = info.default_media_index;
                                let medias = std::mem::take(&mut info.medias)
                                    .into_iter()
                                    .enumerate()
                                    .filter_map(|media| {
                                        let (source_index, mut media) = media;
                                        let direct = match &media.provider {
                                            PlaybackMediaProvider::Fnos(
                                                PlaybackFnosMedia::MediaOriginalRefresh {
                                                    path,
                                                    ..
                                                },
                                            ) => {
                                                let (config, credential) = webdav.as_ref()?;
                                                let url = FnosClient::webdav_file_url(config, path)
                                                    .ok()?;
                                                Some((url, webdav_headers(credential)))
                                            }
                                            PlaybackMediaProvider::Fnos(
                                                PlaybackFnosMedia::MediaRefresh {
                                                    media_guid,
                                                    quality_index,
                                                    ..
                                                },
                                            ) => Some((
                                                client.media_url(media_guid, *quality_index),
                                                FnosMediaClient::auth_headers(&token),
                                            )),
                                            _ => None,
                                        }?;
                                        media.provider = PlaybackMediaProvider::Fnos(
                                            PlaybackFnosMedia::Direct {
                                                url: direct.0,
                                                headers: direct.1,
                                            },
                                        );
                                        Some((source_index, media))
                                    })
                                    .collect::<Vec<_>>();
                                info.default_media_index =
                                    remap_filtered_default_index(&medias, default_media_index);
                                let medias: Vec<PlaybackMedia> =
                                    medias.into_iter().map(|(_, media)| media).collect();
                                (medias, info)
                            });
                    if let Some((medias, info)) = direct_medias {
                        if !medias.is_empty() {
                            result
                                .playback_infos
                                .insert("direct_url".to_string(), PlaybackInfo { medias, ..info });
                        }
                    }
                }
                (result, format!("media:{item_guid}:{}", play.media_guid))
            }
        };
        let result = super::cached_versioned_playback_or_fill(
            Self::NAME,
            &format!(
                "playback:{owner}:{}:room:{}:{cache_key}:profile:{}:proxy:{}",
                config.server_id,
                ctx.room_id
                    .map_or_else(|| "none".to_string(), |id| id.to_string()),
                super::playback_profile_cache_token(ctx.playback_client_profile()),
                config.proxy_mode.as_str()
            ),
            Duration::from_hours(2),
            ctx,
            |result, version, expires_at| {
                mark_fnos_playback_resources(result, version, expires_at, selection);
            },
            || async { Ok(result) },
        )
        .await?;
        if config.proxy_mode == crate::models::PlaybackProxyMode::DirectOnly
            && result.playback_infos.is_empty()
        {
            return Err(ProviderError::UnsupportedFormat(
                "This FNOS media source cannot provide a direct playback route".to_string(),
            ));
        }

        let media = result
            .metadata
            .as_ref()
            .and_then(|metadata| match metadata {
                PlaybackMetadata::Fnos(FnosPlaybackMetadata::Media(media)) => Some(media),
                _ => None,
            });
        let resource_version = result.playback_infos.values().find_map(|info| {
            info.medias.iter().find_map(|media| match &media.provider {
                PlaybackMediaProvider::Fnos(PlaybackFnosMedia::Proxy { version, .. }) => {
                    Some(version.clone())
                }
                _ => None,
            })
        });
        if let (Some(media), Some(resource_version)) = (media, resource_version) {
            if let Some((repo, session)) = super::playback_session_registration(
                ctx,
                format!("media:{resource_version}"),
                Some(resource_version.clone()),
                ProviderPlaybackSession::Fnos(FnosPlaybackSession::MediaSession {
                    server_id: config.server_id.clone(),
                    item_guid: media.item_guid.clone(),
                    media_guid: Some(media.media_guid.clone()),
                }),
            )? {
                repo.upsert(session)
                    .await
                    .map_err(|error| ProviderError::Internal(error.to_string()))?;
            }
        }
        Ok(result)
    }

    async fn validate_source_config(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        let server_id = Self::source_server_id(source_config)?;
        match source_config {
            SourceConfig::Media(config) => match &Self::media_config(config)?.source {
                FnosMediaSource::File { path } => validate_file_path(path)?,
                FnosMediaSource::LibraryItem { item_guid, .. } if item_guid.trim().is_empty() => {
                    return Err(ProviderError::InvalidConfig(
                        "FNOS media item_guid is required".to_string(),
                    ));
                }
                FnosMediaSource::LibraryItem { .. } => {}
            },
            SourceConfig::DynamicPlaylist(config) => match &Self::playlist_config(config)?.source {
                FnosPlaylistSource::Files { path } => validate_path(path)?,
                FnosPlaylistSource::MediaLibrary { .. }
                | FnosPlaylistSource::Favorites { .. }
                | FnosPlaylistSource::History => {}
            },
        }
        let owner = ctx.credential_owner_id().ok_or_else(|| {
            ProviderError::Internal("credential_owner_id is unavailable".to_string())
        })?;
        let exists = self
            .credential_repo_or(ctx.credential_repo)?
            .get_by_provider_and_server(*owner, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .is_some();
        if !exists {
            return Err(ProviderError::CredentialNotFound(format!(
                "Referenced FNOS credential not found for server_id '{server_id}'"
            )));
        }
        Ok(())
    }

    async fn source_cover(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Option<SourceCover>, ProviderError> {
        let owner = *ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        let repo = self.credential_repo_or(ctx.credential_repo)?;
        let server_id = Self::source_server_id(source_config)?.to_string();
        let image_path = match source_config {
            SourceConfig::Media(config) => match &Self::media_config(config)?.source {
                FnosMediaSource::File { .. } => None,
                FnosMediaSource::LibraryItem {
                    item_guid,
                    media_guid,
                } => {
                    let (client, token, _) = self
                        .media_credential_with_repo(repo, owner, &server_id)
                        .await?;
                    let play = client
                        .play_info(&token, item_guid, media_guid.as_deref())
                        .await?;
                    play.item.posters.or(play.item.poster)
                }
            },
            SourceConfig::DynamicPlaylist(config) => match &Self::playlist_config(config)?.source {
                FnosPlaylistSource::Files { .. } => None,
                FnosPlaylistSource::MediaLibrary {
                    library_guid,
                    media_types,
                    parent_guid,
                } => {
                    let (client, token, _) = self
                        .media_credential_with_repo(repo, owner, &server_id)
                        .await?;
                    let libraries = client.libraries(&token).await?;
                    let library_cover = libraries
                        .iter()
                        .find(|library| library.guid == *library_guid);
                    if let Some(library) = library_cover {
                        library
                            .poster
                            .clone()
                            .or_else(|| library.posters.first().cloned())
                    } else {
                        client
                            .all_items(
                                &token,
                                &FnosMediaListRequest {
                                    ancestor_guid: Some(library_guid.clone()),
                                    parent_guid: parent_guid.clone(),
                                    exclude_grouped_video: 1,
                                    sort_type: "ASC".to_string(),
                                    sort_column: "title".to_string(),
                                    page_size: 1,
                                    page: 1,
                                    tags: FnosMediaTags {
                                        media_types: media_types.clone(),
                                    },
                                },
                            )
                            .await?
                            .into_iter()
                            .filter(|item| {
                                parent_guid.as_deref().map_or_else(
                                    || item.parent_guid.as_deref().is_none_or(str::is_empty),
                                    |parent| item.parent_guid.as_deref() == Some(parent),
                                )
                            })
                            .find_map(|item| item.poster)
                    }
                }
                FnosPlaylistSource::Favorites { media_types } => {
                    let (client, token, _) = self
                        .media_credential_with_repo(repo, owner, &server_id)
                        .await?;
                    client
                        .favorites(
                            &token,
                            &FnosMediaListRequest {
                                ancestor_guid: None,
                                parent_guid: None,
                                exclude_grouped_video: 1,
                                sort_type: "DESC".to_string(),
                                sort_column: "create_time".to_string(),
                                page_size: 1,
                                page: 1,
                                tags: FnosMediaTags {
                                    media_types: media_types.clone(),
                                },
                            },
                        )
                        .await?
                        .list
                        .unwrap_or_default()
                        .into_iter()
                        .find_map(|item| item.poster)
                }
                FnosPlaylistSource::History => {
                    let (client, token, _) = self
                        .media_credential_with_repo(repo, owner, &server_id)
                        .await?;
                    client
                        .history(&token)
                        .await?
                        .into_iter()
                        .find_map(|item| item.poster)
                }
            },
        };
        Ok(image_path
            .filter(|path| !path.trim().is_empty())
            .map(|image_path| SourceCover::Fnos {
                server_id,
                credential_owner_id: owner,
                image_path,
            }))
    }

    fn as_dynamic_playlist_provider(&self) -> Option<&dyn DynamicPlaylistProvider> {
        Some(self)
    }

    fn as_playback_session_lifecycle(&self) -> Option<&dyn ProviderPlaybackSessionLifecycle> {
        Some(self)
    }

    fn credential_dependencies(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Vec<ProviderCredentialDependency>, ProviderError> {
        let server_id = Self::source_server_id(source_config)?;
        let user_id = ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        Ok(vec![ProviderCredentialDependency::new(
            crate::models::SourceProvider::Fnos,
            *user_id,
            server_id.to_string(),
        )])
    }
}

#[async_trait]
impl ProviderPlaybackSessionLifecycle for FnosProvider {
    async fn progress(
        &self,
        ctx: &ProviderContext<'_>,
        session: &crate::models::ProviderPlaybackSessionRecord,
        position: f64,
        _paused: bool,
    ) -> Result<(), ProviderError> {
        let ProviderPlaybackSession::Fnos(FnosPlaybackSession::MediaSession {
            server_id,
            item_guid,
            media_guid,
        }) = &session.session
        else {
            return Ok(());
        };
        let source_config = MediaSourceConfig::Fnos(FnosMediaSourceConfig {
            server_id: server_id.clone(),
            proxy_mode: crate::models::PlaybackProxyMode::Auto,
            source: FnosMediaSource::LibraryItem {
                item_guid: item_guid.clone(),
                media_guid: media_guid.clone(),
            },
        });
        self.report_media_progress(
            ctx,
            session
                .resource_version
                .as_deref()
                .unwrap_or(&session.resource_key),
            &source_config,
            position,
        )
        .await
    }

    async fn cleanup(
        &self,
        ctx: &ProviderContext<'_>,
        session: &crate::models::ProviderPlaybackSessionRecord,
    ) -> Result<(), ProviderError> {
        match &session.session {
            ProviderPlaybackSession::Fnos(FnosPlaybackSession::MediaSession { .. }) => {
                self.progress(ctx, session, session.stop_position.unwrap_or(0.0), false)
                    .await
            }
            ProviderPlaybackSession::Fnos(FnosPlaybackSession::Transcode {
                server_id,
                play_link,
                ..
            }) => {
                let owner = *ctx
                    .credential_owner_or_user_id()
                    .ok_or(ProviderError::CredentialRequired)?;
                let credential_repo = self.credential_repo_or(ctx.credential_repo)?;
                let (client, token, _) = self
                    .media_credential_with_repo(credential_repo, owner, server_id)
                    .await?;
                self.quit_transcode(
                    &client,
                    &token,
                    session
                        .resource_version
                        .as_deref()
                        .unwrap_or(&session.resource_key),
                    play_link,
                )
                .await
            }
            _ => Err(ProviderError::InvalidConfig(
                "FNOS lifecycle received another provider's session".to_string(),
            )),
        }
    }
}

async fn discover_file_subtitles(
    client: &FnosClient,
    endpoints: &FnosEndpoints,
    credential: &FnosCredential,
    provider_instance_name: Option<&str>,
    server_id: &str,
    media_path: &str,
    files: &[FnosFile],
) -> Result<Vec<PlaybackSubtitle>, ProviderError> {
    let related = files
        .iter()
        .filter(|file| related_file_subtitle(media_path, file))
        .take(32)
        .collect::<Vec<_>>();
    if related.is_empty() {
        return Ok(Vec::new());
    }
    let webdav = client.webdav_config(endpoints, credential).await?;
    let headers = webdav_headers(credential);
    related
        .into_iter()
        .map(|file| {
            Ok(PlaybackSubtitle {
                name: file.name.clone(),
                language: file_subtitle_language(media_path, &file.name),
                format: file_subtitle_format(&file.name)
                    .unwrap_or_default()
                    .to_string(),
                p2p_swarm_id: Some(fnos_swarm_id(
                    provider_instance_name,
                    "subtitle",
                    server_id,
                    &format!(
                        "file:{}:size:{}:modified:{}:storage:{}",
                        file.path,
                        file.size.unwrap_or_default(),
                        file.modified_at.unwrap_or_default(),
                        file.storage_id.unwrap_or_default()
                    ),
                )),
                provider: PlaybackSubtitleProvider::Fnos(PlaybackFnosSubtitle::Direct {
                    url: FnosClient::webdav_file_url(&webdav, &file.path)?,
                    headers: headers.clone(),
                    expire_at: None,
                }),
            })
        })
        .collect()
}

fn fnos_swarm_id(
    provider_instance_name: Option<&str>,
    resource_kind: &str,
    server_id: &str,
    resource: &str,
) -> String {
    super::provider_p2p_swarm_id(
        FnosProvider::NAME,
        provider_instance_name,
        resource_kind,
        &format!("server:{server_id}:{resource}"),
    )
}

fn related_file_subtitle(media_path: &str, file: &FnosFile) -> bool {
    !file.is_dir
        && file_subtitle_format(&file.name).is_some()
        && related_file_stem(
            media_path.rsplit('/').next().unwrap_or_default(),
            &file.name,
        )
}

fn related_file_stem(media_name: &str, subtitle_name: &str) -> bool {
    let media_stem = media_name
        .rsplit_once('.')
        .map_or(media_name, |(stem, _)| stem)
        .to_ascii_lowercase();
    let subtitle_stem = subtitle_name
        .rsplit_once('.')
        .map_or(subtitle_name, |(stem, _)| stem)
        .to_ascii_lowercase();
    subtitle_stem == media_stem
        || subtitle_stem
            .strip_prefix(&media_stem)
            .is_some_and(|suffix| suffix.starts_with(['.', '_', '-']))
}

fn file_subtitle_format(name: &str) -> Option<&str> {
    let extension = name.rsplit_once('.')?.1;
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "srt" | "vtt" | "ass" | "ssa" | "sub" | "ttml"
    )
    .then_some(extension)
}

fn file_subtitle_language(media_path: &str, subtitle_name: &str) -> String {
    let media_name = media_path.rsplit('/').next().unwrap_or_default();
    let media_stem = media_name
        .rsplit_once('.')
        .map_or(media_name, |(stem, _)| stem);
    let subtitle_stem = subtitle_name
        .rsplit_once('.')
        .map_or(subtitle_name, |(stem, _)| stem);
    subtitle_stem
        .strip_prefix(media_stem)
        .unwrap_or_default()
        .trim_start_matches(['.', '_', '-'])
        .to_string()
}

#[async_trait]
impl DynamicPlaylistProvider for FnosProvider {
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: Option<&ProviderTarget>,
        query: DynamicListQuery,
    ) -> Result<DynamicListResult, ProviderError> {
        let base =
            Self::playlist_config(playlist.source_config.as_ref().ok_or_else(|| {
                ProviderError::InvalidConfig("Missing source_config".to_string())
            })?)?;
        let owner = *ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        let repo = self.credential_repo_or(ctx.credential_repo)?;
        let DynamicPagination::Page { page } = query.pagination else {
            return Err(ProviderError::InvalidConfig(
                "FNOS uses page pagination".to_string(),
            ));
        };
        let page = page.max(1);
        let page_size = query.page_size.max(1);
        let (items, has_more) = match &base.source {
            FnosPlaylistSource::Files { path: base_path } => {
                let relative = decode_file_target(target)?;
                let path = relative.as_deref().map_or_else(
                    || base_path.clone(),
                    |relative| join_path(base_path, relative),
                );
                let (listing, _) = self
                    .list_with_repo(repo, owner, &base.server_id, &path)
                    .await?;
                let search = query.search.as_deref().map(str::to_ascii_lowercase);
                let mut items = listing
                    .files
                    .into_iter()
                    .filter(|file| {
                        search
                            .as_ref()
                            .is_none_or(|search| file.name.to_ascii_lowercase().contains(search))
                    })
                    .filter_map(|file| map_directory_item(base_path, file).ok())
                    .collect::<Vec<_>>();
                items.sort_by(|left, right| {
                    (
                        left.item_type != ItemType::Playlist,
                        left.name.to_ascii_lowercase(),
                    )
                        .cmp(&(
                            right.item_type != ItemType::Playlist,
                            right.name.to_ascii_lowercase(),
                        ))
                });
                let start = page.saturating_sub(1).saturating_mul(page_size);
                let has_more = start.saturating_add(page_size) < items.len();
                (
                    items.into_iter().skip(start).take(page_size).collect(),
                    has_more,
                )
            }
            FnosPlaylistSource::MediaLibrary {
                library_guid,
                media_types,
                parent_guid,
            } => {
                let target = decode_media_target(target)?;
                let current_parent_guid = target
                    .as_ref()
                    .map(|target| target.item_guid.clone())
                    .or_else(|| parent_guid.clone());
                let (client, token, _) = self
                    .media_credential_with_repo(repo, owner, &base.server_id)
                    .await?;
                if let Some(search) = query
                    .search
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    let mut matches = client.search(&token, search).await?;
                    matches.retain(|item| {
                        let in_library = item.ancestor_guid.as_deref() == Some(library_guid);
                        let in_parent = current_parent_guid.as_deref().map_or_else(
                            || item.parent_guid.as_deref().is_none_or(str::is_empty),
                            |parent| item.parent_guid.as_deref() == Some(parent),
                        );
                        let has_type = media_types.is_empty()
                            || media_types
                                .iter()
                                .any(|kind| item.item_type.eq_ignore_ascii_case(kind));
                        in_library && in_parent && has_type
                    });
                    let start = page.saturating_sub(1).saturating_mul(page_size);
                    let has_more = start.saturating_add(page_size) < matches.len();
                    let items = matches
                        .into_iter()
                        .skip(start)
                        .take(page_size)
                        .filter_map(|item| map_media_item(item, owner, &base.server_id).ok())
                        .collect();
                    return Ok(DynamicListResult {
                        items,
                        pagination: DynamicPagination::Page { page },
                        has_more,
                        supports_search: true,
                    });
                }
                let mut items = client
                    .all_items(
                        &token,
                        &FnosMediaListRequest {
                            ancestor_guid: Some(library_guid.clone()),
                            parent_guid: current_parent_guid.clone(),
                            exclude_grouped_video: 1,
                            sort_type: "ASC".to_string(),
                            sort_column: "title".to_string(),
                            page_size: 200,
                            page: 1,
                            tags: FnosMediaTags {
                                media_types: Vec::new(),
                            },
                        },
                    )
                    .await?;
                items.retain(|item| {
                    let in_parent = current_parent_guid.as_deref().map_or_else(
                        || item.parent_guid.as_deref().is_none_or(str::is_empty),
                        |parent| item.parent_guid.as_deref() == Some(parent),
                    );
                    let has_type = media_types.is_empty()
                        || media_types
                            .iter()
                            .any(|kind| item.item_type.eq_ignore_ascii_case(kind));
                    in_parent && has_type
                });
                paginate_media_items(items, owner, &base.server_id, page, page_size)
            }
            FnosPlaylistSource::Favorites { media_types } => {
                let target = decode_media_target(target)?;
                let parent_guid = target.as_ref().map(|target| target.item_guid.clone());
                let library_guid = target
                    .as_ref()
                    .and_then(|target| target.library_guid.clone());
                let (client, token, _) = self
                    .media_credential_with_repo(repo, owner, &base.server_id)
                    .await?;
                if let Some(search) = query
                    .search
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    let mut matches = client.search(&token, search).await?;
                    matches.retain(|item| {
                        let in_library = library_guid
                            .as_deref()
                            .is_none_or(|library| item.ancestor_guid.as_deref() == Some(library));
                        let in_folder = parent_guid
                            .as_deref()
                            .is_none_or(|parent| item.parent_guid.as_deref() == Some(parent));
                        let has_type = media_types.is_empty()
                            || media_types
                                .iter()
                                .any(|kind| item.item_type.eq_ignore_ascii_case(kind));
                        item.is_favorite != 0 && in_library && in_folder && has_type
                    });
                    paginate_media_items(matches, owner, &base.server_id, page, page_size)
                } else if let Some(library_guid) = library_guid {
                    let mut items = client
                        .all_items(
                            &token,
                            &FnosMediaListRequest {
                                ancestor_guid: Some(library_guid),
                                parent_guid: parent_guid.clone(),
                                exclude_grouped_video: 1,
                                sort_type: "ASC".to_string(),
                                sort_column: "title".to_string(),
                                page_size: 200,
                                page: 1,
                                tags: FnosMediaTags {
                                    media_types: Vec::new(),
                                },
                            },
                        )
                        .await?;
                    items.retain(|item| {
                        let in_parent = item.parent_guid.as_deref() == parent_guid.as_deref();
                        let has_type = media_types.is_empty()
                            || media_types
                                .iter()
                                .any(|kind| item.item_type.eq_ignore_ascii_case(kind));
                        in_parent && has_type
                    });
                    paginate_media_items(items, owner, &base.server_id, page, page_size)
                } else {
                    let response = client
                        .favorites(
                            &token,
                            &FnosMediaListRequest {
                                ancestor_guid: None,
                                parent_guid: None,
                                exclude_grouped_video: 1,
                                sort_type: "DESC".to_string(),
                                sort_column: "create_time".to_string(),
                                page_size: u32::try_from(page_size).unwrap_or(u32::MAX),
                                page: u32::try_from(page).unwrap_or(u32::MAX),
                                tags: FnosMediaTags {
                                    media_types: media_types.clone(),
                                },
                            },
                        )
                        .await?;
                    let consumed = page.saturating_mul(page_size);
                    (
                        response
                            .list
                            .unwrap_or_default()
                            .into_iter()
                            .filter_map(|item| map_media_item(item, owner, &base.server_id).ok())
                            .collect(),
                        u64::try_from(consumed).unwrap_or(u64::MAX) < response.total,
                    )
                }
            }
            FnosPlaylistSource::History => {
                let (client, token, _) = self
                    .media_credential_with_repo(repo, owner, &base.server_id)
                    .await?;
                let search = query.search.as_deref().map(str::to_ascii_lowercase);
                let items = client
                    .history(&token)
                    .await?
                    .into_iter()
                    .filter(|item| {
                        search.as_ref().is_none_or(|search| {
                            item.display_title().to_ascii_lowercase().contains(search)
                        })
                    })
                    .collect();
                paginate_media_items(items, owner, &base.server_id, page, page_size)
            }
        };
        Ok(DynamicListResult {
            items,
            pagination: DynamicPagination::Page { page },
            has_more,
            supports_search: true,
        })
    }

    async fn resolve_item(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &ProviderTarget,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let base =
            Self::playlist_config(playlist.source_config.as_ref().ok_or_else(|| {
                ProviderError::InvalidConfig("Missing source_config".to_string())
            })?)?;
        match &base.source {
            FnosPlaylistSource::Files { .. } => {
                let relative = decode_file_target(Some(target))?.ok_or(ProviderError::NotFound)?;
                let parent = relative.rsplit_once('/').map_or("", |(parent, _)| parent);
                let parent_target = (!parent.is_empty())
                    .then(|| encode_file_target(parent))
                    .transpose()?;
                let result = self
                    .list_playlist(
                        ctx,
                        playlist,
                        parent_target.as_ref(),
                        DynamicListQuery {
                            pagination: DynamicPagination::Page { page: 1 },
                            page_size: usize::MAX,
                            ..DynamicListQuery::default()
                        },
                    )
                    .await?;
                result
                    .items
                    .iter()
                    .find(|item| item.item_type == ItemType::Media && &item.target == target)
                    .map(|item| Self::next_item(base, item))
                    .transpose()
            }
            FnosPlaylistSource::MediaLibrary { .. }
            | FnosPlaylistSource::Favorites { .. }
            | FnosPlaylistSource::History => {
                let target = decode_media_target(Some(target))?.ok_or(ProviderError::NotFound)?;
                let owner = *ctx
                    .credential_owner_or_user_id()
                    .ok_or(ProviderError::CredentialRequired)?;
                let repo = self.credential_repo_or(ctx.credential_repo)?;
                let (client, token, _) = self
                    .media_credential_with_repo(repo, owner, &base.server_id)
                    .await?;
                let play = client
                    .play_info(&token, &target.item_guid, target.media_guid.as_deref())
                    .await?;
                let item = DynamicPlaylistItem {
                    name: play
                        .item
                        .title
                        .or(play.item.tv_title)
                        .unwrap_or_else(|| "FNOS media".to_string()),
                    item_type: ItemType::Media,
                    target: ProviderTarget::fnos_media(
                        target.item_guid,
                        Some(play.media_guid),
                        target.library_guid,
                    ),
                    size: None,
                    thumbnail: None,
                    description: play.item.overview,
                    modified_at: None,
                    source_config: None,
                    metadata: None,
                };
                Self::next_media_item(base, &item).map(Some)
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
        let base =
            Self::playlist_config(playlist.source_config.as_ref().ok_or_else(|| {
                ProviderError::InvalidConfig("Missing source_config".to_string())
            })?)?;
        let (parent_target, page_size) = match &base.source {
            FnosPlaylistSource::Files { .. } => {
                let relative = decode_file_target(Some(target))?.ok_or(ProviderError::NotFound)?;
                let parent = relative.rsplit_once('/').map_or("", |(parent, _)| parent);
                (
                    (!parent.is_empty())
                        .then(|| encode_file_target(parent))
                        .transpose()?,
                    usize::MAX,
                )
            }
            FnosPlaylistSource::MediaLibrary { .. } => {
                let target = decode_media_target(Some(target))?.ok_or(ProviderError::NotFound)?;
                let owner = *ctx
                    .credential_owner_or_user_id()
                    .ok_or(ProviderError::CredentialRequired)?;
                let repo = self.credential_repo_or(ctx.credential_repo)?;
                let (client, token, _) = self
                    .media_credential_with_repo(repo, owner, &base.server_id)
                    .await?;
                let play = client
                    .play_info(&token, &target.item_guid, target.media_guid.as_deref())
                    .await?;
                (
                    play.item
                        .parent_guid
                        .filter(|value| !value.is_empty())
                        .map(|parent| {
                            ProviderTarget::fnos_media(parent, None, target.library_guid.clone())
                        }),
                    200,
                )
            }
            FnosPlaylistSource::Favorites { .. } | FnosPlaylistSource::History => (None, 200),
        };
        let mut media = self
            .scan_playlist_media(
                ctx,
                playlist,
                parent_target.as_ref(),
                target,
                page_size,
                play_mode,
            )
            .await?;
        if matches!(&base.source, FnosPlaylistSource::Favorites { .. })
            && !media.iter().any(|item| &item.target == target)
        {
            let decoded_target =
                decode_media_target(Some(target))?.ok_or(ProviderError::NotFound)?;
            let owner = *ctx
                .credential_owner_or_user_id()
                .ok_or(ProviderError::CredentialRequired)?;
            let repo = self.credential_repo_or(ctx.credential_repo)?;
            let (client, token, _) = self
                .media_credential_with_repo(repo, owner, &base.server_id)
                .await?;
            let play = client
                .play_info(
                    &token,
                    &decoded_target.item_guid,
                    decoded_target.media_guid.as_deref(),
                )
                .await?;
            let nested_parent =
                play.item
                    .parent_guid
                    .filter(|value| !value.is_empty())
                    .map(|parent| {
                        ProviderTarget::fnos_media(parent, None, decoded_target.library_guid)
                    });
            if let Some(nested_parent) = nested_parent.as_ref() {
                media = self
                    .scan_playlist_media(ctx, playlist, Some(nested_parent), target, 200, play_mode)
                    .await?;
            }
        }
        let selected = match play_mode {
            PlayMode::Sequential | PlayMode::RepeatAll => {
                let current = media.iter().position(|item| &item.target == target);
                current.and_then(|index| {
                    media.get(index + 1).or_else(|| {
                        (play_mode == PlayMode::RepeatAll)
                            .then(|| media.first())
                            .flatten()
                    })
                })
            }
            PlayMode::Shuffle => media
                .iter()
                .filter(|item| &item.target != target)
                .take(DYNAMIC_MAX_SHUFFLE_ITEMS)
                .collect::<Vec<_>>()
                .choose(&mut rand::rng())
                .copied(),
            PlayMode::RepeatOne => None,
        };
        selected
            .map(|item| match &base.source {
                FnosPlaylistSource::Files { .. } => Self::next_item(base, item),
                FnosPlaylistSource::MediaLibrary { .. }
                | FnosPlaylistSource::Favorites { .. }
                | FnosPlaylistSource::History => Self::next_media_item(base, item),
            })
            .transpose()
    }

    async fn browse_path(
        &self,
        _ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: Option<&ProviderTarget>,
    ) -> Result<Vec<DynamicBrowsePathSegment>, ProviderError> {
        let base =
            Self::playlist_config(playlist.source_config.as_ref().ok_or_else(|| {
                ProviderError::InvalidConfig("Missing source_config".to_string())
            })?)?;
        match &base.source {
            FnosPlaylistSource::Files { .. } => {
                let Some(relative) = decode_file_target(target)? else {
                    return Ok(Vec::new());
                };
                let mut current = String::new();
                relative
                    .split('/')
                    .filter(|segment| !segment.is_empty())
                    .map(|segment| {
                        if !current.is_empty() {
                            current.push('/');
                        }
                        current.push_str(segment);
                        Ok(DynamicBrowsePathSegment {
                            name: segment.to_string(),
                            target: encode_file_target(&current)?,
                        })
                    })
                    .collect()
            }
            FnosPlaylistSource::MediaLibrary { .. }
            | FnosPlaylistSource::Favorites { .. }
            | FnosPlaylistSource::History => {
                let Some(target) = decode_media_target(target)? else {
                    return Ok(Vec::new());
                };
                Ok(vec![DynamicBrowsePathSegment {
                    name: target.item_guid.clone(),
                    target: ProviderTarget::fnos_media(target.item_guid, None, target.library_guid),
                }])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_subtitles_match_media_stem() {
        assert!(related_file_stem("Movie.mkv", "movie.zh-CN.ass"));
        assert!(related_file_stem("movie.mp4", "MOVIE_en.srt"));
        assert!(!related_file_stem("movie.mkv", "movie2.srt"));
        assert_eq!(file_subtitle_format("movie.zh-CN.ASS"), Some("ASS"));
        assert_eq!(file_subtitle_format("movie.txt"), None);
        assert_eq!(
            file_subtitle_language("vol1/Videos/Movie.mkv", "Movie.zh-CN.ass"),
            "zh-CN"
        );
    }

    #[test]
    fn maps_media_and_folder_targets() {
        let folder = map_directory_item(
            "vol1/1000/Videos",
            FnosFile {
                name: "Series".to_string(),
                path: "vol1/1000/Videos/Series".to_string(),
                size: None,
                modified_at: Some(1),
                created_at: None,
                is_dir: true,
                storage_id: None,
            },
        )
        .expect("test operation should succeed");
        assert_eq!(folder.item_type, ItemType::Playlist);
        assert_eq!(
            decode_file_target(Some(&folder.target))
                .expect("test operation should succeed")
                .as_deref(),
            Some("Series")
        );
    }

    #[test]
    fn thumbnail_credentials_follow_fnos_resource_kind() {
        let media = PlaybackFnosMedia::MediaOriginalRefresh {
            credential_owner_id: "42".to_string(),
            server_id: "server".to_string(),
            media_guid: "media".to_string(),
            path: "vol1/1000/Media/Episode.mkv".to_string(),
        };
        let file = PlaybackFnosMedia::FileRefresh {
            credential_owner_id: "42".to_string(),
            server_id: "server".to_string(),
            path: "vol1/1000/Media/Episode.mkv".to_string(),
        };

        assert_eq!(
            fnos_thumbnail_credentials(&media)
                .expect("media thumbnail credentials should resolve")
                .2,
            FnosThumbnailCredentialKind::Media
        );
        assert_eq!(
            fnos_thumbnail_credentials(&file)
                .expect("file thumbnail credentials should resolve")
                .2,
            FnosThumbnailCredentialKind::WebDav
        );
    }

    #[test]
    fn versioning_uses_webdav_for_media_original_and_media_auth_for_metadata() {
        let mut result = PlaybackResult {
            playback_infos: HashMap::from([(
                "direct".to_string(),
                PlaybackInfo {
                    thumbnail: Some("https://nas.example/media/poster.webp".to_string()),
                    medias: vec![PlaybackMedia {
                        name: "Original".to_string(),
                        format: "mkv".to_string(),
                        expire_at: None,
                        metadata: None,
                        p2p_swarm_id: None,
                        provider: PlaybackMediaProvider::Fnos(
                            PlaybackFnosMedia::MediaOriginalRefresh {
                                credential_owner_id: "42".to_string(),
                                server_id: "server".to_string(),
                                media_guid: "media".to_string(),
                                path: "vol1/1000/Media/Episode.mkv".to_string(),
                            },
                        ),
                    }],
                    default_media_index: Some(0),
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            )]),
            default_mode: "direct".to_string(),
            provider: crate::models::SourceProvider::Fnos,
            provider_instance_name: None,
            duration_seconds: None,
            playback_kind: Some(crate::models::PlaybackKind::Regular),
            metadata: None,
        };

        mark_fnos_playback_resources(
            &mut result,
            "version",
            123,
            fnos_route_selection(crate::models::PlaybackProxyMode::Auto),
        );

        assert!(matches!(
            &result.playback_infos["proxy_direct"].medias[0].provider,
            PlaybackMediaProvider::Fnos(PlaybackFnosMedia::Proxy {
                resource: FnosProxyResource::MediaOriginal { media_guid, path },
                ..
            }) if media_guid == "media" && path == "vol1/1000/Media/Episode.mkv"
        ));
    }

    #[test]
    fn versioning_preserves_fnos_credentials_and_proxies_subtitles() {
        let mut result = PlaybackResult {
            playback_infos: HashMap::from([(
                "direct".to_string(),
                PlaybackInfo {
                    thumbnail: Some("https://nas.example/poster.jpg".to_string()),
                    medias: vec![PlaybackMedia {
                        name: "Original".to_string(),
                        format: "hls".to_string(),
                        expire_at: None,
                        metadata: None,
                        p2p_swarm_id: None,
                        provider: PlaybackMediaProvider::Fnos(PlaybackFnosMedia::MediaRefresh {
                            credential_owner_id: "42".to_string(),
                            server_id: "server".to_string(),
                            media_guid: "media".to_string(),
                            quality_index: Some(1),
                        }),
                    }],
                    default_media_index: Some(0),
                    subtitles: vec![PlaybackSubtitle {
                        name: "English".to_string(),
                        language: "en".to_string(),
                        format: "srt".to_string(),
                        p2p_swarm_id: None,
                        provider: PlaybackSubtitleProvider::Fnos(PlaybackFnosSubtitle::Direct {
                            url: "https://nas.example/subtitle.srt".to_string(),
                            headers: HashMap::from([(
                                "Authorization".to_string(),
                                "Bearer secret".to_string(),
                            )]),
                            expire_at: None,
                        }),
                    }],
                    default_subtitle_index: Some(0),
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            )]),
            default_mode: "direct".to_string(),
            provider: crate::models::SourceProvider::Fnos,
            provider_instance_name: None,
            duration_seconds: None,
            playback_kind: Some(crate::models::PlaybackKind::Regular),
            metadata: Some(PlaybackMetadata::Fnos(FnosPlaybackMetadata::Media(
                FnosMediaPlaybackMetadata {
                    item_guid: "item".to_string(),
                    media_guid: "media".to_string(),
                    ..FnosMediaPlaybackMetadata::default()
                },
            ))),
        };

        mark_fnos_playback_resources(
            &mut result,
            "version",
            123,
            fnos_route_selection(crate::models::PlaybackProxyMode::Auto),
        );

        let info = &result.playback_infos["proxy_direct"];
        assert!(matches!(
            &info.medias[0].provider,
            PlaybackMediaProvider::Fnos(PlaybackFnosMedia::Proxy {
                version,
                credential_owner_id,
                server_id,
                resource: FnosProxyResource::Media {
                    media_guid,
                    quality_index: Some(1),
                },
                ..
            }) if version == "version" && credential_owner_id == "42" && server_id == "server" && media_guid == "media"
        ));
        assert!(matches!(
            &info.subtitles[0].provider,
            PlaybackSubtitleProvider::Fnos(PlaybackFnosSubtitle::Proxy {
                version,
                subtitle_index: 0,
                headers,
                ..
            }) if version == "version" && headers.get("Authorization").is_some()
        ));
    }

    #[test]
    fn filtered_direct_media_remaps_the_default_index() {
        assert_eq!(
            remap_filtered_default_index(&[(0, ()), (2, ())], Some(2)),
            Some(1)
        );
        assert_eq!(
            remap_filtered_default_index(&[(0, ()), (2, ())], Some(1)),
            Some(0)
        );
    }
}
