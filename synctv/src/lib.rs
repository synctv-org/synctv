//! SyncTV server library.
//!
//! This crate provides the main server implementation for SyncTV.
//! It is primarily intended to be used as a binary.

pub(crate) mod admin_client;
mod app;
pub mod app_config;
mod bootstrap;
pub mod cli;
mod config_env;
mod config_loader;
mod email_outbox_dispatcher;
mod management_runtime;
mod migrations;
mod path_util;
mod realtime_bridge;
mod realtime_outbox_dispatcher;
mod resource_options;
mod rtmp_auth;
mod server;
mod shutdown;

pub use app::{Application, ApplicationBuildOptions};

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
