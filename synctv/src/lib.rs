//! SyncTV server library.
//!
//! This crate provides the main server implementation for SyncTV.
//! It is primarily intended to be used as a binary, but exposes
//! internal modules for testing purposes.

pub(crate) mod admin_client;
pub mod app;
pub mod bootstrap;
pub mod cli;
pub mod migrations;
pub mod realtime_bridge;
pub mod realtime_outbox_dispatcher;
pub mod rtmp_auth;
pub mod server;
pub mod shutdown;

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
