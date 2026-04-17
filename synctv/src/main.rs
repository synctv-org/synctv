#![cfg_attr(test, allow(clippy::unwrap_used))]

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive — use only one");

#[cfg(all(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
compile_error!(
    "features \"tls-webpki-roots\" and \"tls-native-roots\" are mutually exclusive — use only one"
);

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

mod admin_client;
mod app;
mod bootstrap;
mod cli;
mod cluster_bridge;
mod migrations;
mod rtmp_auth;
mod server;
mod shutdown;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    Box::pin(cli::execute(clap::Parser::parse())).await
}

pub(crate) fn install_panic_hook(include_backtrace: bool) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        default_hook(panic_info);

        if include_backtrace {
            eprintln!("Backtrace:\n{}", std::backtrace::Backtrace::force_capture());
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::install_panic_hook;

    #[test]
    fn install_panic_hook_is_repeatable_for_both_modes() {
        install_panic_hook(false);
        install_panic_hook(true);
        install_panic_hook(false);
    }
}
