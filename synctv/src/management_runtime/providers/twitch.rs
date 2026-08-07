use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError};
use synctv_management::provider_runtime::TwitchRuntime;
use synctv_proto::providers::twitch as twitch_proto;

use super::take_instance;

pub(crate) struct ManagementTwitchRuntime {
    inner: Arc<synctv_api::providers::TwitchApiImpl>,
}

impl ManagementTwitchRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::providers::TwitchApiImpl>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl TwitchRuntime for ManagementTwitchRuntime {
    async fn bind(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::BindRequest,
        instance_name: Option<&str>,
    ) -> Result<twitch_proto::BindResponse, ProviderError> {
        self.inner
            .bind(*caller_user_id, request, instance_name)
            .await
    }

    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<twitch_proto::GetBindsResponse, ProviderError> {
        self.inner.get_binds(*caller_user_id, instance_name).await
    }

    async fn unbind(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::UnbindRequest,
    ) -> Result<twitch_proto::UnbindResponse, ProviderError> {
        self.inner.unbind(*caller_user_id, request).await
    }

    async fn resolve(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::ResolveRequest,
        instance_name: Option<&str>,
    ) -> Result<twitch_proto::ResolveResponse, ProviderError> {
        self.inner
            .resolve(*caller_user_id, request, instance_name)
            .await
    }

    async fn list_channel_items(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::ListChannelItemsRequest,
        instance_name: Option<&str>,
    ) -> Result<twitch_proto::ListChannelItemsResponse, ProviderError> {
        self.inner
            .list_channel_items(*caller_user_id, request, instance_name)
            .await
    }

    async fn list_followed_live(
        &self,
        caller_user_id: &UserId,
        mut request: twitch_proto::ListFollowedLiveRequest,
    ) -> Result<twitch_proto::ListFollowedLiveResponse, ProviderError> {
        let instance_name = take_instance(&mut request.instance_name);
        self.inner
            .list_followed_live(*caller_user_id, request, instance_name.as_deref())
            .await
    }

    async fn list_category_streams(
        &self,
        caller_user_id: &UserId,
        mut request: twitch_proto::ListCategoryStreamsRequest,
    ) -> Result<twitch_proto::ListCategoryStreamsResponse, ProviderError> {
        let instance_name = take_instance(&mut request.instance_name);
        self.inner
            .list_category_streams(*caller_user_id, request, instance_name.as_deref())
            .await
    }

    async fn list_top_categories(
        &self,
        caller_user_id: &UserId,
        mut request: twitch_proto::ListTopCategoriesRequest,
    ) -> Result<twitch_proto::ListTopCategoriesResponse, ProviderError> {
        let instance_name = take_instance(&mut request.instance_name);
        self.inner
            .list_top_categories(*caller_user_id, request, instance_name.as_deref())
            .await
    }

    async fn search_live_channels(
        &self,
        caller_user_id: &UserId,
        mut request: twitch_proto::SearchLiveChannelsRequest,
    ) -> Result<twitch_proto::SearchLiveChannelsResponse, ProviderError> {
        let instance_name = take_instance(&mut request.instance_name);
        self.inner
            .search_live_channels(*caller_user_id, request, instance_name.as_deref())
            .await
    }

    async fn list_schedule(
        &self,
        caller_user_id: &UserId,
        mut request: twitch_proto::ListScheduleRequest,
    ) -> Result<twitch_proto::ListScheduleResponse, ProviderError> {
        let instance_name = take_instance(&mut request.instance_name);
        self.inner
            .list_schedule(*caller_user_id, request, instance_name.as_deref())
            .await
    }
}
