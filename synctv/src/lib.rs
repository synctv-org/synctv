//! SyncTV server library.
//!
//! This crate provides the main server implementation for SyncTV.
//! It is primarily intended to be used as a binary, but exposes
//! internal modules for testing purposes.

pub mod app;
pub mod bootstrap;
pub mod cluster_bridge;
pub mod migrations;
pub mod outbox_dispatcher;
pub mod rtmp_auth;
pub mod server;
pub mod shutdown;
