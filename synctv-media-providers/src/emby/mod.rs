//! Emby/Jellyfin Provider Client
//!
//! Pure HTTP client for Emby/Jellyfin API, independent of `MediaProvider`.
//!
//! # Features
//! - Authentication
//! - Media item retrieval
//! - Playback info generation
//! - Device profile management

mod client;
mod error;
mod service;
mod types;

pub use client::{EmbyClient, PlaybackInfoRequest};
pub use error::EmbyError;
pub use service::{EmbyInterface, EmbyService};
pub use types::{
    default_device_profile, device_profile_from_playback_client_profile, AuthResponse,
    FsListResponse, ImageTags, Item, ItemsResponse, MediaSource, MediaStream, PathInfo,
    PlaybackInfoResp, PlaybackInfoResponse, SystemInfo, User, UserInfo, UserPolicy,
};
