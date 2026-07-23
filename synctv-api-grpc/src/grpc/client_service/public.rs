use tonic::{Request, Response, Status};

use super::{map_api_error, ClientServiceImpl};
use synctv_api_common::impls::{ApiError, EndpointRateLimitCategory};
use synctv_proto::client::{
    public_service_server::PublicService, DiscoverRoomsRequest, DiscoverRoomsResponse,
    GetPublicSettingsRequest, GetPublicSettingsResponse, GetRoomDiscoveryRequest,
    GetServerInfoRequest, GetServerInfoResponse, GetServerTimeRequest, GetServerTimeResponse,
    RoomDiscoveryItem,
};

#[tonic::async_trait]
// Tonic generated service traits require `Result<Response<_>, tonic::Status>`.
#[allow(clippy::result_large_err)]
impl PublicService for ClientServiceImpl {
    async fn get_room_discovery(
        &self,
        request: Request<GetRoomDiscoveryRequest>,
    ) -> Result<Response<RoomDiscoveryItem>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint(
                &metadata,
                EndpointRateLimitCategory::Read,
                move || async move { client_api.get_public_room_discovery(req).await },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn discover_rooms(
        &self,
        request: Request<DiscoverRoomsRequest>,
    ) -> Result<Response<DiscoverRoomsResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint(
                &metadata,
                EndpointRateLimitCategory::Read,
                move || async move { client_api.discover_public_rooms(req).await },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_public_settings(
        &self,
        request: Request<GetPublicSettingsRequest>,
    ) -> Result<Response<GetPublicSettingsResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Read, || async move {
                client_api.get_public_settings()
            })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_server_info(
        &self,
        request: Request<GetServerInfoRequest>,
    ) -> Result<Response<GetServerInfoResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Read, || async move {
                client_api.get_server_info().await
            })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_server_time(
        &self,
        request: Request<GetServerTimeRequest>,
    ) -> Result<Response<GetServerTimeResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Read, || async move {
                Ok::<GetServerTimeResponse, ApiError>(client_api.get_server_time(req))
            })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }
}
