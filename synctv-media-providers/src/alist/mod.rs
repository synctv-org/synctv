//! Alist Provider Client
//!
//! Pure HTTP client for Alist API, independent of `MediaProvider`.
//! Can be used as a standalone library or as a `provider_instance`.
//!
//! # Example
//!
//! ```no_run
//! use synctv_media_providers::alist::AlistClient;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let mut client = AlistClient::new("https://alist.example.com")?;
//! let token = client.login("username", "password", false).await?;
//! let file_info = client.fs_get("/movies/video.mp4", None).await?;
//! # Ok(())
//! # }
//! ```

mod client;
pub mod error;
pub mod service;
pub mod types;

pub use client::AlistClient;
pub use error::AlistError;
pub use service::{AlistInterface, AlistService};
pub use types::*;
