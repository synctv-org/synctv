//! Bilibili API Implementation
//!
//! Unified implementation for all Bilibili API operations.
//! Used by both HTTP and gRPC handlers.

use crate::proto::providers::bilibili::{
    BindInfo, CaptchaResponse, CheckQrRequest, GetBindsResponse, GetCaptchaRequest, LoginQrRequest,
    LoginSmsRequest, LoginSmsResponse, LogoutRequest, LogoutResponse, ParseRequest, ParseResponse,
    QrCodeResponse, QrStatusResponse, SendSmsRequest, SendSmsResponse, UserInfoRequest,
    UserInfoResponse, VideoInfo,
};
use std::collections::HashMap;
use std::sync::Arc;
use synctv_core::models::{ProviderCredential, UserId, UserProviderCredential};
use synctv_core::provider::{BilibiliProvider, ExecutionControl};
use synctv_core::repository::UserProviderCredentialRepository;

use super::{get_provider_credentials, publish_provider_credential_changed};

/// Bilibili API implementation
///
/// Contains all business logic for Bilibili operations.
/// Methods accept grpc-generated request types and return grpc-generated response types.
#[derive(Clone)]
pub struct BilibiliApiImpl {
    provider: Arc<BilibiliProvider>,
    credential_repo: Arc<UserProviderCredentialRepository>,
    event_service: Option<Arc<dyn crate::runtime::RealtimeEventService>>,
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
            event_service: None,
        }
    }

    #[must_use]
    pub fn with_event_service(
        mut self,
        event_service: Option<Arc<dyn crate::runtime::RealtimeEventService>>,
    ) -> Self {
        self.event_service = event_service;
        self
    }

    /// Resolve the user's single global Bilibili credential.
    async fn resolve_cookies(
        &self,
        caller_user_id: &UserId,
    ) -> Result<Option<HashMap<String, String>>, synctv_core::provider::ProviderError> {
        let server_id = UserProviderCredential::bilibili_server_id();
        let cred = self
            .credential_repo
            .get_by_provider_and_server(
                *caller_user_id,
                synctv_core::provider::BilibiliProvider::NAME,
                &server_id,
            )
            .await
            .map_err(|e| {
                synctv_core::provider::ProviderError::Internal(format!(
                    "Failed to query bilibili credential: {e}"
                ))
            })?;

        let Some(cred) = cred else {
            return Ok(None);
        };

        if cred.is_expired() {
            return Ok(None);
        }

        match cred.get_credential() {
            Ok(ProviderCredential::Bilibili { cookies }) => Ok(Some(cookies)),
            Ok(_) => Err(synctv_core::provider::ProviderError::InvalidCredentialType),
            Err(e) => Err(synctv_core::provider::ProviderError::Internal(format!(
                "Failed to parse bilibili credential: {e}"
            ))),
        }
    }

    /// Persist Bilibili cookies as a credential.
    async fn persist_cookies(
        &self,
        caller_user_id: &UserId,
        cookies: &HashMap<String, String>,
    ) -> Result<String, synctv_core::provider::ProviderError> {
        let server_id = UserProviderCredential::bilibili_server_id();
        let credential_data = ProviderCredential::bilibili(cookies.clone());

        let credential = UserProviderCredential {
            id: 0,
            user_id: *caller_user_id,
            provider: synctv_core::provider::BilibiliProvider::NAME.to_string(),
            server_id: server_id.clone(),
            provider_instance_name: None,
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
            .upsert_by_user_provider_server(&credential)
            .await
            .map_err(|e| {
                synctv_core::provider::ProviderError::Internal(format!(
                    "Failed to persist bilibili credential: {e}"
                ))
            })?;

        publish_provider_credential_changed(
            self.event_service.as_ref(),
            *caller_user_id,
            synctv_core::provider::BilibiliProvider::NAME,
            &server_id,
        );

        Ok(server_id)
    }

    /// Parse Bilibili URL using stored cookies
    pub async fn parse(
        &self,
        caller_user_id: &UserId,
        req: ParseRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ParseResponse, synctv_core::provider::ProviderError> {
        self.parse_with_context(caller_user_id, req, requested_instance_name, None)
            .await
    }

    pub async fn parse_with_context(
        &self,
        caller_user_id: &UserId,
        req: ParseRequest,
        requested_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<ParseResponse, synctv_core::provider::ProviderError> {
        let cookies = self
            .resolve_cookies(caller_user_id)
            .await?
            .unwrap_or_default();

        // Step 1: Match URL
        let match_resp = self
            .provider
            .r#match_with_context(req.url.clone(), requested_instance_name, request_context)
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
                    .parse_video_page_with_context(
                        parse_req,
                        requested_instance_name,
                        request_context,
                    )
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
                    .parse_pgc_page_with_context(
                        parse_req,
                        requested_instance_name,
                        request_context,
                    )
                    .await?
            }
            "live" => {
                let parse_req = synctv_media_providers::grpc::bilibili::ParseLivePageReq {
                    cookies: cookies.clone(),
                    room_id: match_resp.id.parse().unwrap_or(0),
                };

                self.provider
                    .parse_live_page_with_context(
                        parse_req,
                        requested_instance_name,
                        request_context,
                    )
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
            .map(|v| {
                let cid = i64::try_from(v.cid).map_err(|_| {
                    synctv_core::provider::ProviderError::ParseError(format!(
                        "Bilibili cid {} exceeds int64 range",
                        v.cid
                    ))
                })?;
                let epid = i64::try_from(v.epid).map_err(|_| {
                    synctv_core::provider::ProviderError::ParseError(format!(
                        "Bilibili epid {} exceeds int64 range",
                        v.epid
                    ))
                })?;

                Ok::<_, synctv_core::provider::ProviderError>(VideoInfo {
                    bvid: v.bvid,
                    cid,
                    epid,
                    name: v.name,
                    cover: v.cover_image,
                    is_live: v.live,
                })
            })
            .collect::<Result<_, _>>()?;

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
        req: LoginQrRequest,
        instance_name: Option<&str>,
    ) -> Result<QrCodeResponse, synctv_core::provider::ProviderError> {
        self.login_qr_with_context(req, instance_name, None).await
    }

    pub async fn login_qr_with_context(
        &self,
        _req: LoginQrRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<QrCodeResponse, synctv_core::provider::ProviderError> {
        let resp = self
            .provider
            .new_qr_code_with_context(instance_name, request_context)
            .await?;

        Ok(QrCodeResponse {
            url: resp.url,
            key: resp.key,
        })
    }

    /// Check QR code login status. On success, persist cookies server-side.
    pub async fn check_qr(
        &self,
        caller_user_id: &UserId,
        req: CheckQrRequest,
        instance_name: Option<&str>,
    ) -> Result<QrStatusResponse, synctv_core::provider::ProviderError> {
        self.check_qr_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    pub async fn check_qr_with_context(
        &self,
        caller_user_id: &UserId,
        req: CheckQrRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<QrStatusResponse, synctv_core::provider::ProviderError> {
        let check_req = synctv_media_providers::grpc::bilibili::LoginWithQrCodeReq { key: req.key };

        let resp = self
            .provider
            .login_with_qr_code_with_context(check_req, instance_name, request_context)
            .await?;

        // If login succeeded (status = 4 = SUCCESS), persist cookies
        if resp.status == 4 && !resp.cookies.is_empty() {
            self.persist_cookies(caller_user_id, &resp.cookies).await?;
        }

        Ok(QrStatusResponse {
            status: resp.status,
        })
    }

    /// Get captcha for SMS login
    pub async fn get_captcha(
        &self,
        req: GetCaptchaRequest,
        instance_name: Option<&str>,
    ) -> Result<CaptchaResponse, synctv_core::provider::ProviderError> {
        self.get_captcha_with_context(req, instance_name, None)
            .await
    }

    pub async fn get_captcha_with_context(
        &self,
        _req: GetCaptchaRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<CaptchaResponse, synctv_core::provider::ProviderError> {
        let resp = self
            .provider
            .new_captcha_with_context(instance_name, request_context)
            .await?;

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
        self.send_sms_with_context(req, instance_name, None).await
    }

    pub async fn send_sms_with_context(
        &self,
        req: SendSmsRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<SendSmsResponse, synctv_core::provider::ProviderError> {
        let sms_req = synctv_media_providers::grpc::bilibili::NewSmsReq {
            phone: req.phone,
            token: req.token,
            challenge: req.challenge,
            validate: req.validate,
        };

        let resp = self
            .provider
            .new_sms_with_context(sms_req, instance_name, request_context)
            .await?;

        Ok(SendSmsResponse {
            captcha_key: resp.captcha_key,
        })
    }

    /// Login with SMS code. On success, persist cookies server-side.
    pub async fn login_sms(
        &self,
        caller_user_id: &UserId,
        req: LoginSmsRequest,
        instance_name: Option<&str>,
    ) -> Result<LoginSmsResponse, synctv_core::provider::ProviderError> {
        self.login_sms_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    pub async fn login_sms_with_context(
        &self,
        caller_user_id: &UserId,
        req: LoginSmsRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<LoginSmsResponse, synctv_core::provider::ProviderError> {
        let login_req = synctv_media_providers::grpc::bilibili::LoginWithSmsReq {
            phone: req.phone,
            code: req.code,
            captcha_key: req.captcha_key,
        };

        let resp = self
            .provider
            .login_with_sms_with_context(login_req, instance_name, request_context)
            .await?;

        // Persist cookies server-side
        self.persist_cookies(caller_user_id, &resp.cookies).await?;

        Ok(LoginSmsResponse {})
    }

    /// Get user info using stored cookies
    pub async fn get_user_info(
        &self,
        caller_user_id: &UserId,
        req: UserInfoRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<UserInfoResponse, synctv_core::provider::ProviderError> {
        self.get_user_info_with_context(caller_user_id, req, requested_instance_name, None)
            .await
    }

    pub async fn get_user_info_with_context(
        &self,
        caller_user_id: &UserId,
        _req: UserInfoRequest,
        requested_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<UserInfoResponse, synctv_core::provider::ProviderError> {
        let Some(cookies) = self.resolve_cookies(caller_user_id).await? else {
            return Ok(UserInfoResponse {
                is_login: false,
                username: String::new(),
                face: String::new(),
                is_vip: false,
            });
        };

        let info_req = synctv_media_providers::grpc::bilibili::UserInfoReq { cookies };

        let resp = self
            .provider
            .user_info_with_context(info_req, requested_instance_name, request_context)
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
        caller_user_id: &UserId,
        _req: LogoutRequest,
    ) -> Result<LogoutResponse, synctv_core::provider::ProviderError> {
        let server_id = UserProviderCredential::bilibili_server_id();
        if let Some(existing) = self
            .credential_repo
            .get_by_provider_and_server(
                *caller_user_id,
                synctv_core::provider::BilibiliProvider::NAME,
                &server_id,
            )
            .await
            .map_err(|e| {
                synctv_core::provider::ProviderError::Internal(format!(
                    "Failed to query credential: {e}"
                ))
            })?
        {
            self.credential_repo
                .delete(existing.id)
                .await
                .map_err(|e| {
                    synctv_core::provider::ProviderError::Internal(format!(
                        "Failed to delete credential: {e}"
                    ))
                })?;
            publish_provider_credential_changed(
                self.event_service.as_ref(),
                *caller_user_id,
                synctv_core::provider::BilibiliProvider::NAME,
                &server_id,
            );
        }

        Ok(LogoutResponse {
            message: "Logout successful".to_string(),
        })
    }

    pub async fn get_binds(
        &self,
        caller_user_id: &UserId,
        _instance_name: Option<&str>,
    ) -> Result<GetBindsResponse, crate::impls::ApiError> {
        let server_id = UserProviderCredential::bilibili_server_id();
        let binds = get_provider_credentials(
            &self.credential_repo,
            caller_user_id,
            synctv_core::provider::BilibiliProvider::NAME,
            None,
        )
        .await?
        .into_iter()
        .filter(|credential| credential.server_id == server_id)
        .map(|credential| BindInfo {
            id: credential.id.to_string(),
            server_id: credential.server_id,
            created_at: credential.created_at.timestamp(),
        })
        .collect();

        Ok(GetBindsResponse { binds })
    }
}

#[cfg(test)]
mod tests {
    use super::BilibiliApiImpl;
    use std::sync::Arc;
    use synctv_core::provider::BilibiliProvider;
    use synctv_core::repository::{ProviderInstanceRepository, UserProviderCredentialRepository};
    use synctv_core::service::RemoteProviderManager;
    use synctv_core_testing::create_test_pool;

    fn provider() -> Arc<BilibiliProvider> {
        let pool = sqlx::PgPool::connect_lazy("postgresql://fake").expect("lazy pool");
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        Arc::new(BilibiliProvider::new(Arc::new(RemoteProviderManager::new(
            repo,
        ))))
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn get_user_info_without_binding_reports_logged_out() {
        let (_postgres, pool) = create_test_pool().await;
        let api = BilibiliApiImpl::new(
            provider(),
            Arc::new(UserProviderCredentialRepository::new(pool)),
        );

        let response = api
            .get_user_info(
                &synctv_core::models::UserId::new(),
                crate::proto::providers::bilibili::UserInfoRequest {
                    instance_name: String::new(),
                },
                None,
            )
            .await
            .expect("missing Bilibili binding should be a logged-out state");

        assert!(!response.is_login);
        assert!(response.username.is_empty());
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn logout_without_binding_is_idempotent() {
        let (_postgres, pool) = create_test_pool().await;
        let api = BilibiliApiImpl::new(
            provider(),
            Arc::new(UserProviderCredentialRepository::new(pool)),
        );

        let response = api
            .logout(
                &synctv_core::models::UserId::new(),
                crate::proto::providers::bilibili::LogoutRequest {
                    instance_name: String::new(),
                },
            )
            .await
            .expect("logout without a Bilibili binding should be idempotent");

        assert_eq!(response.message, "Logout successful");
    }
}
