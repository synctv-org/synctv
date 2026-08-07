//! Remote provider transport helpers.
//!
//! This module owns remote provider connection setup, wire-level request
//! metadata, timeout handling, compression, and remote trait adapters. Keep
//! provider database/domain records outside this module; callers should map
//! them into transport connection options first.

mod clients;
mod compression;
mod connection;
mod connector;
mod endpoint;
mod execution;
mod request;

pub use clients::{
    create_remote_alist_client, create_remote_bilibili_client, create_remote_emby_client,
};
pub(crate) use compression::apply_provider_client_compression;
pub use connection::{
    create_remote_connection, RemoteProviderConnection, RemoteProviderConnectionOptions,
    RemoteProviderTransportConfig,
};
pub use endpoint::{normalized_transport_endpoint, required_auth_secret, validate_endpoint_ssrf};
pub(crate) use execution::execute_remote_call;
pub(crate) use request::build_remote_request;
pub use request::{execute_health_check, validate_auth_secret};
