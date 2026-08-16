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

pub use app::{Application, ApplicationBuildOptions, ApplicationPreboundListeners};

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    pub(crate) fn acquire_process_state_lock() -> MutexGuard<'static, ()> {
        static PROCESS_STATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        PROCESS_STATE_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
