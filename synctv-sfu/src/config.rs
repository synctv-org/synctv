//! SFU Configuration
//!
//! Re-uses the SFU-related fields from `synctv_core::config::WebRTCConfig`
//! to avoid duplication. This struct is the single source of truth within
//! the SFU crate.

use serde::{Deserialize, Serialize};

/// SFU configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SfuConfig {
    /// Room size threshold to automatically switch to SFU mode
    pub sfu_threshold: usize,
    /// Maximum number of concurrent SFU rooms (0 = unlimited)
    pub max_sfu_rooms: usize,
    /// Maximum peers per SFU room
    pub max_peers_per_room: usize,
    /// Enable Simulcast (multiple quality layers)
    pub enable_simulcast: bool,
    /// Enable bandwidth estimation
    pub enable_bandwidth_estimation: bool,
}

impl Default for SfuConfig {
    fn default() -> Self {
        Self {
            sfu_threshold: 5,
            max_sfu_rooms: 0,
            max_peers_per_room: 50,
            enable_simulcast: true,
            enable_bandwidth_estimation: true,
        }
    }
}
