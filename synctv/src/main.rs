#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

// Allocator selection: mimalloc (default) or jemalloc (opt-in, unix only).
// Compile-time guard: cannot enable both simultaneously.
#[cfg(all(feature = "mimalloc", feature = "jemalloc"))]
compile_error!("features \"mimalloc\" and \"jemalloc\" are mutually exclusive - use only one");

#[cfg(all(feature = "jemalloc", not(feature = "mimalloc")))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(all(feature = "mimalloc", not(feature = "jemalloc")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    synctv_core::install_process_crypto_provider();
    Box::pin(synctv::cli::execute(synctv::cli::Cli::parse())).await
}
