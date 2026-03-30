#![cfg_attr(test, allow(clippy::unwrap_used))]

// Allocator selection: mimalloc (default) or jemalloc (opt-in, unix only).
// Compile-time guard: cannot enable both simultaneously.
#[cfg(all(feature = "mimalloc", feature = "jemalloc"))]
compile_error!("features \"mimalloc\" and \"jemalloc\" are mutually exclusive — use only one");

#[cfg(all(feature = "jemalloc", not(feature = "mimalloc")))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(all(feature = "mimalloc", not(feature = "jemalloc")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
    info!("API address: {}", config.api_address());

    let app = app::Application::build(config).await?;
    app.run().await
}
