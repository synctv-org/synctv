use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use synctv_core::models::{RoomId, UserId};
use tonic::{Request, Response, Status};

use super::proto::{
    realtime_presence_service_server, GetRoomConnectionsRequest, GetRoomConnectionsResponse,
    GetUserOnlineStatusRequest, GetUserOnlineStatusResponse, RoomConnection, UserOnlineStatus,
};
use crate::sync::ConnectionRuntime;

const MAX_USER_IDS: usize = 1000;

fn u64_to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn unix_timestamp_secs() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(error) => {
            tracing::warn!(%error, "system clock is before Unix epoch");
            0
        }
    }
}

#[derive(Clone)]
pub struct RealtimePresenceServiceImpl {
    connection_runtime: Arc<dyn ConnectionRuntime>,
    node_id: String,
    cluster_secret: Option<Arc<String>>,
}

impl RealtimePresenceServiceImpl {
    #[must_use]
    pub fn new(connection_runtime: Arc<dyn ConnectionRuntime>, node_id: String) -> Self {
        Self {
            connection_runtime,
            node_id,
            cluster_secret: None,
        }
    }

    #[must_use]
    pub fn with_cluster_secret(mut self, secret: String) -> Self {
        self.cluster_secret = Some(Arc::new(secret));
        self
    }

    #[allow(clippy::result_large_err)]
    fn authenticate<T>(&self, request: &Request<T>) -> Result<(), Status> {
        let Some(expected) = &self.cluster_secret else {
            return Err(Status::unauthenticated(
                "cluster authentication secret is not configured",
            ));
        };
        synctv_cluster::grpc::validate_cluster_secret_metadata(request.metadata(), expected)
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl realtime_presence_service_server::RealtimePresenceService for RealtimePresenceServiceImpl {
    async fn get_user_online_status(
        &self,
        request: Request<GetUserOnlineStatusRequest>,
    ) -> Result<Response<GetUserOnlineStatusResponse>, Status> {
        self.authenticate(&request)?;
        let req = request.into_inner();

        if req.user_ids.len() > MAX_USER_IDS {
            return Err(Status::invalid_argument(format!(
                "user_ids array must contain at most {MAX_USER_IDS} entries"
            )));
        }

        let statuses = req
            .user_ids
            .iter()
            .map(|uid| {
                let user_id = UserId::try_from(*uid).map_err(|error| {
                    Status::invalid_argument(format!("invalid user_id: {error}"))
                })?;
                let connections = self.connection_runtime.get_user_connections(&user_id);
                let is_online = !connections.is_empty();
                let room_ids = connections
                    .iter()
                    .filter_map(|connection| connection.room_id.as_ref().map(RoomId::as_i64))
                    .collect();

                Ok(UserOnlineStatus {
                    user_id: *uid,
                    is_online,
                    room_ids,
                    node_id: self.node_id.clone(),
                })
            })
            .collect::<Result<Vec<_>, Status>>()?;

        Ok(Response::new(GetUserOnlineStatusResponse { statuses }))
    }

    async fn get_room_connections(
        &self,
        request: Request<GetRoomConnectionsRequest>,
    ) -> Result<Response<GetRoomConnectionsResponse>, Status> {
        self.authenticate(&request)?;
        let req = request.into_inner();

        let room_id = RoomId::try_from(req.room_id)
            .map_err(|error| Status::invalid_argument(format!("invalid room_id: {error}")))?;
        let now_unix = unix_timestamp_secs();
        let now_unix = u64_to_i64(now_unix);

        let connections = self
            .connection_runtime
            .get_room_connections(&room_id)
            .iter()
            .map(|connection| {
                let connected_secs_ago = u64_to_i64(connection.connected_at.elapsed().as_secs());
                let last_activity_secs_ago =
                    u64_to_i64(connection.last_activity.elapsed().as_secs());

                RoomConnection {
                    user_id: connection.user_id.as_i64(),
                    node_id: self.node_id.clone(),
                    connected_at: now_unix - connected_secs_ago,
                    last_activity: now_unix - last_activity_secs_ago,
                }
            })
            .collect();

        Ok(Response::new(GetRoomConnectionsResponse { connections }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::proto::realtime_presence_service_server::RealtimePresenceService;
    use super::*;
    use crate::sync::{ConnectionLimits, ConnectionManager};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn request_with_secret<T>(
        value: T,
    ) -> Result<Request<T>, tonic::metadata::errors::InvalidMetadataValue> {
        let mut request = Request::new(value);
        request.metadata_mut().insert(
            synctv_cluster::grpc::CLUSTER_SECRET_METADATA_KEY,
            "cluster-secret".parse()?,
        );
        Ok(request)
    }

    #[tokio::test]
    async fn presence_requests_require_cluster_secret() -> TestResult {
        let service = RealtimePresenceServiceImpl::new(
            Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            "node-a".to_string(),
        )
        .with_cluster_secret("cluster-secret".to_string());

        let error = service
            .get_user_online_status(Request::new(GetUserOnlineStatusRequest {
                user_ids: vec![1],
            }))
            .await
            .expect_err("missing secret must be rejected");

        assert_eq!(error.code(), tonic::Code::Unauthenticated);
        Ok(())
    }

    #[tokio::test]
    async fn user_online_status_reads_local_connection_runtime() -> TestResult {
        let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
        let user_id = UserId::expect_positive(42);
        connection_manager
            .register("conn-1".to_string(), user_id)
            .await?;

        let service = RealtimePresenceServiceImpl::new(connection_manager, "node-a".to_string())
            .with_cluster_secret("cluster-secret".to_string());
        let response = service
            .get_user_online_status(request_with_secret(GetUserOnlineStatusRequest {
                user_ids: vec![user_id.as_i64(), 43],
            })?)
            .await?
            .into_inner();

        assert_eq!(response.statuses.len(), 2);
        let online = response
            .statuses
            .iter()
            .find(|status| status.user_id == user_id.as_i64())
            .ok_or("registered user should be returned")?;
        assert!(online.is_online);
        assert_eq!(online.node_id, "node-a");
        Ok(())
    }
}
