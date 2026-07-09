use std::sync::Arc;
use tracing::{error, info, warn};

use crate::app_config::{AppConfig as Config, WebRTCMode};
use synctv_core::service::{BuiltinStunRuntimeReason, WebRtcRuntimeMode, WebRtcRuntimeStatus};

fn ip_from_env(var: &str) -> Option<std::net::IpAddr> {
    std::env::var(var)
        .ok()
        .and_then(|s| s.trim().parse::<std::net::IpAddr>().ok())
        .filter(|ip| !ip.is_unspecified())
}

async fn try_aws_metadata(client: &reqwest::Client) -> Option<std::net::IpAddr> {
    let token = client
        .put("http://169.254.169.254/latest/api/token")
        .header("X-aws-ec2-metadata-token-ttl-seconds", "30")
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;

    let ip_str = client
        .get("http://169.254.169.254/latest/meta-data/public-ipv4")
        .header("X-aws-ec2-metadata-token", token.trim())
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;

    ip_str.trim().parse::<std::net::IpAddr>().ok()
}

async fn try_gcp_metadata(client: &reqwest::Client) -> Option<std::net::IpAddr> {
    let ip_str = client
        .get("http://metadata.google.internal/computeMetadata/v1/instance/network-interfaces/0/access-configs/0/external-ip")
        .header("Metadata-Flavor", "Google")
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;

    ip_str.trim().parse::<std::net::IpAddr>().ok()
}

async fn try_azure_metadata(client: &reqwest::Client) -> Option<std::net::IpAddr> {
    let resp = client
        .get("http://169.254.169.254/metadata/instance/network/interface/0/ipv4/ipAddress/0/publicIpAddress?api-version=2021-02-01&format=text")
        .header("Metadata", "true")
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;

    resp.trim().parse::<std::net::IpAddr>().ok()
}

async fn resolve_external_ip() -> Option<std::net::IpAddr> {
    if let Some(ip) = ip_from_env("STUN_EXTERNAL_IP") {
        info!(ip = %ip, "Resolved external IP from STUN_EXTERNAL_IP env var");
        return Some(ip);
    }

    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .connect_timeout(std::time::Duration::from_secs(1))
        .build()
    else {
        return None;
    };

    if let Some(ip) = try_aws_metadata(&client).await {
        info!(ip = %ip, "Resolved external IP from AWS EC2 metadata");
        return Some(ip);
    }

    if let Some(ip) = try_gcp_metadata(&client).await {
        info!(ip = %ip, "Resolved external IP from GCP metadata");
        return Some(ip);
    }

    if let Some(ip) = try_azure_metadata(&client).await {
        info!(ip = %ip, "Resolved external IP from Azure IMDS");
        return Some(ip);
    }

    None
}

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

        // Resolve the external address from top-level startup inputs and
        // process-environment discovery. Core receives only the final address.
        let external_addr = if config.webrtc.stun_external_addr.is_empty() {
            let advertise = config.advertise_host();
            let candidate = format!("{advertise}:{}", config.webrtc.stun_port);
            if synctv_core::service::validate_external_addr(&candidate).is_ok() {
                candidate
            } else {
                info!("advertise_host '{}' is not a routable external IP, attempting cloud metadata detection...", advertise);
                if let Some(ip) = resolve_external_ip().await {
                    format!("{ip}:{}", config.webrtc.stun_port)
                } else {
                    let message = format!(
                        "Could not resolve a routable external IP for STUN server. \
                         advertise_host '{advertise}' is not routable and cloud metadata detection failed. \
                         Set webrtc.stun_external_addr to a public ip:port or DNS name:port, \
                         or set STUN_EXTERNAL_IP in the startup environment. \
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
                 webrtc.stun_external_addr to a routable IP."
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
