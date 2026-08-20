pub mod proto {
    tonic::include_proto!("synctv.realtime");
}

mod presence_client;
mod presence_service;

pub use presence_client::{FanOutResult, RealtimePresenceClient, RealtimePresenceClientConfig};
pub use presence_service::RealtimePresenceServiceImpl;
pub use proto::realtime_presence_service_server::{
    RealtimePresenceService, RealtimePresenceServiceServer,
};
pub use proto::{RoomConnection, UserOnlineStatus};
