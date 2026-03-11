//! Bilibili API Implementation
//!
//! Unified implementation for all Bilibili API operations.
//! Used by both HTTP and gRPC handlers.

use crate::proto::providers::bilibili::{
    CaptchaResponse, CheckQrRequest, GetCaptchaRequest, LoginQrRequest, LoginSmsRequest,
    LoginSmsResponse, LogoutRequest, LogoutResponse, ParseRequest, ParseResponse, QrCodeResponse,
    QrStatusResponse, SendSmsRequest, SendSmsResponse, UserInfoRequest, UserInfoResponse,
    VideoInfo,
};
use std::collections::HashMap;
use std::sync::Arc;
use synctv_core::models::{ProviderCredential, UserProviderCredential};
use synctv_core::provider::BilibiliProvider;
use synctv_core::repository::UserProviderCredentialRepository;

use super::resolve_bound_instance_name;

/// Bilibili API implementation
///
/// Contains all business logic for Bilibili operations.
/// Methods accept grpc-generated request types and return grpc-generated response types.
#[derive(Clone)]
pub struct BilibiliApiImpl {
    provider: Arc<BilibiliProvider>,
    credential_repo: Arc<UserProviderCredentialRepository>,
}

impl BilibiliApiImpl {
    #[must_use]
    pub const fn new(
        provider: Arc<BilibiliProvider>,
        credential_repo: Arc<UserProviderCredentialRepository>,
    ) -> Self {
        Self {
            provider,
            credential_repo,
        }
    }

    /// Resolve Bilibili cookies from DB using server_id.
    async fn resolve_cookies(
        &self,
        caller_user_id: &str,
        server_id: &str,
        requested_instance_name: Option<&str>,
    ) -> Result<(HashMap<String, String>, Option<String>), synctv_core::provider::ProviderError> {
        let server_id = if server_id.is_empty()
            || (server_id == UserProviderCredential::BILIBILI_SERVER_ID
                && requested_instance_name.is_some())
        {
            UserProviderCredential::bilibili_server_id(requested_instance_name)
        } else {
            server_id.to_string()
        };

        let cred = self
            .credential_repo
            .get_by_provider_and_server(
                caller_user_id,
                synctv_core::provider::BilibiliProvider::NAME,
                &server_id,
            )
            .await
            .map_err(|e| {
                synctv_core::provider::ProviderError::Internal(format!(
                    "Failed to query bilibili credential: {e}"
                ))
            })?
            .ok_or(synctv_core::provider::ProviderError::CredentialNotFound(
                format!("No bilibili credential found for server_id '{server_id}'"),
            ))?;

        if cred.is_expired() {
            return Err(synctv_core::provider::ProviderError::CredentialExpired(
                "Bilibili credential has expired".to_string(),
            ));
        }

        let effective_instance_name = resolve_bound_instance_name(
            requested_instance_name,
            cred.provider_instance_name.as_deref(),
        )?;

        match cred.get_credential() {
            Ok(ProviderCredential::Bilibili { cookies }) => Ok((cookies, effective_instance_name)),
            Ok(_) => Err(synctv_core::provider::ProviderError::InvalidCredentialType),
            Err(e) => Err(synctv_core::provider::ProviderError::Internal(format!(
                "Failed to parse bilibili credential: {e}"
            ))),
        }
    }

    /// Persist Bilibili cookies as a credential.
    async fn persist_cookies(
        &self,
        caller_user_id: &str,
        cookies: &HashMap<String, String>,
        instance_name: Option<&str>,
    ) -> Result<String, synctv_core::provider::ProviderError> {
        let server_id = UserProviderCredential::bilibili_server_id(instance_name);
        let credential_data = ProviderCredential::bilibili(cookies.clone());

        // Upsert: delete existing then create
        if let Some(existing) = self
            .credential_repo
            .get_by_provider_and_server(
                caller_user_id,
                synctv_core::provider::BilibiliProvider::NAME,
                &server_id,
            )
            .await
            .ok()
            .flatten()
        {
            let _ = self.credential_repo.delete(&existing.id).await;
        }

        let credential = UserProviderCredential {
            id: nanoid::nanoid!(),
            user_id: caller_user_id.to_string(),
            provider: synctv_core::provider::BilibiliProvider::NAME.to_string(),
            server_id: server_id.clone(),
            provider_instance_name: instance_name.map(ToString::to_string),
            credential_data: serde_json::to_value(&credential_data).map_err(|e| {
                synctv_core::provider::ProviderError::Internal(format!(
                    "Failed to serialize credential: {e}"
                ))
            })?,
            expires_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        self.credential_repo
            .create(&credential)
            .await
            .map_err(|e| {
                synctv_core::provider::ProviderError::Internal(format!(
                    "Failed to persist bilibili credential: {e}"
                ))
            })?;

        Ok(server_id)
    }

    /// Parse Bilibili URL using stored cookies
    pub async fn parse(
        &self,
        caller_user_id: &str,
        req: ParseRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ParseResponse, synctv_core::provider::ProviderError> {
        let (cookies, effective_instance_name) = self
            .resolve_cookies(caller_user_id, &req.server_id, requested_instance_name)
            .await?;

        // Step 1: Match URL
        let match_resp = self
            .provider
            .r#match(req.url.clone(), effective_instance_name.as_deref())
            .await?;

        // Step 2: Parse based on type
        let page_info = match match_resp.r#type.as_str() {
            "video" | "bv" | "av" => {
                let parse_req = synctv_media_providers::grpc::bilibili::ParseVideoPageReq {
                    cookies: cookies.clone(),
                    bvid: if match_resp.r#type == "bv" {
                        match_resp.id.clone()
                    } else {
                        String::new()
                    },
                    aid: if match_resp.r#type == "av" {
                        match_resp.id.parse().unwrap_or(0)
                    } else {
                        0
                    },
                    sections: false,
                };

                self.provider
                    .parse_video_page(parse_req, effective_instance_name.as_deref())
                    .await?
            }
            "pgc" | "ep" | "ss" => {
                let parse_req = synctv_media_providers::grpc::bilibili::ParsePgcPageReq {
                    cookies: cookies.clone(),
                    epid: if match_resp.r#type == "ep" {
                        match_resp.id.parse().unwrap_or(0)
                    } else {
                        0
                    },
                    ssid: if match_resp.r#type == "ss" {
                        match_resp.id.parse().unwrap_or(0)
                    } else {
                        0
                    },
                };

                self.provider
                    .parse_pgc_page(parse_req, effective_instance_name.as_deref())
                    .await?
            }
            "live" => {
                let parse_req = synctv_media_providers::grpc::bilibili::ParseLivePageReq {
                    cookies: cookies.clone(),
                    room_id: match_resp.id.parse().unwrap_or(0),
                };

                self.provider
                    .parse_live_page(parse_req, effective_instance_name.as_deref())
                    .await?
            }
            _ => {
                return Err(synctv_core::provider::ProviderError::UnsupportedFormat(
                    format!("Unsupported URL type: {}", match_resp.r#type),
                ));
            }
        };

        // Convert to response format
        let videos: Vec<VideoInfo> = page_info
            .video_infos
            .into_iter()
            .map(|v| VideoInfo {
                bvid: v.bvid,
                cid: v.cid as i64,
                epid: v.epid as i64,
                name: v.name,
                cover: v.cover_image,
                is_live: v.live,
            })
            .collect();

        let actors = if page_info.actors.is_empty() {
            vec![]
        } else {
            page_info
                .actors
                .split(',')
                .map(|s| s.trim().to_string())
                .collect()
        };

        Ok(ParseResponse {
            title: page_info.title,
            actors,
            videos,
        })
    }

    /// Generate QR code for login
    pub async fn login_qr(
        &self,
        _req: LoginQrRequest,
        instance_name: Option<&str>,
    ) -> Result<QrCodeResponse, synctv_core::provider::ProviderError> {
        let resp = self.provider.new_qr_code(instance_name).await?;

        Ok(QrCodeResponse {
            url: resp.url,
            key: resp.key,
        })
    }

    /// Check QR code login status. On success, persist cookies server-side.
    pub async fn check_qr(
        &self,
        caller_user_id: &str,
        req: CheckQrRequest,
        instance_name: Option<&str>,
    ) -> Result<QrStatusResponse, synctv_core::provider::ProviderError> {
        let check_req = synctv_media_providers::grpc::bilibili::LoginWithQrCodeReq { key: req.key };

        let resp = self
            .provider
            .login_with_qr_code(check_req, instance_name)
            .await?;

        // If login succeeded (status = 4 = SUCCESS), persist cookies
        let server_id = if resp.status == 4 && !resp.cookies.is_empty() {
            self.persist_cookies(caller_user_id, &resp.cookies, instance_name)
                .await?
        } else {
            String::new()
        };

        Ok(QrStatusResponse {
            status: resp.status,
            server_id,
        })
    }

    /// Get captcha for SMS login
    pub async fn get_captcha(
        &self,
        _req: GetCaptchaRequest,
        instance_name: Option<&str>,
    ) -> Result<CaptchaResponse, synctv_core::provider::ProviderError> {
        let resp = self.provider.new_captcha(instance_name).await?;

        Ok(CaptchaResponse {
            token: resp.token,
            gt: resp.gt,
            challenge: resp.challenge,
        })
    }

    /// Send SMS verification code
    pub async fn send_sms(
        &self,
        req: SendSmsRequest,
        instance_name: Option<&str>,
    ) -> Result<SendSmsResponse, synctv_core::provider::ProviderError> {
        let sms_req = synctv_media_providers::grpc::bilibili::NewSmsReq {
            phone: req.phone,
            token: req.token,
            challenge: req.challenge,
            validate: req.validate,
        };

        let resp = self.provider.new_sms(sms_req, instance_name).await?;

        Ok(SendSmsResponse {
            captcha_key: resp.captcha_key,
        })
    }

    /// Login with SMS code. On success, persist cookies server-side.
    pub async fn login_sms(
        &self,
        caller_user_id: &str,
        req: LoginSmsRequest,
        instance_name: Option<&str>,
    ) -> Result<LoginSmsResponse, synctv_core::provider::ProviderError> {
        let login_req = synctv_media_providers::grpc::bilibili::LoginWithSmsReq {
            phone: req.phone,
            code: req.code,
            captcha_key: req.captcha_key,
        };

        let resp = self
            .provider
            .login_with_sms(login_req, instance_name)
            .await?;

        // Persist cookies server-side
        let server_id = self
            .persist_cookies(caller_user_id, &resp.cookies, instance_name)
            .await?;

        Ok(LoginSmsResponse { server_id })
    }

    /// Get user info using stored cookies
    pub async fn get_user_info(
        &self,
        caller_user_id: &str,
        req: UserInfoRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<UserInfoResponse, synctv_core::provider::ProviderError> {
        let (cookies, effective_instance_name) = self
            .resolve_cookies(caller_user_id, &req.server_id, requested_instance_name)
            .await?;

        let info_req = synctv_media_providers::grpc::bilibili::UserInfoReq { cookies };

        let resp = self
            .provider
            .user_info(info_req, effective_instance_name.as_deref())
            .await?;

        Ok(UserInfoResponse {
            is_login: resp.is_login,
            username: resp.username,
            face: resp.face,
            is_vip: resp.is_vip,
        })
    }

    /// Logout and delete stored credential
    pub async fn logout(
        &self,
        caller_user_id: &str,
        req: LogoutRequest,
    ) -> Result<LogoutResponse, synctv_core::provider::ProviderError> {
        let default_server_id = UserProviderCredential::bilibili_server_id(None);
        let server_id = if req.server_id.is_empty() {
            default_server_id.as_str()
        } else {
            &req.server_id
        };

        if let Some(existing) = self
            .credential_repo
            .get_by_provider_and_server(
                caller_user_id,
                synctv_core::provider::BilibiliProvider::NAME,
                server_id,
            )
            .await
            .map_err(|e| {
                synctv_core::provider::ProviderError::Internal(format!(
                    "Failed to query credential: {e}"
                ))
            })?
        {
            self.credential_repo
                .delete(&existing.id)
                .await
                .map_err(|e| {
                    synctv_core::provider::ProviderError::Internal(format!(
                        "Failed to delete credential: {e}"
                    ))
                })?;
        }

        Ok(LogoutResponse {
            message: "Logout successful".to_string(),
        })
    }
}
