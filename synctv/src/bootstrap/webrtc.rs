use std::sync::Arc;
use tracing::{error, info, warn};

use synctv_core::config::WebRTCMode;
use synctv_core::service::{BuiltinStunRuntimeReason, WebRtcRuntimeMode, WebRtcRuntimeStatus};
use synctv_core::Config;

/// WebRTC components initialized during bootstrap
pub struct WebRTCComponents {
    /// Built-in STUN server (if enabled)
    pub stun_server: Option<Arc<synctv_core::service::StunServer>>,
    /// Runtime status exposed to health and ICE bootstrap responses.
    pub status: WebRtcRuntimeStatus,
}

/// Initialize WebRTC components.
pub async fn init_webrtc(config: &Config) -> WebRTCComponents {
    if config.webrtc.mode == WebRTCMode::SignalingOnly {
        info!("WebRTC signaling_only mode selected; built-in STUN server disabled");
        return WebRTCComponents {
            stun_server: None,
            status: WebRtcRuntimeStatus::signaling_only(),
        };
    }

    if !config.webrtc.enable_builtin_stun {
        info!("Built-in STUN server disabled");
        return WebRTCComponents {
            stun_server: None,
            status: WebRtcRuntimeStatus::disabled_by_config(WebRtcRuntimeMode::PeerToPeer),
        };
    }

    let (stun_server, status) = {
        info!("Starting built-in STUN server...");
        let bind_addr = format!("{}:{}", config.webrtc.stun_host, config.webrtc.stun_port);

        // Resolve external address with auto-detection fallback chain:
        // 1. Explicit config (stun_external_addr)
        // 2. advertise_host config
        // 3. STUN_EXTERNAL_IP / cloud metadata (AWS/GCP/Azure)
        let external_addr = if config.webrtc.stun_external_addr.is_empty() {
            let advertise = config.advertise_host();
            // Check if advertise_host resolved to something usable
            let candidate = format!("{advertise}:{}", config.webrtc.stun_port);
            if synctv_core::service::validate_external_addr(&candidate).is_ok() {
                candidate
            } else {
                // Try auto-detecting from cloud metadata
                info!("advertise_host '{}' is not a routable external IP, attempting cloud metadata detection...", advertise);
                if let Some(ip) = synctv_core::service::resolve_external_ip().await {
                    format!("{ip}:{}", config.webrtc.stun_port)
                } else {
                    let message = format!(
                        "Could not resolve a routable external IP for STUN server. \
                         advertise_host '{advertise}' is not routable and cloud metadata detection failed. \
                         Set SYNCTV_WEBRTC_STUN_EXTERNAL_ADDR to a public ip:port or DNS name:port, \
                         or set STUN_EXTERNAL_IP to a public IP. \
                         Built-in STUN server will NOT start."
                    );
                    error!("{message}");
                    return WebRTCComponents {
                        stun_server: None,
                        status: WebRtcRuntimeStatus::degraded(
                            BuiltinStunRuntimeReason::ExternalAddrUnresolved,
                            message,
                            None,
                        ),
                    };
                }
            }
        } else {
            config.webrtc.stun_external_addr.clone()
        };

        // Validate the final external address
        if let Err(e) = synctv_core::service::validate_external_addr(&external_addr) {
            let message = format!(
                "STUN external address validation failed: {e}. Built-in STUN server will NOT start. \
                 NAT traversal requires a valid public external address. Set \
                 SYNCTV_WEBRTC_STUN_EXTERNAL_ADDR to a routable IP."
            );
            error!("{message}");
            return WebRTCComponents {
                stun_server: None,
                status: WebRtcRuntimeStatus::degraded(
                    BuiltinStunRuntimeReason::ExternalAddrInvalid,
                    message,
                    Some(external_addr),
                ),
            };
        }

        let stun_config = synctv_core::service::StunServerConfig {
            bind_addr,
            external_addr,
        };
        match synctv_core::service::StunServer::start(&stun_config) {
            Ok(server) => {
                info!("Built-in STUN server started on {}", server.local_addr());
                let status =
                    WebRtcRuntimeStatus::running(server.local_addr(), server.external_addr());
                (Some(server), status)
            }
            Err(e) => {
                let message = format!("Failed to start STUN server: {e}");
                warn!("{message}");
                warn!("WebRTC P2P connectivity may be limited without STUN");
                (
                    None,
                    WebRtcRuntimeStatus::degraded(
                        BuiltinStunRuntimeReason::BindFailed,
                        message,
                        Some(stun_config.external_addr),
                    ),
                )
            }
        }
    };

    WebRTCComponents {
        stun_server,
        status,
    }
}
