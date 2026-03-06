use std::sync::Arc;
use tracing::{error, info, warn};

use synctv_core::Config;
use tokio_util::sync::CancellationToken;

/// WebRTC components initialized during bootstrap
pub struct WebRTCComponents {
    /// Built-in STUN server (if enabled)
    pub stun_server: Option<Arc<synctv_core::service::StunServer>>,
    /// TURN health checker for monitoring TURN servers
    pub turn_health_checker: Option<Arc<synctv_core::service::TurnHealthChecker>>,
}

/// Initialize WebRTC components: STUN server and TURN health checker.
///
/// Note: STUN and TURN are independent. STUN failure does not prevent TURN
/// health checker from starting.
pub async fn init_webrtc(config: &Config, cancel: CancellationToken) -> WebRTCComponents {
    // TURN health checker (initialize first, independent of STUN)
    // This ensures TURN monitoring is available even if STUN configuration is invalid
    let turn_health_checker = if !config.webrtc.turn_shared_secret.is_empty()
        && !config.webrtc.turn_server_urls.is_empty()
    {
        info!("Initializing TURN health checker...");

        let checker = Arc::new(synctv_core::service::TurnHealthChecker::new());

        // Start periodic health checks
        checker
            .clone()
            .spawn_health_checks(config.webrtc.turn_server_urls.clone(), cancel.clone());

        info!(
            "TURN health checker started for {} servers",
            config.webrtc.turn_server_urls.len()
        );
        Some(checker)
    } else {
        info!("TURN health checker disabled (no TURN servers configured)");
        None
    };

    // STUN server (if enabled, powered by turn-rs)
    // STUN is independent of TURN - STUN failure should not affect TURN health checker
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
                if let Some(ip) = synctv_core::service::resolve_external_ip().await {
                    format!("{ip}:{}", config.webrtc.stun_port)
                } else {
                    error!(
                        "Could not resolve a routable external IP for STUN server. \
                         advertise_host '{}' is not routable and cloud metadata detection failed. \
                         Set SYNCTV_WEBRTC_STUN_EXTERNAL_ADDR or STUN_EXTERNAL_IP to a public IP. \
                         Built-in STUN server will NOT start.",
                        advertise
                    );
                    // Return with TURN health checker (already initialized above)
                    return WebRTCComponents {
                        stun_server: None,
                        turn_health_checker,
                    };
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
            // Return with TURN health checker (already initialized above)
            return WebRTCComponents {
                stun_server: None,
                turn_health_checker,
            };
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

    WebRTCComponents {
        stun_server,
        turn_health_checker,
    }
}
