//! Emby Service - Complete implementation
//!
//! This is the full HTTP client implementation.
//! Both gRPC server and local usage call this service.

use super::{client::EmbyClient, types::UserInfo, EmbyError};
use crate::grpc::emby::{
    DeleteActiveEncodingsReq, Empty, FsListReq, FsListResp, GetItemReq, GetItemsReq, GetItemsResp,
    Item, LoginReq, LoginResp, LogoutReq, MeReq, MeResp, PlaybackInfoReq, PlaybackInfoResp,
    ReportPlaybackProgressReq, ReportPlaybackStartReq, ReportPlaybackStopReq, SystemInfoReq,
    SystemInfoResp,
};
use async_trait::async_trait;
use reqwest::{Client, StatusCode};

fn current_user_unavailable_for_token(error: &EmbyError) -> bool {
    matches!(
        error,
        EmbyError::Http { status, url, .. }
            if *status == StatusCode::BAD_REQUEST && url.ends_with("/Users/Me")
    ) || matches!(
        error,
        EmbyError::InvalidConfig(message)
            if message.contains("/Users/Me")
    )
}

fn username_matches(candidate_name: &str, requested_name: &str) -> bool {
    candidate_name == requested_name || candidate_name.eq_ignore_ascii_case(requested_name)
}

fn select_user_by_username<'a>(
    users: &'a [UserInfo],
    username: &str,
) -> Result<&'a UserInfo, EmbyError> {
    let trimmed = username.trim();
    if trimmed.is_empty() {
        return Err(EmbyError::InvalidConfig(
            "username must not be empty".to_string(),
        ));
    }

    let matches: Vec<&UserInfo> = users
        .iter()
        .filter(|user| username_matches(&user.name, trimmed))
        .collect();

    match matches.as_slice() {
        [user] => Ok(*user),
        [] => Err(EmbyError::InvalidConfig(format!(
            "No Emby/Jellyfin user found for username '{trimmed}'"
        ))),
        _ => Err(EmbyError::InvalidConfig(format!(
            "Multiple Emby/Jellyfin users matched username '{trimmed}'"
        ))),
    }
}

/// Unified Emby service interface
///
/// This trait defines all Emby operations using proto request/response types.
#[async_trait]
pub trait EmbyInterface: Send + Sync {
    async fn login(&self, request: LoginReq) -> Result<LoginResp, EmbyError>;

    async fn me(&self, request: MeReq) -> Result<MeResp, EmbyError>;

    async fn get_items(&self, request: GetItemsReq) -> Result<GetItemsResp, EmbyError>;

    async fn get_item(&self, request: GetItemReq) -> Result<Item, EmbyError>;

    async fn fs_list(&self, request: FsListReq) -> Result<FsListResp, EmbyError>;

    async fn get_system_info(&self, request: SystemInfoReq) -> Result<SystemInfoResp, EmbyError>;

    async fn logout(&self, request: LogoutReq) -> Result<Empty, EmbyError>;

    async fn playback_info(&self, request: PlaybackInfoReq) -> Result<PlaybackInfoResp, EmbyError>;

    async fn delete_active_encodings(
        &self,
        request: DeleteActiveEncodingsReq,
    ) -> Result<Empty, EmbyError>;

    async fn report_playback_start(
        &self,
        request: ReportPlaybackStartReq,
    ) -> Result<Empty, EmbyError>;

    async fn report_playback_stop(
        &self,
        request: ReportPlaybackStopReq,
    ) -> Result<Empty, EmbyError>;

    async fn report_playback_progress(
        &self,
        request: ReportPlaybackProgressReq,
    ) -> Result<Empty, EmbyError>;
}

/// Emby service implementation
///
/// This is the complete implementation that makes actual HTTP calls.
/// Used by both local callers and gRPC server.
pub struct EmbyService {
    client: Client,
}

impl EmbyService {
    pub fn new() -> Result<Self, reqwest::Error> {
        let client =
            crate::build_provider_http_client(synctv_common::ssrf::SsrfGuard::strict_policy())?;
        Ok(Self { client })
    }

    #[must_use]
    pub const fn with_client(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl EmbyInterface for EmbyService {
    async fn login(&self, request: LoginReq) -> Result<LoginResp, EmbyError> {
        let username = request.username.trim();
        if username.is_empty() {
            return Err(EmbyError::InvalidConfig(
                "username must not be empty".to_string(),
            ));
        }
        match request.credential {
            Some(crate::grpc::emby::login_req::Credential::Password(password)) => {
                let password = password.trim();
                if password.is_empty() {
                    return Err(EmbyError::InvalidConfig(
                        "password must not be empty".to_string(),
                    ));
                }

                let mut client = EmbyClient::with_http_client(&request.host, self.client.clone())?;
                let (token, user_id) = client.login(username, password).await?;
                let user_info = client.me().await?;

                if !username_matches(&user_info.name, username) {
                    return Err(EmbyError::InvalidConfig(format!(
                        "authenticated Emby/Jellyfin user '{}' does not match requested username '{}'",
                        user_info.name, username
                    )));
                }

                Ok(LoginResp {
                    token,
                    user_id,
                    username: user_info.name,
                    server_id: user_info.server_id,
                    policy: user_info.policy.map(Into::into),
                })
            }
            Some(crate::grpc::emby::login_req::Credential::ApiKey(api_key)) => {
                let api_key = api_key.trim();
                if api_key.is_empty() {
                    return Err(EmbyError::InvalidConfig(
                        "api_key must not be empty".to_string(),
                    ));
                }

                let client = EmbyClient::with_credentials_and_http_client(
                    &request.host,
                    api_key,
                    String::new(),
                    self.client.clone(),
                )?;
                let selected_user = match client.me().await {
                    Ok(user_info) => {
                        if !username_matches(&user_info.name, username) {
                            return Err(EmbyError::InvalidConfig(format!(
                                "API key authenticated as Emby/Jellyfin user '{}' instead of requested username '{}'",
                                user_info.name, username
                            )));
                        }
                        user_info
                    }
                    Err(error) if current_user_unavailable_for_token(&error) => {
                        let users = client.list_users().await?;
                        (*select_user_by_username(&users, username)?).clone()
                    }
                    Err(error) => return Err(error),
                };

                Ok(LoginResp {
                    token: api_key.to_string(),
                    user_id: selected_user.id.clone(),
                    username: selected_user.name,
                    server_id: selected_user.server_id,
                    policy: selected_user.policy.map(Into::into),
                })
            }
            None => Err(EmbyError::InvalidConfig(
                "exactly one of password or api_key must be provided".to_string(),
            )),
        }
    }

    async fn me(&self, request: MeReq) -> Result<MeResp, EmbyError> {
        let client = EmbyClient::with_credentials_and_http_client(
            &request.host,
            &request.token,
            &request.user_id,
            self.client.clone(),
        )?;
        let user_info = client.me().await?;

        Ok(user_info.into())
    }

    async fn get_items(&self, request: GetItemsReq) -> Result<GetItemsResp, EmbyError> {
        let client = EmbyClient::with_credentials_and_http_client(
            &request.host,
            &request.token,
            &request.user_id,
            self.client.clone(),
        )?;

        let parent_id = if request.parent_id.is_empty() {
            None
        } else {
            Some(request.parent_id.as_str())
        };

        let search_term = if request.search_term.is_empty() {
            None
        } else {
            Some(request.search_term.as_str())
        };

        let items_response = client.get_items(parent_id, search_term).await?;

        Ok(items_response.into())
    }

    async fn get_item(&self, request: GetItemReq) -> Result<Item, EmbyError> {
        let client = EmbyClient::with_credentials_and_http_client(
            &request.host,
            &request.token,
            &request.user_id,
            self.client.clone(),
        )?;
        let item = client.get_item(&request.item_id).await?;

        Ok(item.into())
    }

    async fn fs_list(&self, request: FsListReq) -> Result<FsListResp, EmbyError> {
        let client = EmbyClient::with_credentials_and_http_client(
            &request.host,
            &request.token,
            &request.user_id,
            self.client.clone(),
        )?;

        let path = if request.path.is_empty() {
            None
        } else {
            Some(request.path.as_str())
        };

        let search_term = if request.search_term.is_empty() {
            None
        } else {
            Some(request.search_term.as_str())
        };

        let fs_response = client
            .fs_list(path, request.start_index, request.limit, search_term)
            .await?;

        Ok(fs_response.into())
    }

    async fn get_system_info(&self, request: SystemInfoReq) -> Result<SystemInfoResp, EmbyError> {
        let client = EmbyClient::with_credentials_and_http_client(
            &request.host,
            &request.token,
            String::new(),
            self.client.clone(),
        )?;
        let info = client.get_system_info().await?;

        Ok(info.into())
    }

    async fn logout(&self, request: LogoutReq) -> Result<Empty, EmbyError> {
        let client = EmbyClient::with_credentials_and_http_client(
            &request.host,
            &request.token,
            String::new(),
            self.client.clone(),
        )?;
        client.logout().await?;
        Ok(Empty {})
    }

    async fn playback_info(&self, request: PlaybackInfoReq) -> Result<PlaybackInfoResp, EmbyError> {
        let client = EmbyClient::with_credentials_and_http_client(
            &request.host,
            &request.token,
            &request.user_id,
            self.client.clone(),
        )?;

        let media_source_id = if request.media_source_id.is_empty() {
            None
        } else {
            Some(request.media_source_id.as_str())
        };

        let audio_idx = if request.audio_stream_index < 0 {
            None
        } else {
            Some(request.audio_stream_index)
        };

        let subtitle_idx = if request.subtitle_stream_index < 0 {
            None
        } else {
            Some(request.subtitle_stream_index)
        };

        let max_bitrate = if request.max_streaming_bitrate == 0 {
            None
        } else {
            Some(request.max_streaming_bitrate)
        };

        let playback_info = client
            .get_playback_info(super::client::PlaybackInfoRequest {
                item_id: &request.item_id,
                media_source_id,
                audio_stream_index: audio_idx,
                subtitle_stream_index: subtitle_idx,
                max_streaming_bitrate: max_bitrate,
                max_audio_channels: request.max_audio_channels,
                enable_direct_play: request.enable_direct_play,
                enable_direct_stream: request.enable_direct_stream,
                enable_transcoding: request.enable_transcoding,
                device_profile: request.device_profile.as_ref(),
            })
            .await?;

        Ok(playback_info.into())
    }

    async fn delete_active_encodings(
        &self,
        request: DeleteActiveEncodingsReq,
    ) -> Result<Empty, EmbyError> {
        let client = EmbyClient::with_credentials_and_http_client(
            &request.host,
            &request.token,
            String::new(),
            self.client.clone(),
        )?;
        client
            .delete_active_encodings(&request.play_session_id)
            .await?;
        Ok(Empty {})
    }

    async fn report_playback_start(
        &self,
        request: ReportPlaybackStartReq,
    ) -> Result<Empty, EmbyError> {
        let client = EmbyClient::with_credentials_and_http_client(
            &request.host,
            &request.token,
            String::new(),
            self.client.clone(),
        )?;
        let media_source_id = if request.media_source_id.is_empty() {
            None
        } else {
            Some(request.media_source_id.as_str())
        };
        client
            .report_playback_start(
                &request.item_id,
                &request.play_session_id,
                media_source_id,
                request.position_ticks,
            )
            .await?;
        Ok(Empty {})
    }

    async fn report_playback_stop(
        &self,
        request: ReportPlaybackStopReq,
    ) -> Result<Empty, EmbyError> {
        let client = EmbyClient::with_credentials_and_http_client(
            &request.host,
            &request.token,
            String::new(),
            self.client.clone(),
        )?;
        client
            .report_playback_stop(
                &request.item_id,
                &request.play_session_id,
                request.position_ticks,
            )
            .await?;
        Ok(Empty {})
    }

    async fn report_playback_progress(
        &self,
        request: ReportPlaybackProgressReq,
    ) -> Result<Empty, EmbyError> {
        let client = EmbyClient::with_credentials_and_http_client(
            &request.host,
            &request.token,
            String::new(),
            self.client.clone(),
        )?;
        let media_source_id = if request.media_source_id.is_empty() {
            None
        } else {
            Some(request.media_source_id.as_str())
        };
        client
            .report_playback_progress(
                &request.item_id,
                &request.play_session_id,
                media_source_id,
                request.position_ticks,
                request.is_paused,
            )
            .await?;
        Ok(Empty {})
    }
}
