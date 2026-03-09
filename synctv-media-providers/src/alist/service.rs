//! Alist Service - Complete implementation
//!
//! This is the full HTTP client implementation.
//! Both gRPC server and local usage call this service.

use super::{AlistClient, AlistError};
use crate::grpc::alist::{
    FsGetReq, FsGetResp, FsListReq, FsListResp, FsOtherReq, FsOtherResp, FsSearchReq, FsSearchResp,
    LoginReq, MeReq, MeResp,
};
use async_trait::async_trait;
use reqwest::Client;

/// Unified Alist service interface
///
/// This trait defines all Alist operations using proto request/response types.
/// This eliminates manual parameter binding - just pass proto requests directly.
#[async_trait]
pub trait AlistInterface: Send + Sync {
    async fn fs_get(&self, request: FsGetReq) -> Result<FsGetResp, AlistError>;

    async fn fs_list(&self, request: FsListReq) -> Result<FsListResp, AlistError>;

    async fn fs_other(&self, request: FsOtherReq) -> Result<FsOtherResp, AlistError>;

    async fn fs_search(&self, request: FsSearchReq) -> Result<FsSearchResp, AlistError>;

    async fn me(&self, request: MeReq) -> Result<MeResp, AlistError>;

    async fn login(&self, request: LoginReq) -> Result<String, AlistError>;
}

/// Alist service implementation
///
/// This is the complete implementation that makes actual HTTP calls.
/// Used by both local callers and gRPC server.
pub struct AlistService {
    client: Client,
}

impl AlistService {
    pub fn new() -> Self {
        Self {
            client: synctv_common::http::build_provider_client()
                .expect("default provider HTTP client should build"),
        }
    }

    #[must_use]
    pub fn with_client(client: Client) -> Self {
        Self { client }
    }
}

impl Default for AlistService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AlistInterface for AlistService {
    async fn fs_get(&self, request: FsGetReq) -> Result<FsGetResp, AlistError> {
        let client = AlistClient::with_token_and_http_client(
            &request.host,
            &request.token,
            self.client.clone(),
        )?;
        let password = if request.password.is_empty() {
            None
        } else {
            Some(request.password.as_str())
        };
        let http_resp = client.fs_get(&request.path, password).await?;

        Ok(http_resp.into())
    }

    async fn fs_list(&self, request: FsListReq) -> Result<FsListResp, AlistError> {
        let client = AlistClient::with_token_and_http_client(
            &request.host,
            &request.token,
            self.client.clone(),
        )?;
        let password = if request.password.is_empty() {
            None
        } else {
            Some(request.password.as_str())
        };
        let http_resp = client
            .fs_list(&request.path, request.page, request.per_page, password)
            .await?;

        Ok(http_resp.into())
    }

    async fn fs_other(&self, request: FsOtherReq) -> Result<FsOtherResp, AlistError> {
        let client = AlistClient::with_token_and_http_client(
            &request.host,
            &request.token,
            self.client.clone(),
        )?;
        let password = if request.password.is_empty() {
            None
        } else {
            Some(request.password.as_str())
        };
        let http_resp = client
            .fs_other(&request.path, &request.method, password)
            .await?;

        Ok(http_resp.into())
    }

    async fn fs_search(&self, request: FsSearchReq) -> Result<FsSearchResp, AlistError> {
        let client = AlistClient::with_token_and_http_client(
            &request.host,
            &request.token,
            self.client.clone(),
        )?;
        let password = if request.password.is_empty() {
            None
        } else {
            Some(request.password.as_str())
        };
        let http_resp = client
            .fs_search(
                &request.parent,
                &request.keywords,
                request.scope,
                request.page,
                request.per_page,
                password,
            )
            .await?;

        Ok(http_resp.into())
    }

    async fn me(&self, request: MeReq) -> Result<MeResp, AlistError> {
        let client = AlistClient::with_token_and_http_client(
            &request.host,
            &request.token,
            self.client.clone(),
        )?;
        let http_resp = client.me().await?;

        Ok(http_resp.into())
    }

    async fn login(&self, request: LoginReq) -> Result<String, AlistError> {
        let mut client = AlistClient::with_http_client(&request.host, self.client.clone())?;
        client
            .login(&request.username, &request.password, request.hashed)
            .await
    }
}
