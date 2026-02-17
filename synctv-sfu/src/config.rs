//! SFU Configuration
//!
//! This struct mirrors the SFU-related fields from `synctv_core::config::WebRTCConfig`.
//! The `synctv-sfu` crate intentionally does **not** depend on `synctv-core` to keep
//! the SFU forwarding plane self-contained and testable in isolation.
//!
//! ## Field mapping from `WebRTCConfig` (synctv-core)
//!
//! | `SfuConfig`                  | `WebRTCConfig`                     |
//! |------------------------------|------------------------------------|
//! | `sfu_threshold`              | `sfu_threshold`                    |
//! | `max_sfu_rooms`              | `max_sfu_rooms`                    |
//! | `max_peers_per_room`         | `max_peers_per_sfu_room`           |
//! | `enable_simulcast`           | `enable_simulcast`                 |
//! | `enable_bandwidth_estimation`| `enable_bandwidth_estimation`      |
//!
//! The conversion is performed in `synctv/src/main.rs` at startup.
//! Use [`SfuConfig::from_webrtc_fields`] for a less error-prone mapping.

use serde::{Deserialize, Serialize};

/// SFU configuration
///
/// Defaults match those of `synctv_core::config::WebRTCConfig` so that
/// constructing `SfuConfig::default()` produces the same behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SfuConfig {
    /// Room size threshold to automatically switch to SFU mode.
    /// Maps to `WebRTCConfig::sfu_threshold`.
    pub sfu_threshold: usize,
    /// Maximum number of concurrent SFU rooms (0 = unlimited).
    /// Maps to `WebRTCConfig::max_sfu_rooms`.
    pub max_sfu_rooms: usize,
    /// Maximum peers per SFU room.
    /// Maps to `WebRTCConfig::max_peers_per_sfu_room`.
    pub max_peers_per_room: usize,
    /// Enable Simulcast (multiple quality layers).
    /// Maps to `WebRTCConfig::enable_simulcast`.
    pub enable_simulcast: bool,
    /// Enable bandwidth estimation.
    /// Maps to `WebRTCConfig::enable_bandwidth_estimation`.
    pub enable_bandwidth_estimation: bool,
    /// Timeout in seconds for ICE connection establishment.
    /// If a peer fails to reach `Connected` ICE state within this duration,
    /// the peer connection is closed and the session is removed.
    /// 0 = no timeout (not recommended in production).
    pub ice_connection_timeout_secs: u64,
    /// Timeout in seconds for room migration from P2P to SFU mode.
    /// If `complete_migration()` is not called within this duration after
    /// entering `Migrating` mode, the room is force-transitioned to SFU mode.
    /// This prevents rooms from being stuck indefinitely in `Migrating` state
    /// due to crashes or errors during the migration process.
    pub migration_timeout_secs: u64,
}

impl SfuConfig {
    /// Construct from the SFU-related fields of `WebRTCConfig`.
    ///
    /// This is a convenience constructor that takes the five shared fields
    /// directly, avoiding accidental field-name mismatches between
    /// `WebRTCConfig::max_peers_per_sfu_room` and `SfuConfig::max_peers_per_room`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let sfu_config = SfuConfig::from_webrtc_fields(
    ///     config.webrtc.sfu_threshold,
    ///     config.webrtc.max_sfu_rooms,
    ///     config.webrtc.max_peers_per_sfu_room,
    ///     config.webrtc.enable_simulcast,
    ///     config.webrtc.enable_bandwidth_estimation,
    /// );
    /// ```
    #[must_use]
    pub const fn from_webrtc_fields(
        sfu_threshold: usize,
        max_sfu_rooms: usize,
        max_peers_per_room: usize,
        enable_simulcast: bool,
        enable_bandwidth_estimation: bool,
    ) -> Self {
        Self {
            sfu_threshold,
            max_sfu_rooms,
            max_peers_per_room,
            enable_simulcast,
            enable_bandwidth_estimation,
            ice_connection_timeout_secs: 30,
            migration_timeout_secs: 60,
        }
    }
}

impl Default for SfuConfig {
    fn default() -> Self {
        Self {
            sfu_threshold: 5,
            max_sfu_rooms: 0,
            max_peers_per_room: 50,
            enable_simulcast: true,
            enable_bandwidth_estimation: true,
            ice_connection_timeout_secs: 30,
            migration_timeout_secs: 60,
        }
    }
}
