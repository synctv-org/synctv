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
mod service;
mod types;

pub use crate::error::ProviderClientError as EmbyError;
pub use client::{EmbyClient, EmbyListSource, PlaybackInfoRequest};
pub use service::{EmbyInterface, EmbyService};
pub use types::{
    default_device_profile, device_profile_from_playback_client_profile, AuthResponse,
    FsListResponse, ImageTags, Item, ItemsResponse, MediaSource, MediaStream, PathInfo,
    PlaybackInfoResp, PlaybackInfoResponse, SystemInfo, User, UserInfo, UserPolicy,
};
