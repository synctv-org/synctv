//! Bilibili API Implementation
//!
//! Unified implementation for all Bilibili API operations.
//! Used by both HTTP and gRPC handlers.

use crate::proto::providers::bilibili::{
    BindInfo, CheckQrRequest, GetBindsResponse, LoginQrRequest, LoginSmsRequest, LoginSmsResponse,
    LogoutRequest, LogoutResponse, ParseRequest, ParseResponse, QrCodeResponse, QrStatusResponse,
    SendSmsRequest, SendSmsResponse, StartSmsLoginRequest, StartSmsLoginResponse, UserInfoRequest,
    UserInfoResponse, VideoInfo,
};
use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::{
    aead::{Aead, KeyInit as AeadKeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, KeyInit as HmacKeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use synctv_core::models::{
    normalize_provider_instance_name, resolve_provider_instance_binding,
    CredentialProviderInstanceName, ProviderCredential, UserId, UserProviderCredential,
};
use synctv_core::provider::{BilibiliProvider, ExecutionControl, ProviderAccessService};
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
    access_service: Option<Arc<dyn ProviderAccessService>>,
    event_service: Option<Arc<dyn crate::runtime::RealtimeEventService>>,
    sms_login_token_codec: Arc<BilibiliSmsLoginTokenCodec>,
    qr_login_status_cache: Arc<moka::sync::Cache<String, BilibiliQrLoginStatusSnapshot>>,
}

struct ResolvedBilibiliCredential {
    cookies: HashMap<String, String>,
    provider_instance_name: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct BilibiliSmsLoginSession {
    token: String,
    challenge: String,
    phone: Option<String>,
    captcha_key: Option<String>,
    instance_name: Option<String>,
    expires_at: i64,
}

#[derive(Clone)]
struct BilibiliQrLoginStatusSnapshot {
    status: i32,
}

const SMS_LOGIN_SESSION_TTL_SECONDS: i64 = 10 * 60;
const SMS_LOGIN_SESSION_VERSION: &str = "v2";
const SMS_LOGIN_DOMAIN_SEPARATOR: &[u8] = b"synctv-bilibili-sms-login";
const SMS_LOGIN_TOKEN_NONCE_SIZE: usize = 12;
const QR_LOGIN_STATUS_CACHE_TTL_SECONDS: u64 = 2;
type HmacSha256 = Hmac<Sha256>;

struct BilibiliSmsLoginTokenCodec {
    cipher: Aes256Gcm,
}

impl BilibiliSmsLoginTokenCodec {
    fn derive_from(secret: &[u8]) -> Self {
        let mut derivation_mac =
            HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
        derivation_mac.update(SMS_LOGIN_DOMAIN_SEPARATOR);
        let derived = derivation_mac.finalize().into_bytes();
        let key = Key::<Aes256Gcm>::from_slice(&derived);
        Self {
            cipher: Aes256Gcm::new(key),
        }
    }

    fn encode(
        &self,
        session: &BilibiliSmsLoginSession,
    ) -> Result<String, synctv_core::provider::ProviderError> {
        let payload = serde_json::to_vec(session).map_err(|e| {
            synctv_core::provider::ProviderError::Internal(format!(
                "Failed to serialize Bilibili SMS login session: {e}"
            ))
        })?;
        let mut nonce_bytes = [0_u8; SMS_LOGIN_TOKEN_NONCE_SIZE];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self.cipher.encrypt(nonce, payload.as_ref()).map_err(|_| {
            synctv_core::provider::ProviderError::Internal(
                "Failed to encrypt Bilibili SMS login session".to_string(),
            )
        })?;
        let mut token = Vec::with_capacity(SMS_LOGIN_TOKEN_NONCE_SIZE + ciphertext.len());
        token.extend_from_slice(&nonce_bytes);
        token.extend_from_slice(&ciphertext);

        Ok(format!(
            "{SMS_LOGIN_SESSION_VERSION}.{}",
            URL_SAFE_NO_PAD.encode(token)
        ))
    }

    fn decode(
        &self,
        session_token: &str,
    ) -> Result<BilibiliSmsLoginSession, synctv_core::provider::ProviderError> {
        let invalid = || {
            synctv_core::provider::ProviderError::Authentication(
                "Bilibili SMS login session is invalid or expired".to_string(),
            )
        };
        let mut parts = session_token.split('.');
        let version = parts.next().ok_or_else(invalid)?;
        let token = parts.next().ok_or_else(invalid)?;
        if version != SMS_LOGIN_SESSION_VERSION || parts.next().is_some() {
            return Err(invalid());
        }

        let token = URL_SAFE_NO_PAD.decode(token).map_err(|_| invalid())?;
        if token.len() <= SMS_LOGIN_TOKEN_NONCE_SIZE {
            return Err(invalid());
        }
        let (nonce_bytes, ciphertext) = token.split_at(SMS_LOGIN_TOKEN_NONCE_SIZE);
        let nonce = Nonce::from_slice(nonce_bytes);
        let payload = self
            .cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| invalid())?;
        let session: BilibiliSmsLoginSession =
            serde_json::from_slice(&payload).map_err(|_| invalid())?;
        if chrono::Utc::now().timestamp() >= session.expires_at {
            return Err(invalid());
        }
        Ok(session)
    }
}

fn ensure_login_cookies_present(
    cookies: &HashMap<String, String>,
    method: &str,
) -> Result<(), synctv_core::provider::ProviderError> {
    if cookies.is_empty() {
        return Err(synctv_core::provider::ProviderError::Authentication(
            format!("Bilibili {method} login did not return session cookies"),
        ));
    }
    Ok(())
}

impl BilibiliApiImpl {
    #[must_use]
    pub fn new(
        provider: Arc<BilibiliProvider>,
        credential_repo: Arc<UserProviderCredentialRepository>,
        sms_login_secret: &[u8],
    ) -> Self {
        Self {
            provider,
            credential_repo,
            access_service: None,
            event_service: None,
            sms_login_token_codec: Arc::new(BilibiliSmsLoginTokenCodec::derive_from(
                sms_login_secret,
            )),
            qr_login_status_cache: Arc::new(
                moka::sync::Cache::builder()
                    .max_capacity(10_000)
                    .time_to_live(Duration::from_secs(QR_LOGIN_STATUS_CACHE_TTL_SECONDS))
                    .build(),
            ),
        }
    }

    #[must_use]
    pub fn with_access_service(mut self, access_service: Arc<dyn ProviderAccessService>) -> Self {
        self.access_service = Some(access_service);
        self
    }

    #[must_use]
    pub fn with_event_service(
        mut self,
        event_service: Option<Arc<dyn crate::runtime::RealtimeEventService>>,
    ) -> Self {
        self.event_service = event_service;
        self
    }

    fn resolve_effective_instance_name(
        requested_instance_name: Option<&str>,
        credential_instance_name: CredentialProviderInstanceName<'_>,
    ) -> Result<Option<String>, synctv_core::provider::ProviderError> {
        resolve_provider_instance_binding(requested_instance_name, credential_instance_name)
            .map_err(|error| synctv_core::provider::ProviderError::InvalidConfig(error.to_string()))
    }

    /// Resolve the user's single global Bilibili credential.
    async fn resolve_credential(
        &self,
        caller_user_id: &UserId,
        request_context: Option<&ExecutionControl>,
    ) -> Result<Option<ResolvedBilibiliCredential>, synctv_core::provider::ProviderError> {
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

        let provider_instance_name =
            normalize_provider_instance_name(cred.provider_instance_name.as_deref())
                .map(str::to_string);

        if let Some(access_service) = &self.access_service {
            let access = access_service
                .bilibili_access(*caller_user_id, request_context)
                .await?;
            return Ok(access.authenticated.then_some(ResolvedBilibiliCredential {
                cookies: access.cookies,
                provider_instance_name,
            }));
        }

        match cred.get_credential() {
            Ok(ProviderCredential::Bilibili { cookies }) => Ok(Some(ResolvedBilibiliCredential {
                cookies,
                provider_instance_name,
            })),
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
        instance_name: Option<&str>,
    ) -> Result<String, synctv_core::provider::ProviderError> {
        let server_id = UserProviderCredential::bilibili_server_id();
        let credential_data = ProviderCredential::bilibili(cookies.clone());

        let credential = UserProviderCredential {
            id: 0,
            user_id: *caller_user_id,
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
            .upsert_by_user_provider_server(&credential)
            .await
            .map_err(|e| {
                synctv_core::provider::ProviderError::Internal(format!(
                    "Failed to persist bilibili credential: {e}"
                ))
            })?;

        if let Some(access_service) = &self.access_service {
            access_service
                .invalidate(
                    *caller_user_id,
                    synctv_core::provider::BilibiliProvider::NAME,
                    &server_id,
                )
                .await?;
        }
        publish_provider_credential_changed(
            self.event_service.as_ref(),
            *caller_user_id,
            synctv_core::provider::BilibiliProvider::NAME,
            &server_id,
        );

        Ok(server_id)
    }

    fn decode_sms_session(
        &self,
        session_token: &str,
    ) -> Result<BilibiliSmsLoginSession, synctv_core::provider::ProviderError> {
        self.sms_login_token_codec.decode(session_token)
    }

    fn encode_sms_session(
        &self,
        session: &BilibiliSmsLoginSession,
    ) -> Result<String, synctv_core::provider::ProviderError> {
        self.sms_login_token_codec.encode(session)
    }

    fn sanitize_sms_validate(
        validate: &str,
    ) -> Result<String, synctv_core::provider::ProviderError> {
        let validate = validate.trim().to_string();
        if validate.is_empty() {
            return Err(synctv_core::provider::ProviderError::Authentication(
                "Bilibili SMS verification result is empty".to_string(),
            ));
        }
        Ok(validate)
    }

    fn verify_sms_instance_name(
        requested: Option<&str>,
        session: &BilibiliSmsLoginSession,
    ) -> Result<(), synctv_core::provider::ProviderError> {
        if requested.is_none() {
            return Ok(());
        }
        let expected = normalize_provider_instance_name(session.instance_name.as_deref());
        let requested = normalize_provider_instance_name(requested);
        if expected != requested {
            return Err(synctv_core::provider::ProviderError::Authentication(
                "Bilibili SMS login session does not match the requested provider instance"
                    .to_string(),
            ));
        }
        Ok(())
    }

    fn require_sms_phone(
        session: &BilibiliSmsLoginSession,
    ) -> Result<String, synctv_core::provider::ProviderError> {
        session.phone.clone().ok_or_else(|| {
            synctv_core::provider::ProviderError::Authentication(
                "Request a Bilibili SMS code before logging in".to_string(),
            )
        })
    }

    fn qr_login_status_cache_key(instance_name: Option<&str>, key: &str) -> String {
        let instance_name = normalize_provider_instance_name(instance_name).unwrap_or_default();
        format!("{instance_name}:{key}")
    }

    fn require_sms_captcha_key(
        session: &BilibiliSmsLoginSession,
    ) -> Result<String, synctv_core::provider::ProviderError> {
        session.captcha_key.clone().ok_or_else(|| {
            synctv_core::provider::ProviderError::Authentication(
                "Request a Bilibili SMS code before logging in".to_string(),
            )
        })
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
        let credential = self
            .resolve_credential(caller_user_id, request_context)
            .await?;
        let effective_instance_name = Self::resolve_effective_instance_name(
            requested_instance_name,
            credential.as_ref().map_or(
                CredentialProviderInstanceName::NotCredentialBacked,
                |credential| {
                    CredentialProviderInstanceName::CredentialBacked(
                        credential.provider_instance_name.as_deref(),
                    )
                },
            ),
        )?;
        let cookies = credential
            .as_ref()
            .map(|credential| credential.cookies.clone())
            .unwrap_or_default();

        // Step 1: Match URL
        let match_resp = self
            .provider
            .r#match_with_context(
                req.url.clone(),
                effective_instance_name.as_deref(),
                request_context,
            )
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
                        effective_instance_name.as_deref(),
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
                        effective_instance_name.as_deref(),
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
                        effective_instance_name.as_deref(),
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
        let cache_key = Self::qr_login_status_cache_key(instance_name, &req.key);
        if let Some(snapshot) = self.qr_login_status_cache.get(&cache_key) {
            return Ok(QrStatusResponse {
                status: snapshot.status,
            });
        }

        let check_req = synctv_media_providers::grpc::bilibili::LoginWithQrCodeReq {
            key: req.key.clone(),
        };

        let resp = self
            .provider
            .login_with_qr_code_with_context(check_req, instance_name, request_context)
            .await?;

        // If login succeeded (status = 4 = SUCCESS), persist cookies.
        if resp.status == 4 {
            ensure_login_cookies_present(&resp.cookies, "QR")?;
            self.persist_cookies(caller_user_id, &resp.cookies, instance_name)
                .await?;
            self.qr_login_status_cache.invalidate(&cache_key);
        } else {
            self.qr_login_status_cache.insert(
                cache_key,
                BilibiliQrLoginStatusSnapshot {
                    status: resp.status,
                },
            );
        }

        Ok(QrStatusResponse {
            status: resp.status,
        })
    }

    /// Start SMS login and keep captcha details server-side.
    pub async fn start_sms_login(
        &self,
        req: StartSmsLoginRequest,
        instance_name: Option<&str>,
    ) -> Result<StartSmsLoginResponse, synctv_core::provider::ProviderError> {
        self.start_sms_login_with_context(req, instance_name, None)
            .await
    }

    pub async fn start_sms_login_with_context(
        &self,
        _req: StartSmsLoginRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<StartSmsLoginResponse, synctv_core::provider::ProviderError> {
        let resp = self
            .provider
            .new_captcha_with_context(instance_name, request_context)
            .await?;

        let now = chrono::Utc::now().timestamp();
        let expires_at = now + SMS_LOGIN_SESSION_TTL_SECONDS;
        let challenge = resp.challenge.clone();
        let session = BilibiliSmsLoginSession {
            token: resp.token,
            challenge,
            phone: None,
            captcha_key: None,
            instance_name: instance_name.map(ToString::to_string),
            expires_at,
        };

        Ok(StartSmsLoginResponse {
            session_token: self.encode_sms_session(&session)?,
            gt: resp.gt,
            challenge: resp.challenge,
            expires_at,
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
        let mut session = self.decode_sms_session(&req.session_token)?;
        Self::verify_sms_instance_name(instance_name, &session)?;
        let validate = Self::sanitize_sms_validate(&req.validate)?;
        let sms_req = synctv_media_providers::grpc::bilibili::NewSmsReq {
            phone: req.phone.clone(),
            token: session.token.clone(),
            challenge: session.challenge.clone(),
            validate,
        };

        let resp = self
            .provider
            .new_sms_with_context(sms_req, session.instance_name.as_deref(), request_context)
            .await?;

        session.phone = Some(req.phone);
        session.captcha_key = Some(resp.captcha_key);

        Ok(SendSmsResponse {
            session_token: self.encode_sms_session(&session)?,
            expires_at: session.expires_at,
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
        let session = self.decode_sms_session(&req.session_token)?;
        Self::verify_sms_instance_name(instance_name, &session)?;
        let login_req = synctv_media_providers::grpc::bilibili::LoginWithSmsReq {
            phone: Self::require_sms_phone(&session)?,
            code: req.code,
            captcha_key: Self::require_sms_captcha_key(&session)?,
        };

        let resp = self
            .provider
            .login_with_sms_with_context(
                login_req,
                session.instance_name.as_deref(),
                request_context,
            )
            .await?;

        ensure_login_cookies_present(&resp.cookies, "SMS")?;
        self.persist_cookies(
            caller_user_id,
            &resp.cookies,
            session.instance_name.as_deref(),
        )
        .await?;

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
        let Some(credential) = self
            .resolve_credential(caller_user_id, request_context)
            .await?
        else {
            return Ok(UserInfoResponse {
                is_login: false,
                username: String::new(),
                face: String::new(),
                is_vip: false,
            });
        };
        let effective_instance_name = Self::resolve_effective_instance_name(
            requested_instance_name,
            CredentialProviderInstanceName::CredentialBacked(
                credential.provider_instance_name.as_deref(),
            ),
        )?;

        let info_req = synctv_media_providers::grpc::bilibili::UserInfoReq {
            cookies: credential.cookies,
        };

        let resp = self
            .provider
            .user_info_with_context(
                info_req,
                effective_instance_name.as_deref(),
                request_context,
            )
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
            if let Some(access_service) = &self.access_service {
                access_service
                    .invalidate(
                        *caller_user_id,
                        synctv_core::provider::BilibiliProvider::NAME,
                        &server_id,
                    )
                    .await?;
            }
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
        instance_name: Option<&str>,
    ) -> Result<GetBindsResponse, crate::impls::ApiError> {
        let server_id = UserProviderCredential::bilibili_server_id();
        let binds = get_provider_credentials(
            &self.credential_repo,
            caller_user_id,
            synctv_core::provider::BilibiliProvider::NAME,
            instance_name,
        )
        .await?
        .into_iter()
        .filter(|credential| credential.server_id == server_id)
        .map(|credential| BindInfo {
            id: credential.id.to_string(),
            server_id: credential.server_id,
            created_at: credential.created_at.timestamp(),
            provider_instance_name: credential.provider_instance_name.unwrap_or_default(),
        })
        .collect();

        Ok(GetBindsResponse { binds })
    }
}

#[cfg(test)]
mod tests {
    use super::{ensure_login_cookies_present, BilibiliApiImpl, BilibiliSmsLoginSession};
    use std::collections::HashMap;
    use std::sync::Arc;
    use synctv_core::credential_encryption::CredentialEncryption;
    use synctv_core::models::{
        CredentialProviderInstanceName, NewProviderInstance, SignupMethod, User,
    };
    use synctv_core::provider::BilibiliProvider;
    use synctv_core::repository::{
        ProviderInstanceRepository, UserProviderCredentialRepository, UserRepository,
    };
    use synctv_core::service::RemoteProviderManager;
    use synctv_core_testing::create_test_pool;

    fn provider() -> Arc<BilibiliProvider> {
        let pool = sqlx::PgPool::connect_lazy("postgresql://fake").expect("lazy pool");
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        Arc::new(BilibiliProvider::new(Arc::new(RemoteProviderManager::new(
            repo,
        ))))
    }

    fn test_encryption() -> CredentialEncryption {
        CredentialEncryption::new(&[0x42; 32]).expect("test encryption should initialize")
    }

    fn test_sms_login_secret() -> &'static [u8] {
        b"test-bilibili-sms-login-secret"
    }

    fn test_user(username: &str) -> User {
        User::new(username.to_string(), SignupMethod::Email)
    }

    async fn create_bilibili_provider_instance(pool: &sqlx::PgPool, name: &str) {
        ProviderInstanceRepository::new(pool.clone())
            .create(&synctv_core::models::ProviderInstance::new_remote(
                NewProviderInstance {
                    name: name.to_string(),
                    endpoint: format!("http://{name}.example.test:50051"),
                    comment: None,
                    jwt_secret: None,
                    custom_ca: None,
                    timeout_seconds: 10,
                    tls: false,
                    insecure_tls: false,
                    providers: vec![synctv_core::provider::BilibiliProvider::NAME.to_string()],
                },
            ))
            .await
            .expect("provider instance should be created");
    }

    #[test]
    fn effective_instance_uses_credential_binding_when_request_omits_instance() {
        let resolved = BilibiliApiImpl::resolve_effective_instance_name(
            None,
            CredentialProviderInstanceName::CredentialBacked(Some(" bilibili_remote ")),
        )
        .expect("credential binding should be authoritative");

        assert_eq!(resolved.as_deref(), Some("bilibili_remote"));
    }

    #[test]
    fn effective_instance_rejects_explicit_request_conflicting_with_credential_binding() {
        let err = BilibiliApiImpl::resolve_effective_instance_name(
            Some("bilibili_other"),
            CredentialProviderInstanceName::CredentialBacked(Some("bilibili_remote")),
        )
        .expect_err("explicit conflicting instance should be rejected");

        assert!(matches!(
            err,
            synctv_core::provider::ProviderError::InvalidConfig(_)
        ));
    }

    #[test]
    fn effective_instance_rejects_explicit_instance_for_unbound_credential() {
        let err = BilibiliApiImpl::resolve_effective_instance_name(
            Some("bilibili_remote"),
            CredentialProviderInstanceName::CredentialBacked(None),
        )
        .expect_err("unbound credential cannot satisfy an explicit instance request");

        assert!(matches!(
            err,
            synctv_core::provider::ProviderError::InvalidConfig(_)
        ));
    }

    #[test]
    fn login_cookie_validation_rejects_empty_provider_response() {
        let err = ensure_login_cookies_present(&HashMap::new(), "SMS")
            .expect_err("successful login without cookies must not be persisted");

        assert!(matches!(
            err,
            synctv_core::provider::ProviderError::Authentication(_)
        ));
    }

    #[test]
    fn login_cookie_validation_accepts_session_cookies() {
        let cookies = HashMap::from([("SESSDATA".to_string(), "session".to_string())]);

        ensure_login_cookies_present(&cookies, "SMS")
            .expect("non-empty provider cookies should be accepted");
    }

    #[tokio::test]
    async fn sms_login_session_token_decodes_across_api_instances() {
        let secret = test_sms_login_secret();
        let api_one = BilibiliApiImpl::new(
            provider(),
            Arc::new(UserProviderCredentialRepository::new(
                sqlx::PgPool::connect_lazy("postgresql://fake").expect("lazy pool"),
            )),
            secret,
        );
        let api_two = BilibiliApiImpl::new(
            provider(),
            Arc::new(UserProviderCredentialRepository::new(
                sqlx::PgPool::connect_lazy("postgresql://fake").expect("lazy pool"),
            )),
            secret,
        );
        let session = BilibiliSmsLoginSession {
            token: "captcha-token".to_string(),
            challenge: "captcha-challenge".to_string(),
            phone: Some("13800000000".to_string()),
            captcha_key: Some("captcha-key".to_string()),
            instance_name: Some("bilibili_remote".to_string()),
            expires_at: chrono::Utc::now().timestamp() + 60,
        };

        let encoded = api_one
            .encode_sms_session(&session)
            .expect("session token should encode");
        assert!(
            !encoded.contains("captcha-token")
                && !encoded.contains("captcha-challenge")
                && !encoded.contains("captcha-key")
                && !encoded.contains("13800000000"),
            "session token must not expose Bilibili SMS login secrets or phone number"
        );
        let decoded = api_two
            .decode_sms_session(&encoded)
            .expect("session token should decode with the same deployment secret");

        assert_eq!(decoded.token, session.token);
        assert_eq!(decoded.challenge, session.challenge);
        assert_eq!(decoded.phone, session.phone);
        assert_eq!(decoded.captcha_key, session.captcha_key);
        assert_eq!(decoded.instance_name, session.instance_name);
    }

    #[tokio::test]
    async fn sms_login_session_token_rejects_tampering_and_expiry() {
        let api = BilibiliApiImpl::new(
            provider(),
            Arc::new(UserProviderCredentialRepository::new(
                sqlx::PgPool::connect_lazy("postgresql://fake").expect("lazy pool"),
            )),
            test_sms_login_secret(),
        );
        let valid = BilibiliSmsLoginSession {
            token: "captcha-token".to_string(),
            challenge: "captcha-challenge".to_string(),
            phone: None,
            captcha_key: None,
            instance_name: None,
            expires_at: chrono::Utc::now().timestamp() + 60,
        };
        let expired = BilibiliSmsLoginSession {
            expires_at: chrono::Utc::now().timestamp() - 1,
            ..valid.clone()
        };

        let encoded = api
            .encode_sms_session(&valid)
            .expect("session token should encode");
        let mut tampered = encoded.clone();
        tampered.push('x');

        assert!(api.decode_sms_session(&tampered).is_err());
        assert!(api
            .decode_sms_session(
                &api.encode_sms_session(&expired)
                    .expect("expired session should still encode")
            )
            .is_err());
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn get_user_info_without_binding_reports_logged_out() {
        let (_postgres, pool) = create_test_pool().await;
        let api = BilibiliApiImpl::new(
            provider(),
            Arc::new(UserProviderCredentialRepository::new(pool)),
            test_sms_login_secret(),
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
            test_sms_login_secret(),
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

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn persist_cookies_stores_login_provider_instance_binding() {
        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let credential_repo = Arc::new(UserProviderCredentialRepository::new_with_encryption(
            pool.clone(),
            test_encryption(),
        ));
        let api =
            BilibiliApiImpl::new(provider(), credential_repo.clone(), test_sms_login_secret());
        let user = user_repo
            .create(&test_user("bilibili_instance_login"))
            .await
            .expect("test user should be created");
        create_bilibili_provider_instance(&pool, "bilibili_remote").await;
        let cookies = HashMap::from([("SESSDATA".to_string(), "session".to_string())]);

        api.persist_cookies(&user.id, &cookies, Some(" bilibili_remote "))
            .await
            .expect("cookies should persist");

        let credential = credential_repo
            .get_by_provider_and_server(
                user.id,
                synctv_core::provider::BilibiliProvider::NAME,
                &synctv_core::models::UserProviderCredential::bilibili_server_id(),
            )
            .await
            .expect("credential query should succeed")
            .expect("credential should exist");
        assert_eq!(
            credential.provider_instance_name.as_deref(),
            Some("bilibili_remote")
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn get_binds_filters_by_provider_instance_binding() {
        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let credential_repo = Arc::new(UserProviderCredentialRepository::new_with_encryption(
            pool.clone(),
            test_encryption(),
        ));
        let api = BilibiliApiImpl::new(provider(), credential_repo, test_sms_login_secret());
        let user = user_repo
            .create(&test_user("bilibili_bind_filter"))
            .await
            .expect("test user should be created");
        create_bilibili_provider_instance(&pool, "bilibili_remote").await;
        let cookies = HashMap::from([("SESSDATA".to_string(), "session".to_string())]);

        api.persist_cookies(&user.id, &cookies, Some("bilibili_remote"))
            .await
            .expect("cookies should persist");

        let matching = api
            .get_binds(&user.id, Some("bilibili_remote"))
            .await
            .expect("matching bind query should succeed");
        let non_matching = api
            .get_binds(&user.id, Some("bilibili_other"))
            .await
            .expect("non-matching bind query should succeed");

        assert_eq!(matching.binds.len(), 1);
        assert!(non_matching.binds.is_empty());
    }
}
