//! `SyncTV` SFU (Selective Forwarding Unit)
//!
//! # Experimental -- Infrastructure Only
//!
//! **This crate is not production-ready.** The forwarding plane (RTP packet
//! routing, simulcast, bandwidth estimation) is implemented, but the control
//! plane (peer connection management, SDP signaling, ICE, subscriber output
//! path) is missing. See `synctv-sfu/README.md` for a detailed status report.
//!
//! ## Architecture
//!
//! - **[`SfuRoom`]**: Manages a single room with multiple peers
//! - **[`SfuPeer`]**: Represents a single participant in an SFU room
//! - **[`MediaTrack`]**: Represents an audio or video track
//! - **[`QualityLayer`]**: Simulcast quality selection (high/medium/low)
//!
//! ## Implemented Features
//!
//! - Selective forwarding of RTP media streams
//! - Simulcast support (multiple quality layers)
//! - Automatic mode switching (P2P / SFU based on room size)
//! - Bandwidth estimation and adaptive quality
//! - Per-peer subscription management
//! - Network quality monitoring with adaptive actions
//!
//! ## Partially Implemented
//!
//! - WebRTC control plane (basic implementation, needs integration testing)
//! - Integration with synctv-api signaling endpoints (TODO)

mod config;
mod manager;
pub mod network_monitor;
mod packet_pacer;
mod peer;
mod room;
mod rtcp_handler;
mod track;
mod types;
mod webrtc_control;

pub use config::SfuConfig;
pub use manager::SfuManager;
pub use network_monitor::{NetworkQualityMonitor, NetworkStats, QualityAction};
pub use packet_pacer::{CongestionController, PacketPacer};
pub use peer::{SfuPeer, PeerStats};
pub use room::{SfuRoom, RoomMode, RoomStats};
pub use rtcp_handler::RtcpHandler;
pub use track::{MediaTrack, QualityLayer, TrackKind};
pub use types::{PeerId, RoomId, TrackId};
pub use webrtc_control::{IceManager, IceServerConfig, PeerConnection};
