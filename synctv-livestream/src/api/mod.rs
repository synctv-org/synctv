// Public API for synctv-api integration
// This module provides high-level APIs for implementing live streaming
// endpoints in synctv-api. All streaming is scoped to room_id:media_id.

pub(crate) mod livestream;
pub(crate) mod tracker;
