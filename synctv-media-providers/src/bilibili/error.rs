//! Bilibili Provider Client Error Types
//!
//! `BilibiliError` is a type alias for the shared `ProviderClientError`.
//!
//! Design rationale: All provider clients (Alist, Bilibili, Emby) share an
//! identical set of error variants (Network, Http, Api, Parse, Auth, etc.).
//! Rather than duplicating the enum per provider -- which would require
//! per-provider `From` impls and prevent cross-provider error unification --
//! we define the variants once in `crate::error::ProviderClientError` and
//! re-export a type alias in each provider module. This gives each provider
//! an ergonomic, provider-specific name (`BilibiliError`, `AlistError`)
//! while keeping error handling consistent and zero-cost.

pub use crate::error::{check_response, json_with_limit, ProviderClientError as BilibiliError};
