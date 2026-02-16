//! Built-in STUN server powered by turn-rs.
//!
//! Starts turn-rs with STUN-only configuration (no auth configured,
//! so TURN allocations are rejected while STUN Binding requests work).
//! This is multi-replica safe since STUN is stateless.

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::task::JoinHandle;

/// STUN server configuration
#[derive(Debug, Clone)]
pub struct StunServerConfig {
    /// Bind address (e.g., "0.0.0.0:3478")
    pub bind_addr: String,
    /// External address for reflexive candidates (public IP:port)
    pub external_addr: String,
}

impl Default for StunServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:3478".to_string(),
            external_addr: "0.0.0.0:3478".to_string(),
        }
    }
}

/// Built-in STUN server backed by turn-rs.
///
/// Runs turn-rs with no authentication configured, so:
/// - STUN Binding requests work (stateless, no auth needed)
/// - TURN Allocate requests are rejected (auth required but none configured)
pub struct StunServer {
    task: JoinHandle<()>,
    local_addr: SocketAddr,
}

impl StunServer {
    /// Start the STUN server.
    ///
    /// Configures turn-rs with a single UDP interface and no authentication,
    /// so only STUN Binding requests succeed.
    pub async fn start(config: StunServerConfig) -> anyhow::Result<Arc<Self>> {
        let listen: SocketAddr = config
            .bind_addr
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid bind_addr '{}': {e}", config.bind_addr))?;
        let external: SocketAddr = config
            .external_addr
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid external_addr '{}': {e}", config.external_addr))?;

        let turn_config = turn_server::config::Config {
            server: turn_server::config::Server {
                realm: "synctv".to_string(),
                interfaces: vec![turn_server::config::Interface::Udp {
                    listen,
                    external,
                    idle_timeout: 60,
                    mtu: 1500,
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        let local_addr = listen;

        let task = crate::spawn::spawn_monitored("stun_server", async move {
            if let Err(e) = turn_server::start_server(turn_config).await {
                tracing::error!(error = %e, "STUN server (turn-rs) exited with error");
            }
        });

        tracing::info!(
            bind_addr = %listen,
            external_addr = %external,
            "STUN server started (powered by turn-rs)"
        );

        Ok(Arc::new(Self { task, local_addr }))
    }

    /// Get the local bind address.
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Shut down the STUN server by aborting the background task.
    pub async fn shutdown(&self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stun_server_config_default() {
        let config = StunServerConfig::default();
        assert_eq!(config.bind_addr, "0.0.0.0:3478");
        assert_eq!(config.external_addr, "0.0.0.0:3478");
    }
}
