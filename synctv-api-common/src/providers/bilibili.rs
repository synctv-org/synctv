//! Bilibili API Implementation
//!
//! Unified implementation for all Bilibili API operations.
//! Used by both HTTP and gRPC handlers.

use source_config_proto::bilibili_playlist_source_config::Source as PlaylistSource;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use synctv_core::models::{
    resolve_provider_instance_binding, BilibiliHistoryType, BilibiliPgcTimelineType,
    CredentialProviderInstanceName, UserId,
};
use synctv_core::provider::{
    BilibiliMatchRequest, BilibiliMatchedResource, BilibiliParseLivePageRequest,
    BilibiliParsePgcPageRequest, BilibiliParseVideoPageRequest, BilibiliProvider,
    BilibiliQrLoginStatus, BilibiliSmsLoginTokenCodec, BilibiliUserInfoRequest, ExecutionControl,
    ProviderAccessService, ProviderError,
};
use synctv_proto::providers::bilibili::{
    BindInfo, CheckQrRequest, FavoriteFolder, FollowedPgcSeason, GetBindsResponse, HistoryItem,
    ListFavoriteFoldersRequest, ListFavoriteFoldersResponse, ListFollowedPgcRequest,
    ListFollowedPgcResponse, ListHistoryRequest, ListHistoryResponse, ListLiveAreasRequest,
    ListLiveAreasResponse, ListPgcSeasonsRequest, ListPgcSeasonsResponse, ListPgcTimelineRequest,
    ListPgcTimelineResponse, LiveArea, LoginQrRequest, LoginSmsRequest, LoginSmsResponse,
    LogoutRequest, LogoutResponse, ParseCandidate, ParseRequest, ParseResponse, PgcSeason,
    PgcTimelineItem, QrCodeResponse, QrStatusResponse, SendSmsRequest, SendSmsResponse,
    StartSmsLoginRequest, StartSmsLoginResponse, UserInfoRequest, UserInfoResponse,
};
use synctv_proto::source_config as source_config_proto;
use synctv_realtime::fanout::RealtimeEventService;

use super::ProviderApiRuntime;
use super::{
    provider_instance_name_for_provider, provider_instance_name_for_response,
    publish_provider_credential_changed,
};

fn checked_u32(value: i32, field: &str) -> Result<u32, ProviderError> {
    u32::try_from(value)
        .map_err(|_| ProviderError::InvalidConfig(format!("Bilibili {field} must be non-negative")))
}

/// Bilibili API implementation
///
/// Contains all business logic for Bilibili operations.
/// Methods accept grpc-generated request types and return grpc-generated response types.
#[derive(Clone)]
pub struct BilibiliApiImpl {
    provider: Arc<BilibiliProvider>,
    access_service: Arc<dyn ProviderAccessService>,
    event_service: Arc<dyn RealtimeEventService>,
    sms_login_token_codec: Arc<BilibiliSmsLoginTokenCodec>,
    qr_login_status_cache: Arc<moka::sync::Cache<String, i32>>,
}

struct ResolvedBilibiliCredential {
    cookies: HashMap<String, String>,
    provider_instance_name: Option<String>,
}

const QR_LOGIN_STATUS_CACHE_TTL_SECONDS: u64 = 2;

fn bilibili_media_parse_candidate(
    video: synctv_core::provider::BilibiliVideoInfo,
    page_title: &str,
    actors: &[String],
) -> ParseCandidate {
    use source_config_proto::bilibili_media_source_config::Source;
    use source_config_proto::media_source_config::Provider;

    let source = if video.r#live {
        Source::Live(source_config_proto::BilibiliLiveSourceConfig {
            room_id: video.cid,
            shared: false,
        })
    } else if video.epid > 0 {
        Source::Pgc(source_config_proto::BilibiliPgcSourceConfig {
            epid: video.epid,
            cid: video.cid,
            shared: false,
        })
    } else {
        Source::Video(source_config_proto::BilibiliVideoSourceConfig {
            bvid: video.bvid,
            aid: (video.aid > 0).then_some(video.aid),
            cid: video.cid,
            shared: false,
        })
    };
    let title = if video.name.trim().is_empty() {
        page_title.to_string()
    } else {
        video.name
    };
    ParseCandidate {
        title,
        description: String::new(),
        cover: video.cover_image,
        actors: actors.to_vec(),
        duration_seconds: (video.duration_seconds > 0).then_some(video.duration_seconds),
        part_number: (video.page > 0).then_some(video.page),
        width: (video.width > 0).then_some(video.width),
        height: (video.height > 0).then_some(video.height),
        source_config: Some(
            synctv_proto::providers::bilibili::parse_candidate::SourceConfig::Media(
                source_config_proto::MediaSourceConfig {
                    provider: Some(Provider::Bilibili(
                        source_config_proto::BilibiliMediaSourceConfig {
                            source: Some(source),
                        },
                    )),
                },
            ),
        ),
    }
}

fn bilibili_playlist_parse_candidate(
    title: String,
    description: String,
    cover: String,
    source: source_config_proto::bilibili_playlist_source_config::Source,
) -> ParseCandidate {
    use source_config_proto::playlist_source_config::Provider;

    ParseCandidate {
        title,
        description,
        cover,
        actors: Vec::new(),
        duration_seconds: None,
        part_number: None,
        width: None,
        height: None,
        source_config: Some(
            synctv_proto::providers::bilibili::parse_candidate::SourceConfig::Playlist(
                source_config_proto::PlaylistSourceConfig {
                    provider: Some(Provider::Bilibili(
                        source_config_proto::BilibiliPlaylistSourceConfig {
                            source: Some(source),
                            shared: false,
                        },
                    )),
                },
            ),
        ),
    }
}

const fn bilibili_qr_status_to_proto(status: BilibiliQrLoginStatus) -> i32 {
    match status {
        BilibiliQrLoginStatus::Unknown => 0,
        BilibiliQrLoginStatus::Expired => 1,
        BilibiliQrLoginStatus::NotScanned => 2,
        BilibiliQrLoginStatus::Scanned => 3,
        BilibiliQrLoginStatus::Success => 4,
    }
}

impl BilibiliApiImpl {
    pub fn new_with_runtime(
        provider: Arc<BilibiliProvider>,
        sms_login_secret: &[u8],
        runtime: ProviderApiRuntime,
    ) -> Result<Self, synctv_core::provider::ProviderError> {
        Ok(Self {
            provider,
            access_service: runtime.access_service,
            event_service: runtime.event_service,
            sms_login_token_codec: Arc::new(BilibiliProvider::sms_login_token_codec_from_secret(
                sms_login_secret,
            )?),
            qr_login_status_cache: Arc::new(
                moka::sync::Cache::builder()
                    .max_capacity(10_000)
                    .time_to_live(Duration::from_secs(QR_LOGIN_STATUS_CACHE_TTL_SECONDS))
                    .build(),
            ),
        })
    }

    fn resolve_effective_instance_name(
        requested_instance_name: Option<&str>,
        credential_instance_name: CredentialProviderInstanceName<'_>,
    ) -> Result<Option<String>, synctv_core::provider::ProviderError> {
        let requested_instance_name = provider_instance_name_for_provider(requested_instance_name)?;
        resolve_provider_instance_binding(requested_instance_name, credential_instance_name)
            .map_err(|error| synctv_core::provider::ProviderError::InvalidConfig(error.to_string()))
    }

    /// Resolve the user's single global Bilibili credential.
    async fn resolve_credential(
        &self,
        caller_user_id: &UserId,
        request_context: Option<&ExecutionControl>,
    ) -> Result<Option<ResolvedBilibiliCredential>, synctv_core::provider::ProviderError> {
        let access = self
            .access_service
            .bilibili_access(*caller_user_id, request_context)
            .await?;
        Ok(access.authenticated.then_some(ResolvedBilibiliCredential {
            cookies: access.cookies,
            provider_instance_name: access.provider_instance_name,
        }))
    }

    async fn publish_login_change(
        &self,
        caller_user_id: &UserId,
        server_id: &str,
    ) -> Result<(), synctv_core::provider::ProviderError> {
        self.access_service
            .invalidate(
                *caller_user_id,
                synctv_core::provider::BilibiliProvider::NAME,
                server_id,
            )
            .await?;
        publish_provider_credential_changed(
            &self.event_service,
            *caller_user_id,
            synctv_core::provider::BilibiliProvider::NAME,
            server_id,
        );

        Ok(())
    }

    pub async fn parse_with_context(
        &self,
        caller_user_id: &UserId,
        req: ParseRequest,
        requested_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<ParseResponse, synctv_core::provider::ProviderError> {
        let credential = self
            .resolve_credential(caller_user_id, request_context)
            .await?;
        let effective_instance_name = Self::resolve_effective_instance_name(
            requested_instance_name,
            credential.as_ref().map_or(
                CredentialProviderInstanceName::NotCredentialBacked,
                |credential| {
                    CredentialProviderInstanceName::CredentialBacked(
                        credential.provider_instance_name.as_deref(),
                    )
                },
            ),
        )?;
        let cookies = credential
            .as_ref()
            .map_or_else(HashMap::new, |credential| credential.cookies.clone());

        // Step 1: Match URL
        let match_resp = self
            .provider
            .r#match_with_context(
                BilibiliMatchRequest {
                    url: req.url.clone(),
                },
                effective_instance_name.as_deref(),
                request_context,
            )
            .await?;

        let normalized_url = match_resp.normalized_url;
        let mut candidates = Vec::new();
        match match_resp.resource {
            BilibiliMatchedResource::Video { bvid, aid, page } => {
                let page_info = self
                    .provider
                    .parse_video_page_with_context(
                        BilibiliParseVideoPageRequest {
                            cookies: cookies.clone(),
                            bvid: bvid.clone(),
                            aid,
                            sections: true,
                        },
                        effective_instance_name.as_deref(),
                        request_context,
                    )
                    .await?;
                let mut videos = page_info.videos;
                if page > 0 {
                    videos.sort_by_key(|video| video.page != page);
                }
                candidates.extend(videos.into_iter().map(|video| {
                    bilibili_media_parse_candidate(video, &page_info.title, &page_info.actors)
                }));
                if candidates.len() > 1 {
                    candidates.push(bilibili_playlist_parse_candidate(
                        page_info.title.clone(),
                        "All video parts".to_string(),
                        page_info.cover.clone(),
                        PlaylistSource::VideoParts(
                            source_config_proto::BilibiliVideoPartsPlaylistSource {
                                bvid: bvid.clone(),
                                aid: (aid > 0).then_some(aid),
                            },
                        ),
                    ));
                }
                if let Some(collection) = page_info.collection {
                    candidates.push(bilibili_playlist_parse_candidate(
                        collection.title,
                        "Bilibili collection".to_string(),
                        collection.cover,
                        PlaylistSource::CollectionVideos(
                            source_config_proto::BilibiliCollectionVideosPlaylistSource {
                                mid: collection.mid,
                                season_id: collection.season_id,
                            },
                        ),
                    ));
                }
            }
            resource @ (BilibiliMatchedResource::PgcEpisode { .. }
            | BilibiliMatchedResource::PgcSeason { .. }) => {
                let (episode_request, episode_id) = match resource {
                    BilibiliMatchedResource::PgcEpisode { episode_id } => (true, episode_id),
                    BilibiliMatchedResource::PgcSeason { season_id } => (false, season_id),
                    _ => unreachable!(),
                };
                let page_info = self
                    .provider
                    .parse_pgc_page_with_context(
                        BilibiliParsePgcPageRequest {
                            cookies: cookies.clone(),
                            ssid: if episode_request {
                                Default::default()
                            } else {
                                episode_id
                            },
                            epid: if episode_request {
                                episode_id
                            } else {
                                Default::default()
                            },
                        },
                        effective_instance_name.as_deref(),
                        request_context,
                    )
                    .await?;
                let season_id = if page_info.season_id > 0 {
                    page_info.season_id
                } else if episode_request {
                    0
                } else {
                    episode_id
                };
                let season_candidate = (season_id > 0).then(|| {
                    bilibili_playlist_parse_candidate(
                        page_info.title.clone(),
                        "Bilibili PGC season".to_string(),
                        page_info.cover.clone(),
                        PlaylistSource::PgcSeason(
                            source_config_proto::BilibiliPgcSeasonPlaylistSource { season_id },
                        ),
                    )
                });
                let mut videos = page_info.videos;
                if episode_request {
                    videos.sort_by_key(|video| video.epid != episode_id);
                } else if let Some(candidate) = season_candidate.clone() {
                    candidates.push(candidate);
                }
                candidates.extend(videos.into_iter().map(|video| {
                    bilibili_media_parse_candidate(video, &page_info.title, &page_info.actors)
                }));
                if episode_request {
                    candidates.extend(season_candidate);
                }
            }
            BilibiliMatchedResource::Live { room_id } => {
                let page_info = self
                    .provider
                    .parse_live_page_with_context(
                        BilibiliParseLivePageRequest {
                            cookies: cookies.clone(),
                            room_id,
                        },
                        effective_instance_name.as_deref(),
                        request_context,
                    )
                    .await?;
                candidates.extend(page_info.videos.into_iter().map(|video| {
                    bilibili_media_parse_candidate(video, &page_info.title, &page_info.actors)
                }));
            }
            BilibiliMatchedResource::LiveRecommended => {
                candidates.push(bilibili_playlist_parse_candidate(
                    "Bilibili Live".to_string(),
                    "Recommended live rooms".to_string(),
                    String::new(),
                    PlaylistSource::LiveRecommended(
                        source_config_proto::BilibiliLiveRecommendedPlaylistSource {},
                    ),
                ));
            }
            BilibiliMatchedResource::LiveArea {
                parent_area_id,
                area_id,
            } => {
                candidates.push(bilibili_playlist_parse_candidate(
                    format!("Bilibili Live Area {area_id}"),
                    "Live rooms in this area".to_string(),
                    String::new(),
                    PlaylistSource::LiveArea(source_config_proto::BilibiliLiveAreaPlaylistSource {
                        parent_area_id,
                        area_id,
                    }),
                ));
            }
            BilibiliMatchedResource::UpVideos { mid } => {
                candidates.push(bilibili_playlist_parse_candidate(
                    format!("UP {mid} videos"),
                    "Bilibili UP submissions".to_string(),
                    String::new(),
                    PlaylistSource::UpVideos(source_config_proto::BilibiliUpVideosPlaylistSource {
                        mid,
                        keyword: String::new(),
                    }),
                ));
            }
            BilibiliMatchedResource::FavoriteVideos { media_id } => {
                candidates.push(bilibili_playlist_parse_candidate(
                    format!("Favorite {media_id}"),
                    "Bilibili favorite videos".to_string(),
                    String::new(),
                    PlaylistSource::FavoriteVideos(
                        source_config_proto::BilibiliFavoriteVideosPlaylistSource { media_id },
                    ),
                ));
            }
            BilibiliMatchedResource::CollectionVideos { mid, season_id } => {
                candidates.push(bilibili_playlist_parse_candidate(
                    format!("Collection {season_id}"),
                    "Bilibili collection".to_string(),
                    String::new(),
                    PlaylistSource::CollectionVideos(
                        source_config_proto::BilibiliCollectionVideosPlaylistSource {
                            mid,
                            season_id,
                        },
                    ),
                ));
            }
            BilibiliMatchedResource::SeriesVideos { mid, series_id } => {
                candidates.push(bilibili_playlist_parse_candidate(
                    format!("Series {series_id}"),
                    "Bilibili series".to_string(),
                    String::new(),
                    PlaylistSource::SeriesVideos(
                        source_config_proto::BilibiliSeriesVideosPlaylistSource { mid, series_id },
                    ),
                ));
            }
            BilibiliMatchedResource::WatchLater => {
                candidates.push(bilibili_playlist_parse_candidate(
                    "Watch later".to_string(),
                    "Bilibili watch later".to_string(),
                    String::new(),
                    PlaylistSource::WatchLater(
                        source_config_proto::BilibiliWatchLaterPlaylistSource {},
                    ),
                ));
            }
        }

        Ok(ParseResponse {
            normalized_url,
            candidates,
        })
    }

    pub async fn login_qr_with_context(
        &self,
        _req: LoginQrRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<QrCodeResponse, synctv_core::provider::ProviderError> {
        let resp = self
            .provider
            .new_qr_code_with_context(instance_name, request_context)
            .await?;

        Ok(QrCodeResponse {
            url: resp.url,
            key: resp.key,
        })
    }

    pub async fn list_live_areas_with_context(
        &self,
        _req: ListLiveAreasRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<ListLiveAreasResponse, synctv_core::provider::ProviderError> {
        let areas = self
            .provider
            .list_live_areas_with_context(instance_name, request_context)
            .await?
            .into_iter()
            .map(|area| LiveArea {
                id: area.id,
                parent_id: area.parent_id,
                name: area.name,
                parent_name: area.parent_name,
                picture: area.picture,
                hot: area.hot,
            })
            .collect();
        Ok(ListLiveAreasResponse { areas })
    }

    pub async fn list_favorite_folders_with_context(
        &self,
        caller_user_id: &UserId,
        _req: ListFavoriteFoldersRequest,
        requested_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<ListFavoriteFoldersResponse, synctv_core::provider::ProviderError> {
        use source_config_proto::bilibili_playlist_source_config::Source;
        use source_config_proto::playlist_source_config::Provider;

        let credential = self
            .resolve_credential(caller_user_id, request_context)
            .await?
            .ok_or(synctv_core::provider::ProviderError::CredentialRequired)?;
        let effective_instance_name = Self::resolve_effective_instance_name(
            requested_instance_name,
            CredentialProviderInstanceName::CredentialBacked(
                credential.provider_instance_name.as_deref(),
            ),
        )?;
        let folders = self
            .provider
            .list_favorite_folders_with_context(
                credential.cookies,
                effective_instance_name.as_deref(),
                request_context,
            )
            .await?
            .into_iter()
            .map(|folder| FavoriteFolder {
                media_id: folder.media_id,
                title: folder.title,
                media_count: folder.media_count,
                private: folder.private,
                default_folder: folder.default_folder,
                source_config: Some(source_config_proto::PlaylistSourceConfig {
                    provider: Some(Provider::Bilibili(
                        source_config_proto::BilibiliPlaylistSourceConfig {
                            source: Some(Source::FavoriteVideos(
                                source_config_proto::BilibiliFavoriteVideosPlaylistSource {
                                    media_id: folder.media_id,
                                },
                            )),
                            shared: false,
                        },
                    )),
                }),
            })
            .collect();
        Ok(ListFavoriteFoldersResponse { folders })
    }

    pub async fn list_followed_pgc_with_context(
        &self,
        caller_user_id: &UserId,
        req: ListFollowedPgcRequest,
        requested_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<ListFollowedPgcResponse, synctv_core::provider::ProviderError> {
        use source_config_proto::bilibili_playlist_source_config::Source;
        use source_config_proto::playlist_source_config::Provider;

        if !matches!(req.r#type, 1 | 2) {
            return Err(synctv_core::provider::ProviderError::InvalidConfig(
                "Bilibili PGC follow type must be anime or cinema".to_string(),
            ));
        }
        let credential = self
            .resolve_credential(caller_user_id, request_context)
            .await?
            .ok_or(synctv_core::provider::ProviderError::CredentialRequired)?;
        let effective_instance_name = Self::resolve_effective_instance_name(
            requested_instance_name,
            CredentialProviderInstanceName::CredentialBacked(
                credential.provider_instance_name.as_deref(),
            ),
        )?;
        let result = self
            .provider
            .list_followed_pgc_with_context(
                credential.cookies,
                checked_u32(req.r#type, "PGC follow type")?,
                req.page,
                req.page_size,
                effective_instance_name.as_deref(),
                request_context,
            )
            .await?;
        let seasons = result
            .items
            .into_iter()
            .map(|season| FollowedPgcSeason {
                season_id: season.season_id,
                title: season.title,
                cover: season.cover,
                description: season.description,
                latest_episode: season.latest_episode,
                source_config: Some(source_config_proto::PlaylistSourceConfig {
                    provider: Some(Provider::Bilibili(
                        source_config_proto::BilibiliPlaylistSourceConfig {
                            source: Some(Source::PgcSeason(
                                source_config_proto::BilibiliPgcSeasonPlaylistSource {
                                    season_id: season.season_id,
                                },
                            )),
                            shared: false,
                        },
                    )),
                }),
            })
            .collect();
        Ok(ListFollowedPgcResponse {
            seasons,
            total: result.total,
            has_more: result.has_more,
        })
    }

    pub async fn list_history_with_context(
        &self,
        caller_user_id: &UserId,
        req: ListHistoryRequest,
        requested_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<ListHistoryResponse, synctv_core::provider::ProviderError> {
        use source_config_proto::bilibili_playlist_source_config::Source;
        use source_config_proto::playlist_source_config::Provider;

        let history_type = match source_config_proto::BilibiliHistoryType::try_from(req.r#type)
            .map_err(|_| {
                synctv_core::provider::ProviderError::InvalidConfig(
                    "Bilibili history type is invalid".to_string(),
                )
            })? {
            source_config_proto::BilibiliHistoryType::All => BilibiliHistoryType::All,
            source_config_proto::BilibiliHistoryType::Archive => BilibiliHistoryType::Archive,
            source_config_proto::BilibiliHistoryType::Live => BilibiliHistoryType::Live,
        };
        let credential = self
            .resolve_credential(caller_user_id, request_context)
            .await?
            .ok_or(synctv_core::provider::ProviderError::CredentialRequired)?;
        let effective_instance_name = Self::resolve_effective_instance_name(
            requested_instance_name,
            CredentialProviderInstanceName::CredentialBacked(
                credential.provider_instance_name.as_deref(),
            ),
        )?;
        let result = self
            .provider
            .list_history_with_context(
                credential.cookies,
                history_type,
                req.cursor.as_deref(),
                req.page_size,
                effective_instance_name.as_deref(),
                request_context,
            )
            .await?;
        let mut items = Vec::with_capacity(result.items.len());
        for item in result.items {
            items.push(HistoryItem {
                title: item.title,
                subtitle: item.subtitle,
                cover: item.cover,
                author: item.author,
                viewed_at: item.viewed_at,
                progress_seconds: item.progress_seconds,
                duration_seconds: item.duration_seconds,
                source_config: Some(
                    crate::impls::client::convert::media_source_config_to_proto(
                        &item.source_config,
                    )
                    .map_err(|error| {
                        synctv_core::provider::ProviderError::Internal(error.to_string())
                    })?,
                ),
            });
        }
        let source = source_config_proto::BilibiliHistoryPlaylistSource { r#type: req.r#type };
        Ok(ListHistoryResponse {
            items,
            cursor: result.cursor,
            has_more: result.has_more,
            source_config: Some(source_config_proto::PlaylistSourceConfig {
                provider: Some(Provider::Bilibili(
                    source_config_proto::BilibiliPlaylistSourceConfig {
                        source: Some(Source::History(source)),
                        shared: false,
                    },
                )),
            }),
        })
    }

    pub async fn list_pgc_timeline_with_context(
        &self,
        caller_user_id: &UserId,
        req: ListPgcTimelineRequest,
        requested_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<ListPgcTimelineResponse, synctv_core::provider::ProviderError> {
        use source_config_proto::bilibili_playlist_source_config::Source;
        use source_config_proto::playlist_source_config::Provider;

        let timeline_type = match source_config_proto::BilibiliPgcTimelineType::try_from(req.r#type)
            .map_err(|_| {
                synctv_core::provider::ProviderError::InvalidConfig(
                    "Bilibili PGC timeline type is invalid".to_string(),
                )
            })? {
            source_config_proto::BilibiliPgcTimelineType::Anime => BilibiliPgcTimelineType::Anime,
            source_config_proto::BilibiliPgcTimelineType::Cinema => BilibiliPgcTimelineType::Cinema,
            source_config_proto::BilibiliPgcTimelineType::Guochuang => {
                BilibiliPgcTimelineType::Guochuang
            }
            source_config_proto::BilibiliPgcTimelineType::Unspecified => {
                return Err(synctv_core::provider::ProviderError::InvalidConfig(
                    "Bilibili PGC timeline type is required".to_string(),
                ));
            }
        };
        let credential = self
            .resolve_credential(caller_user_id, request_context)
            .await?;
        let effective_instance_name = Self::resolve_effective_instance_name(
            requested_instance_name,
            credential.as_ref().map_or(
                CredentialProviderInstanceName::NotCredentialBacked,
                |credential| {
                    CredentialProviderInstanceName::CredentialBacked(
                        credential.provider_instance_name.as_deref(),
                    )
                },
            ),
        )?;
        let cookies = credential.map_or_else(HashMap::new, |credential| credential.cookies);
        let result = self
            .provider
            .list_pgc_timeline_with_context(
                cookies,
                timeline_type,
                req.before_days,
                req.after_days,
                effective_instance_name.as_deref(),
                request_context,
            )
            .await?;
        let mut items = Vec::with_capacity(result.len());
        for item in result {
            items.push(PgcTimelineItem {
                source_config: item
                    .source_config
                    .as_ref()
                    .map(crate::impls::client::convert::media_source_config_to_proto)
                    .transpose()
                    .map_err(|error| {
                        synctv_core::provider::ProviderError::Internal(error.to_string())
                    })?,
                episode_id: item.episode_id,
                season_id: item.season_id,
                title: item.title,
                episode_title: item.episode_title,
                cover: item.cover,
                episode_cover: item.episode_cover,
                publish_at: item.publish_at,
                published: item.published,
                date: item.date,
                day_of_week: item.day_of_week,
                delayed: item.delayed,
                delay_reason: item.delay_reason,
            });
        }
        let source = source_config_proto::BilibiliPgcTimelinePlaylistSource {
            r#type: req.r#type,
            before_days: req.before_days,
            after_days: req.after_days,
        };
        Ok(ListPgcTimelineResponse {
            items,
            source_config: Some(source_config_proto::PlaylistSourceConfig {
                provider: Some(Provider::Bilibili(
                    source_config_proto::BilibiliPlaylistSourceConfig {
                        source: Some(Source::PgcTimeline(source)),
                        shared: false,
                    },
                )),
            }),
        })
    }

    pub async fn list_pgc_seasons_with_context(
        &self,
        caller_user_id: &UserId,
        req: ListPgcSeasonsRequest,
        requested_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<ListPgcSeasonsResponse, synctv_core::provider::ProviderError> {
        use source_config_proto::bilibili_playlist_source_config::Source;
        use source_config_proto::playlist_source_config::Provider;

        let credential = self
            .resolve_credential(caller_user_id, request_context)
            .await?;
        let effective_instance_name = Self::resolve_effective_instance_name(
            requested_instance_name,
            credential.as_ref().map_or(
                CredentialProviderInstanceName::NotCredentialBacked,
                |credential| {
                    CredentialProviderInstanceName::CredentialBacked(
                        credential.provider_instance_name.as_deref(),
                    )
                },
            ),
        )?;
        let cookies = credential.map_or_else(HashMap::new, |credential| credential.cookies);
        let result = self
            .provider
            .list_pgc_seasons_with_context(
                cookies,
                checked_u32(req.r#type, "season type")?,
                req.page,
                req.page_size,
                checked_u32(req.order, "season order")?,
                req.ascending,
                req.finished,
                req.area,
                req.year,
                req.style_id,
                effective_instance_name.as_deref(),
                request_context,
            )
            .await?;
        let seasons = result
            .items
            .into_iter()
            .map(|season| {
                Ok(PgcSeason {
                    source_config: Some(source_config_proto::PlaylistSourceConfig {
                        provider: Some(Provider::Bilibili(
                            source_config_proto::BilibiliPlaylistSourceConfig {
                                source: Some(Source::PgcSeason(
                                    source_config_proto::BilibiliPgcSeasonPlaylistSource {
                                        season_id: season.season_id,
                                    },
                                )),
                                shared: false,
                            },
                        )),
                    }),
                    season_id: season.season_id,
                    media_id: season.media_id,
                    first_episode_id: season.first_episode_id,
                    title: season.title,
                    subtitle: season.subtitle,
                    cover: season.cover,
                    first_episode_cover: season.first_episode_cover,
                    badge: season.badge,
                    progress: season.progress,
                    score: season.score,
                    finished: season.finished,
                    r#type: i32::try_from(season.season_type).map_err(|_| {
                        ProviderError::ApiError(
                            "Bilibili returned a season type outside the supported range"
                                .to_string(),
                        )
                    })?,
                })
            })
            .collect::<Result<Vec<_>, ProviderError>>()?;
        Ok(ListPgcSeasonsResponse {
            seasons,
            total: result.total,
            has_more: result.has_more,
        })
    }

    pub async fn check_qr_with_context(
        &self,
        caller_user_id: &UserId,
        req: CheckQrRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<QrStatusResponse, synctv_core::provider::ProviderError> {
        let cache_key = BilibiliProvider::qr_login_status_cache_key(instance_name, &req.key)?;
        if let Some(status) = self.qr_login_status_cache.get(&cache_key) {
            return Ok(QrStatusResponse { status });
        }

        let resp = self
            .provider
            .check_qr_and_persist_with_context(
                *caller_user_id,
                req.key,
                instance_name,
                request_context,
            )
            .await?;

        if resp.status == BilibiliQrLoginStatus::Success {
            if let Some(server_id) = resp.server_id.as_deref() {
                self.publish_login_change(caller_user_id, server_id).await?;
            }
            self.qr_login_status_cache.invalidate(&cache_key);
        } else {
            self.qr_login_status_cache
                .insert(cache_key, bilibili_qr_status_to_proto(resp.status));
        }

        Ok(QrStatusResponse {
            status: bilibili_qr_status_to_proto(resp.status),
        })
    }

    pub async fn start_sms_login_with_context(
        &self,
        _req: StartSmsLoginRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<StartSmsLoginResponse, synctv_core::provider::ProviderError> {
        let started = self
            .provider
            .start_sms_login_session_with_context(
                &self.sms_login_token_codec,
                instance_name,
                request_context,
            )
            .await?;

        Ok(StartSmsLoginResponse {
            session_token: started.session_token,
            gt: started.gt,
            challenge: started.challenge,
            expires_at: started.expires_at,
        })
    }

    pub async fn send_sms_with_context(
        &self,
        req: SendSmsRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<SendSmsResponse, synctv_core::provider::ProviderError> {
        let updated = self
            .provider
            .send_sms_with_session_context(
                &self.sms_login_token_codec,
                &req.session_token,
                req.phone,
                &req.validate,
                instance_name,
                request_context,
            )
            .await?;

        Ok(SendSmsResponse {
            session_token: updated.session_token,
            expires_at: updated.expires_at,
        })
    }

    pub async fn login_sms_with_context(
        &self,
        caller_user_id: &UserId,
        req: LoginSmsRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<LoginSmsResponse, synctv_core::provider::ProviderError> {
        let resp = self
            .provider
            .login_with_sms_session_context(
                *caller_user_id,
                &self.sms_login_token_codec,
                &req.session_token,
                req.code,
                instance_name,
                request_context,
            )
            .await?;
        self.publish_login_change(caller_user_id, &resp.server_id)
            .await?;

        Ok(LoginSmsResponse {})
    }

    pub async fn get_user_info_with_context(
        &self,
        caller_user_id: &UserId,
        _req: UserInfoRequest,
        requested_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<UserInfoResponse, synctv_core::provider::ProviderError> {
        let Some(credential) = self
            .resolve_credential(caller_user_id, request_context)
            .await?
        else {
            return Ok(UserInfoResponse {
                is_login: false,
                user_id: 0,
                username: String::new(),
                face: String::new(),
                is_vip: false,
            });
        };
        let effective_instance_name = Self::resolve_effective_instance_name(
            requested_instance_name,
            CredentialProviderInstanceName::CredentialBacked(
                credential.provider_instance_name.as_deref(),
            ),
        )?;

        let info_req = BilibiliUserInfoRequest {
            cookies: credential.cookies,
        };

        let resp = self
            .provider
            .user_info_with_context(
                info_req,
                effective_instance_name.as_deref(),
                request_context,
            )
            .await?;

        Ok(UserInfoResponse {
            is_login: resp.is_login,
            user_id: resp.user_id,
            username: resp.username,
            face: resp.face,
            is_vip: resp.is_vip,
        })
    }

    /// Logout and delete stored credential
    pub async fn logout(
        &self,
        caller_user_id: &UserId,
        _req: LogoutRequest,
    ) -> Result<LogoutResponse, synctv_core::provider::ProviderError> {
        let server_id = BilibiliProvider::credential_server_id();
        if self.provider.delete_credential(*caller_user_id).await? {
            self.access_service
                .invalidate(
                    *caller_user_id,
                    synctv_core::provider::BilibiliProvider::NAME,
                    &server_id,
                )
                .await?;
            publish_provider_credential_changed(
                &self.event_service,
                *caller_user_id,
                synctv_core::provider::BilibiliProvider::NAME,
                &server_id,
            );
        }

        Ok(LogoutResponse {
            message: "Logout successful".to_string(),
        })
    }

    pub async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<GetBindsResponse, crate::impls::ApiError> {
        let binds = self
            .provider
            .list_binds(*caller_user_id, instance_name)
            .await?
            .into_iter()
            .map(|bind| BindInfo {
                id: bind.id.to_string(),
                server_id: bind.server_id,
                created_at: bind.created_at,
                provider_instance_name: provider_instance_name_for_response(
                    bind.provider_instance_name,
                ),
            })
            .collect();

        Ok(GetBindsResponse { binds })
    }
}

#[cfg(test)]
mod parse_candidate_tests {
    use super::*;

    #[test]
    fn media_parse_candidate_contains_typed_bilibili_source_config() {
        let candidate = bilibili_media_parse_candidate(
            synctv_core::provider::BilibiliVideoInfo {
                bvid: "BV1typed".to_string(),
                aid: 123,
                cid: 456,
                epid: 0,
                page: 2,
                name: "Part 2".to_string(),
                cover_image: "https://example.com/cover.jpg".to_string(),
                r#live: false,
                duration_seconds: 90,
                width: 1920,
                height: 1080,
            },
            "Typed video",
            &["Creator".to_string()],
        );

        let Some(synctv_proto::providers::bilibili::parse_candidate::SourceConfig::Media(config)) =
            candidate.source_config
        else {
            panic!("candidate should contain a media source config");
        };
        let Some(source_config_proto::media_source_config::Provider::Bilibili(config)) =
            config.provider
        else {
            panic!("candidate should contain a Bilibili source config");
        };
        let Some(source_config_proto::bilibili_media_source_config::Source::Video(video)) =
            config.source
        else {
            panic!("candidate should contain a Bilibili video source");
        };
        assert_eq!(video.bvid, "BV1typed");
        assert_eq!(video.aid, Some(123));
        assert_eq!(video.cid, 456);
        assert!(!video.shared);
        assert_eq!(candidate.part_number, Some(2));
    }

    #[test]
    fn playlist_parse_candidate_contains_typed_video_parts_source_config() {
        let candidate = bilibili_playlist_parse_candidate(
            "Typed video".to_string(),
            "All video parts".to_string(),
            String::new(),
            source_config_proto::bilibili_playlist_source_config::Source::VideoParts(
                source_config_proto::BilibiliVideoPartsPlaylistSource {
                    bvid: "BV1typed".to_string(),
                    aid: Some(123),
                },
            ),
        );

        let Some(synctv_proto::providers::bilibili::parse_candidate::SourceConfig::Playlist(
            config,
        )) = candidate.source_config
        else {
            panic!("candidate should contain a playlist source config");
        };
        let Some(source_config_proto::playlist_source_config::Provider::Bilibili(config)) =
            config.provider
        else {
            panic!("candidate should contain a Bilibili playlist source config");
        };
        let Some(source_config_proto::bilibili_playlist_source_config::Source::VideoParts(source)) =
            config.source
        else {
            panic!("candidate should contain a video-parts source");
        };
        assert_eq!(source.bvid, "BV1typed");
        assert_eq!(source.aid, Some(123));
        assert!(!config.shared);
    }
}

#[cfg(test)]
mod tests {
    use super::{BilibiliApiImpl, ProviderApiRuntime};
    use std::collections::HashMap;
    use std::sync::Arc;
    use synctv_core::credential_encryption::CredentialEncryption;
    use synctv_core::models::{
        CredentialProviderInstanceName, NewProviderInstance, SignupMethod, User,
    };
    use synctv_core::provider::BilibiliProvider;
    use synctv_core::repository::{
        ProviderInstanceRepository, UserProviderCredentialRepository, UserRepository,
    };
    use synctv_core_testing::create_test_pool;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn provider_ok<T>(result: Result<T, synctv_core::provider::ProviderError>) -> TestResult<T> {
        result.map_err(|error| test_error(error.to_string()))
    }

    fn provider_err<T>(
        result: Result<T, synctv_core::provider::ProviderError>,
    ) -> TestResult<synctv_core::provider::ProviderError> {
        match result {
            Ok(_) => Err(test_error("expected provider error")),
            Err(error) => Ok(error),
        }
    }

    fn core_ok<T>(result: synctv_core::Result<T>) -> TestResult<T> {
        result.map_err(|error| test_error(error.to_string()))
    }

    fn api_ok<T>(result: Result<T, crate::impls::ApiError>) -> TestResult<T> {
        result.map_err(|error| test_error(format!("{error:?}")))
    }

    fn test_encryption() -> TestResult<CredentialEncryption> {
        Ok(CredentialEncryption::new(&[0x42; 32])?)
    }

    fn test_sms_login_secret() -> &'static [u8] {
        b"test-bilibili-sms-login-secret"
    }

    fn test_api(
        pool: sqlx::PgPool,
        credential_repo: &Arc<UserProviderCredentialRepository>,
    ) -> TestResult<BilibiliApiImpl> {
        let instance_manager = Arc::new(synctv_core::service::RemoteProviderManager::new(
            Arc::new(ProviderInstanceRepository::new(pool.clone())),
        ));
        let provider = Arc::new(BilibiliProvider::with_client_manager(
            instance_manager,
            Arc::new(synctv_core::provider::ProviderClientManager::new()?),
        ));
        let alist_instance_manager = Arc::new(synctv_core::service::RemoteProviderManager::new(
            Arc::new(ProviderInstanceRepository::new(pool)),
        ));
        let alist_provider = Arc::new(synctv_core::provider::AlistProvider::with_client_manager(
            alist_instance_manager,
            Arc::new(synctv_core::provider::ProviderClientManager::new()?),
        ));
        let runtime = ProviderApiRuntime {
            access_service: Arc::new(synctv_core::provider::CachedProviderAccessService::new(
                credential_repo.clone(),
                alist_provider,
            )),
            event_service: Arc::new(synctv_realtime::fanout::LocalNoopRealtimeEventService::new()),
        };
        provider_ok(BilibiliApiImpl::new_with_runtime(
            Arc::new(provider.with_credential_repo(credential_repo.clone())),
            test_sms_login_secret(),
            runtime,
        ))
    }

    fn test_user(username: &str) -> User {
        User::new(username.to_string(), SignupMethod::Email)
    }

    async fn create_bilibili_provider_instance(pool: &sqlx::PgPool, name: &str) -> TestResult {
        ProviderInstanceRepository::new(pool.clone())
            .create(&synctv_core::models::ProviderInstance::new_remote(
                NewProviderInstance {
                    name: name.to_string(),
                    endpoint: format!("http://{name}.example.test:50051"),
                    comment: None,
                    jwt_secret: None,
                    custom_ca: None,
                    timeout_seconds: 10,
                    tls: false,
                    insecure_tls: false,
                    providers: vec![synctv_core::models::SourceProvider::Bilibili],
                },
            ))
            .await?;
        Ok(())
    }

    #[test]
    fn effective_instance_uses_credential_binding_when_request_omits_instance() -> TestResult {
        let resolved = provider_ok(BilibiliApiImpl::resolve_effective_instance_name(
            None,
            CredentialProviderInstanceName::CredentialBacked(Some(" bilibili_remote ")),
        ))?;

        assert_eq!(resolved.as_deref(), Some("bilibili_remote"));
        Ok(())
    }

    #[test]
    fn effective_instance_rejects_explicit_request_conflicting_with_credential_binding(
    ) -> TestResult {
        let err = provider_err(BilibiliApiImpl::resolve_effective_instance_name(
            Some("bilibili_other"),
            CredentialProviderInstanceName::CredentialBacked(Some("bilibili_remote")),
        ))?;

        assert!(matches!(
            err,
            synctv_core::provider::ProviderError::InvalidConfig(_)
        ));
        Ok(())
    }

    #[test]
    fn effective_instance_rejects_invalid_requested_instance_name() -> TestResult {
        let err = provider_err(BilibiliApiImpl::resolve_effective_instance_name(
            Some("bad instance!"),
            CredentialProviderInstanceName::NotCredentialBacked,
        ))?;

        assert!(matches!(
            err,
            synctv_core::provider::ProviderError::InvalidConfig(message)
                if message.contains("provider instance name")
        ));
        Ok(())
    }

    #[test]
    fn effective_instance_rejects_explicit_instance_for_unbound_credential() -> TestResult {
        let err = provider_err(BilibiliApiImpl::resolve_effective_instance_name(
            Some("bilibili_remote"),
            CredentialProviderInstanceName::CredentialBacked(None),
        ))?;

        assert!(matches!(
            err,
            synctv_core::provider::ProviderError::InvalidConfig(_)
        ));
        Ok(())
    }

    #[test]
    fn qr_login_status_cache_key_rejects_invalid_instance_name() -> TestResult {
        let err = provider_err(BilibiliProvider::qr_login_status_cache_key(
            Some("bad instance!"),
            "qr-key",
        ))?;

        assert!(matches!(
            err,
            synctv_core::provider::ProviderError::InvalidConfig(message)
                if message.contains("provider instance name")
        ));
        Ok(())
    }

    #[test]
    fn login_cookie_validation_rejects_empty_provider_response() -> TestResult {
        let err = provider_err(BilibiliProvider::ensure_login_cookies_present(
            &HashMap::new(),
            "SMS",
        ))?;

        assert!(matches!(
            err,
            synctv_core::provider::ProviderError::Authentication(_)
        ));
        Ok(())
    }

    #[test]
    fn login_cookie_validation_accepts_session_cookies() -> TestResult {
        let cookies = HashMap::from([("SESSDATA".to_string(), "session".to_string())]);

        provider_ok(BilibiliProvider::ensure_login_cookies_present(
            &cookies, "SMS",
        ))?;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn get_user_info_without_binding_reports_logged_out() -> TestResult {
        let (_postgres, pool) = create_test_pool().await;
        let credential_repo = Arc::new(UserProviderCredentialRepository::new(pool.clone()));
        let api = test_api(pool.clone(), &credential_repo)?;

        let response = provider_ok(
            api.get_user_info_with_context(
                &synctv_core::models::UserId::new(),
                synctv_proto::providers::bilibili::UserInfoRequest {
                    instance_name: String::new(),
                },
                None,
                None,
            )
            .await,
        )?;

        assert!(!response.is_login);
        assert!(response.username.is_empty());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn logout_without_binding_is_idempotent() -> TestResult {
        let (_postgres, pool) = create_test_pool().await;
        let credential_repo = Arc::new(UserProviderCredentialRepository::new(pool.clone()));
        let api = test_api(pool.clone(), &credential_repo)?;

        let response = provider_ok(
            api.logout(
                &synctv_core::models::UserId::new(),
                synctv_proto::providers::bilibili::LogoutRequest {},
            )
            .await,
        )?;

        assert_eq!(response.message, "Logout successful");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn persist_cookies_stores_login_provider_instance_binding() -> TestResult {
        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let credential_repo = Arc::new(UserProviderCredentialRepository::new_with_encryption(
            pool.clone(),
            test_encryption()?,
        ));
        let api = test_api(pool.clone(), &credential_repo)?;
        let user = user_repo
            .create(&test_user("bilibili_instance_login"))
            .await?;
        create_bilibili_provider_instance(&pool, "bilibili_remote").await?;
        let cookies = HashMap::from([("SESSDATA".to_string(), "session".to_string())]);

        provider_ok(
            api.provider
                .persist_cookies_credential(user.id, cookies, Some(" bilibili_remote "))
                .await,
        )?;

        let credential = core_ok(
            credential_repo
                .get_by_provider_and_server(
                    user.id,
                    synctv_core::provider::BilibiliProvider::NAME,
                    &BilibiliProvider::credential_server_id(),
                )
                .await,
        )?
        .ok_or_else(|| test_error("credential should exist"))?;
        assert_eq!(
            credential.provider_instance_name.as_deref(),
            Some("bilibili_remote")
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn get_binds_filters_by_provider_instance_binding() -> TestResult {
        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let credential_repo = Arc::new(UserProviderCredentialRepository::new_with_encryption(
            pool.clone(),
            test_encryption()?,
        ));
        let api = test_api(pool.clone(), &credential_repo)?;
        let user = user_repo.create(&test_user("bilibili_bind_filter")).await?;
        create_bilibili_provider_instance(&pool, "bilibili_remote").await?;
        let cookies = HashMap::from([("SESSDATA".to_string(), "session".to_string())]);

        provider_ok(
            api.provider
                .persist_cookies_credential(user.id, cookies, Some("bilibili_remote"))
                .await,
        )?;

        let matching = api_ok(api.get_binds(&user.id, Some("bilibili_remote")).await)?;
        let non_matching = api_ok(api.get_binds(&user.id, Some("bilibili_other")).await)?;

        assert_eq!(matching.binds.len(), 1);
        assert!(non_matching.binds.is_empty());
        Ok(())
    }
}
