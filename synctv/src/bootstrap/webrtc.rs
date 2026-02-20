use std::sync::Arc;
use tracing::{error, info, warn};

use synctv_core::Config;

/// Initialize WebRTC components: STUN server.
pub async fn init_webrtc(
    config: &Config,
) -> Option<Arc<synctv_core::service::StunServer>> {
    // STUN server (if enabled, powered by turn-rs)
    let stun_server = if config.webrtc.enable_builtin_stun {
        info!("Starting built-in STUN server (turn-rs)...");
        let bind_addr = format!("{}:{}", config.webrtc.stun_host, config.webrtc.stun_port);

        // Resolve external address with auto-detection fallback chain:
        // 1. Explicit config (stun_external_addr)
        // 2. advertise_host config / POD_IP
        // 3. Cloud metadata (AWS/GCP/Azure)
        let external_addr = if config.webrtc.stun_external_addr.is_empty() {
            let advertise = config.advertise_host();
            // Check if advertise_host resolved to something usable
            let candidate = format!("{advertise}:{}", config.webrtc.stun_port);
            if synctv_core::service::validate_external_addr(&candidate).is_ok() {
                candidate
            } else {
                // Try auto-detecting from cloud metadata
                info!("advertise_host '{}' is not a routable external IP, attempting cloud metadata detection...", advertise);
                if let Some(ip) = synctv_core::service::resolve_external_ip().await { format!("{ip}:{}", config.webrtc.stun_port) } else {
                    error!(
                        "Could not resolve a routable external IP for STUN server. \
                         advertise_host '{}' is not routable and cloud metadata detection failed. \
                         Set SYNCTV_WEBRTC_STUN_EXTERNAL_ADDR or STUN_EXTERNAL_IP to a public IP. \
                         Built-in STUN server will NOT start.",
                        advertise
                    );
                    return None;
                }
            }
        } else {
            config.webrtc.stun_external_addr.clone()
        };

        // Validate the final external address
        if let Err(e) = synctv_core::service::validate_external_addr(&external_addr) {
            error!("STUN external address validation failed: {}", e);
            error!(
                "Built-in STUN server will NOT start. NAT traversal requires a valid \
                 public external address. Set SYNCTV_WEBRTC_STUN_EXTERNAL_ADDR to a routable IP."
            );
            return None;
        }

        let stun_config = synctv_core::service::StunServerConfig {
            bind_addr,
            external_addr,
        };
        match synctv_core::service::StunServer::start(stun_config).await {
            Ok(server) => {
                info!("Built-in STUN server started on {}", server.local_addr());
                Some(server)
            }
            Err(e) => {
                warn!("Failed to start STUN server: {}", e);
                warn!("WebRTC P2P connectivity may be limited without STUN");
                None
            }
        }
    } else {
        info!("Built-in STUN server disabled");
        None
    };

    stun_server
}
