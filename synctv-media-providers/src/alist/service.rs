//! Alist Service - Complete implementation
//!
//! This is the full HTTP client implementation.
//! Both gRPC server and local usage call this service.

use super::{AlistClient, AlistError};
use crate::transport_dto::alist::{
    FsGetReq, FsGetResp, FsListReq, FsListResp, FsOtherReq, FsOtherResp, FsSearchReq, FsSearchResp,
    LoginReq, MeReq, MeResp,
};
use async_trait::async_trait;
use reqwest::Client;

fn non_empty_otp_code(otp_code: &str) -> Option<&str> {
    let trimmed = otp_code.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Map an empty string to `None`, otherwise borrow it as `Some(&str)`.
fn opt_str(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

/// Unified Alist service interface
///
/// This trait defines all Alist operations using provider transport DTOs.
/// Local and remote clients share this boundary.
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
    pub fn new() -> Result<Self, reqwest::Error> {
        let client =
            crate::build_provider_http_client(synctv_common::ssrf::SsrfGuard::strict_policy())?;
        Ok(Self { client })
    }

    #[must_use]
    pub const fn with_client(client: Client) -> Self {
        Self { client }
    }

    fn authenticated_client(&self, host: &str, token: &str) -> Result<AlistClient, AlistError> {
        AlistClient::with_token_and_http_client(host, token, self.client.clone())
    }

    fn anonymous_client(&self, host: &str) -> Result<AlistClient, AlistError> {
        AlistClient::with_http_client(host, self.client.clone())
    }
}

#[async_trait]
impl AlistInterface for AlistService {
    async fn fs_get(&self, request: FsGetReq) -> Result<FsGetResp, AlistError> {
        let client = self.authenticated_client(&request.host, &request.token)?;
        let password = opt_str(&request.password);
        let http_resp = client
            .fs_get(&request.path, password, &request.headers)
            .await?;

        Ok(http_resp.into())
    }

    async fn fs_list(&self, request: FsListReq) -> Result<FsListResp, AlistError> {
        let client = self.authenticated_client(&request.host, &request.token)?;
        let password = opt_str(&request.password);
        let http_resp = client
            .fs_list_with_refresh(
                &request.path,
                request.page,
                request.per_page,
                password,
                request.refresh,
            )
            .await?;

        Ok(http_resp.into())
    }

    async fn fs_other(&self, request: FsOtherReq) -> Result<FsOtherResp, AlistError> {
        let client = self.authenticated_client(&request.host, &request.token)?;
        let password = opt_str(&request.password);
        let http_resp = client
            .fs_other(&request.path, &request.method, password)
            .await?;

        Ok(http_resp.into())
    }

    async fn fs_search(&self, request: FsSearchReq) -> Result<FsSearchResp, AlistError> {
        let client = self.authenticated_client(&request.host, &request.token)?;
        let password = opt_str(&request.password);
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
        let client = self.authenticated_client(&request.host, &request.token)?;
        let http_resp = client.me().await?;

        Ok(http_resp.into())
    }

    async fn login(&self, request: LoginReq) -> Result<String, AlistError> {
        let mut client = self.anonymous_client(&request.host)?;
        match request.credential {
            Some(crate::transport_dto::alist::login_req::Credential::Password(password)) => {
                client
                    .login_with_otp(
                        &request.username,
                        &password,
                        false,
                        non_empty_otp_code(&request.otp_code),
                    )
                    .await
            }
            Some(crate::transport_dto::alist::login_req::Credential::HashedPassword(
                hashed_password,
            )) => {
                client
                    .login_with_otp(
                        &request.username,
                        &hashed_password,
                        true,
                        non_empty_otp_code(&request.otp_code),
                    )
                    .await
            }
            None => Err(AlistError::InvalidConfig(
                "exactly one of password or hashed_password must be provided".to_string(),
            )),
        }
    }
}
