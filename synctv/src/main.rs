mod app;
mod bootstrap;
mod cluster_bridge;
mod migrations;
mod rtmp_auth;
mod server;
mod shutdown;

use anyhow::Result;
use synctv_core::bootstrap::load_config;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    let config = load_config()?;
    let _log_guard = synctv_core::logging::init_logging(&config.logging)?;
    info!("SyncTV server starting...");
    info!("gRPC address: {}", config.grpc_address());
    info!("HTTP address: {}", config.http_address());

    let app = app::Application::build(config).await?;
    app.run().await
}
