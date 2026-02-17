//! Bilibili Provider Client
//!
//! Pure HTTP client for Bilibili API, independent of `MediaProvider`.
//!
//! # Features
//! - Video parsing (BVID/EPID extraction)
//! - Quality selection
//! - Short link resolution
//! - Anti-crawler handling

pub mod client;
pub mod error;
pub mod service;
pub mod types;

pub use client::BilibiliClient;
pub use client::{ApiRateLimiter, WbiKeyCache, set_shared_rate_limiter, set_shared_wbi_cache};
pub use error::BilibiliError;
pub use service::{BilibiliInterface, BilibiliService};
pub use types::*;
