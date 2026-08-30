//! Standards-based WHIP/WHEP media primitives.
//!
//! HTTP resource handling, authorization, and stream ownership belong to the
//! application layer. This module owns only WebRTC negotiation and bounded RTP
//! transfer to and from `StreamHub`.

mod media;
mod peer;

pub use peer::{
    create_whep_client_session, create_whep_session, create_whip_session, PeerSession,
    WebRtcConfig, WebRtcError, WebRtcIceServer, WhepClientSession,
};
