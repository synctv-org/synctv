//! Emby Service - Complete implementation
//!
//! This is the full HTTP client implementation.
//! Both gRPC server and local usage call this service.

use super::{client::EmbyClient, EmbyError};
use crate::grpc::emby::{
    DeleteActiveEncodingsReq, Empty, FsListReq, FsListResp, GetItemReq, GetItemsReq, GetItemsResp,
    Item, LoginReq, LoginResp, LogoutReq, MeReq, MeResp, PlaybackInfoReq, PlaybackInfoResp,
    ReportPlaybackProgressReq, ReportPlaybackStartReq, ReportPlaybackStopReq, SystemInfoReq,
    SystemInfoResp,
};
use async_trait::async_trait;
use reqwest::Client;

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
    pub fn new() -> Self {
        Self {
            client: synctv_common::http::build_provider_client()
                .expect("default provider HTTP client should build"),
        }
    }

    #[must_use]
    pub const fn with_client(client: Client) -> Self {
        Self { client }
    }
}

impl Default for EmbyService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EmbyInterface for EmbyService {
    async fn login(&self, request: LoginReq) -> Result<LoginResp, EmbyError> {
        let mut client = EmbyClient::with_http_client(&request.host, self.client.clone())?;
        let (token, user_id) = client.login(&request.username, &request.password).await?;

        // Get server ID
        let system_info = client.get_system_info().await?;

        Ok(LoginResp {
            token,
            user_id,
            server_id: system_info.id,
        })
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
            .get_playback_info(
                &request.item_id,
                media_source_id,
                audio_idx,
                subtitle_idx,
                max_bitrate,
            )
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
