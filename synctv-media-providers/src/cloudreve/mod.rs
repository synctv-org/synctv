//! Cloudreve v4 HTTP client.

mod client;
mod types;

#[cfg(test)]
mod client_tests;

pub use client::CloudreveClient;
pub use types::{CloudreveFile, CloudreveList, CloudreveToken, CloudreveUrl, CloudreveUser};
