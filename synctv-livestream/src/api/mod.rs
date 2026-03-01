// Public API for synctv-api integration
//
// This module provides high-level APIs for implementing live streaming
// endpoints in synctv-api. All streaming is scoped to room_id:media_id.

pub mod livestream;
pub mod tracker;

// Re-export public types from livestream module
pub use livestream::{
    FlvStreamingApi, HlsStreamingApi, LiveStreamingInfrastructure, StreamTracker, UserStreamTracker,
};
