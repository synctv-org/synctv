use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::twitch as twitch_proto;

#[tonic::async_trait]
pub trait TwitchRuntime: Send + Sync {
    async fn bind(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::BindRequest,
        instance_name: Option<&str>,
    ) -> Result<twitch_proto::BindResponse, ProviderError>;
    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<twitch_proto::GetBindsResponse, ProviderError>;
    async fn unbind(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::UnbindRequest,
    ) -> Result<twitch_proto::UnbindResponse, ProviderError>;
    async fn resolve(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::ResolveRequest,
        instance_name: Option<&str>,
    ) -> Result<twitch_proto::ResolveResponse, ProviderError>;
    async fn list_channel_items(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::ListChannelItemsRequest,
        instance_name: Option<&str>,
    ) -> Result<twitch_proto::ListChannelItemsResponse, ProviderError>;
    async fn list_followed_live(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::ListFollowedLiveRequest,
    ) -> Result<twitch_proto::ListFollowedLiveResponse, ProviderError>;
    async fn list_category_streams(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::ListCategoryStreamsRequest,
    ) -> Result<twitch_proto::ListCategoryStreamsResponse, ProviderError>;
    async fn list_top_categories(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::ListTopCategoriesRequest,
    ) -> Result<twitch_proto::ListTopCategoriesResponse, ProviderError>;
    async fn search_live_channels(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::SearchLiveChannelsRequest,
    ) -> Result<twitch_proto::SearchLiveChannelsResponse, ProviderError>;
    async fn list_schedule(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::ListScheduleRequest,
    ) -> Result<twitch_proto::ListScheduleResponse, ProviderError>;
}
