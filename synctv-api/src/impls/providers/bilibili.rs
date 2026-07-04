//! Bilibili API Implementation
//!
//! Unified implementation for all Bilibili API operations.
//! Used by both HTTP and gRPC handlers.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use synctv_core::models::{
    resolve_provider_instance_binding, CredentialProviderInstanceName, UserId,
};
use synctv_core::provider::{
    BilibiliMatchRequest, BilibiliParseLivePageRequest, BilibiliParsePgcPageRequest,
    BilibiliParseVideoPageRequest, BilibiliProvider, BilibiliQrLoginStatus,
    BilibiliSmsLoginTokenCodec, BilibiliUserInfoRequest, ExecutionControl, ProviderAccessService,
};
use synctv_core::repository::UserProviderCredentialRepository;
use synctv_proto::providers::bilibili::{
    BindInfo, CheckQrRequest, GetBindsResponse, LoginQrRequest, LoginSmsRequest, LoginSmsResponse,
    LogoutRequest, LogoutResponse, ParseRequest, ParseResponse, QrCodeResponse, QrStatusResponse,
    SendSmsRequest, SendSmsResponse, StartSmsLoginRequest, StartSmsLoginResponse, UserInfoRequest,
    UserInfoResponse, VideoInfo,
};

use super::ProviderApiRuntime;
use super::{
    provider_instance_name_for_provider, provider_instance_name_for_response,
    publish_provider_credential_changed,
};

/// Bilibili API implementation
///
/// Contains all business logic for Bilibili operations.
/// Methods accept grpc-generated request types and return grpc-generated response types.
#[derive(Clone)]
pub struct BilibiliApiImpl {
    provider: Arc<BilibiliProvider>,
    access_service: Arc<dyn ProviderAccessService>,
    event_service: Arc<dyn crate::runtime::RealtimeEventService>,
    sms_login_token_codec: Arc<BilibiliSmsLoginTokenCodec>,
    qr_login_status_cache: Arc<moka::sync::Cache<String, i32>>,
}

struct ResolvedBilibiliCredential {
    cookies: HashMap<String, String>,
    provider_instance_name: Option<String>,
}

const QR_LOGIN_STATUS_CACHE_TTL_SECONDS: u64 = 2;

fn parse_bilibili_match_id(
    raw: &str,
    field: &str,
) -> Result<u64, synctv_core::provider::ProviderError> {
    raw.parse::<u64>().map_err(|error| {
        synctv_core::provider::ProviderError::ParseError(format!(
            "Invalid Bilibili {field} id '{raw}': {error}"
        ))
    })
}

const fn bilibili_qr_status_to_proto(status: BilibiliQrLoginStatus) -> i32 {
    match status {
        BilibiliQrLoginStatus::Unknown => 0,
        BilibiliQrLoginStatus::Expired => 1,
        BilibiliQrLoginStatus::NotScanned => 2,
        BilibiliQrLoginStatus::Scanned => 3,
        BilibiliQrLoginStatus::Success => 4,
    }
}

impl BilibiliApiImpl {
    pub fn new_with_runtime(
        provider: &Arc<BilibiliProvider>,
        credential_repo: Arc<UserProviderCredentialRepository>,
        sms_login_secret: &[u8],
        runtime: ProviderApiRuntime,
    ) -> Result<Self, synctv_core::provider::ProviderError> {
        Ok(Self {
            provider: Arc::new(provider.with_credential_repo(credential_repo)),
            access_service: runtime.access_service,
            event_service: runtime.event_service,
            sms_login_token_codec: Arc::new(BilibiliProvider::sms_login_token_codec_from_secret(
                sms_login_secret,
            )?),
            qr_login_status_cache: Arc::new(
                moka::sync::Cache::builder()
                    .max_capacity(10_000)
                    .time_to_live(Duration::from_secs(QR_LOGIN_STATUS_CACHE_TTL_SECONDS))
                    .build(),
            ),
        })
    }

    fn resolve_effective_instance_name(
        requested_instance_name: Option<&str>,
        credential_instance_name: CredentialProviderInstanceName<'_>,
    ) -> Result<Option<String>, synctv_core::provider::ProviderError> {
        let requested_instance_name = provider_instance_name_for_provider(requested_instance_name)?;
        resolve_provider_instance_binding(requested_instance_name, credential_instance_name)
            .map_err(|error| synctv_core::provider::ProviderError::InvalidConfig(error.to_string()))
    }

    /// Resolve the user's single global Bilibili credential.
    async fn resolve_credential(
        &self,
        caller_user_id: &UserId,
        request_context: Option<&ExecutionControl>,
    ) -> Result<Option<ResolvedBilibiliCredential>, synctv_core::provider::ProviderError> {
        let access = self
            .access_service
            .bilibili_access(*caller_user_id, request_context)
            .await?;
        Ok(access.authenticated.then_some(ResolvedBilibiliCredential {
            cookies: access.cookies,
            provider_instance_name: access.provider_instance_name,
        }))
    }

    async fn publish_login_change(
        &self,
        caller_user_id: &UserId,
        server_id: &str,
    ) -> Result<(), synctv_core::provider::ProviderError> {
        self.access_service
            .invalidate(
                *caller_user_id,
                synctv_core::provider::BilibiliProvider::NAME,
                server_id,
            )
            .await?;
        publish_provider_credential_changed(
            &self.event_service,
            *caller_user_id,
            synctv_core::provider::BilibiliProvider::NAME,
            server_id,
        );

        Ok(())
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
            .map_or_else(HashMap::new, |credential| credential.cookies.clone());

        // Step 1: Match URL
        let match_resp = self
            .provider
            .r#match_with_context(
                BilibiliMatchRequest {
                    url: req.url.clone(),
                },
                effective_instance_name.as_deref(),
                request_context,
            )
            .await?;

        // Step 2: Parse based on type
        let page_info = match match_resp.content_type.as_str() {
            "video" | "bv" | "av" => {
                let parse_req = BilibiliParseVideoPageRequest {
                    cookies: cookies.clone(),
                    bvid: if match_resp.content_type == "bv" {
                        match_resp.id.clone()
                    } else {
                        String::new()
                    },
                    aid: if match_resp.content_type == "av" {
                        parse_bilibili_match_id(&match_resp.id, "aid")?
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
                let parse_req = BilibiliParsePgcPageRequest {
                    cookies: cookies.clone(),
                    epid: if match_resp.content_type == "ep" {
                        parse_bilibili_match_id(&match_resp.id, "epid")?
                    } else {
                        0
                    },
                    ssid: if match_resp.content_type == "ss" {
                        parse_bilibili_match_id(&match_resp.id, "ssid")?
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
                let parse_req = BilibiliParseLivePageRequest {
                    cookies: cookies.clone(),
                    room_id: parse_bilibili_match_id(&match_resp.id, "room")?,
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
                    format!("Unsupported URL type: {}", match_resp.content_type),
                ));
            }
        };

        // Convert to response format
        let videos: Vec<VideoInfo> = page_info
            .videos
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

        Ok(ParseResponse {
            title: page_info.title,
            actors: page_info.actors,
            videos,
        })
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

    pub async fn check_qr_with_context(
        &self,
        caller_user_id: &UserId,
        req: CheckQrRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<QrStatusResponse, synctv_core::provider::ProviderError> {
        let cache_key = BilibiliProvider::qr_login_status_cache_key(instance_name, &req.key)?;
        if let Some(status) = self.qr_login_status_cache.get(&cache_key) {
            return Ok(QrStatusResponse { status });
        }

        let resp = self
            .provider
            .check_qr_and_persist_with_context(
                *caller_user_id,
                req.key,
                instance_name,
                request_context,
            )
            .await?;

        if resp.status == BilibiliQrLoginStatus::Success {
            if let Some(server_id) = resp.server_id.as_deref() {
                self.publish_login_change(caller_user_id, server_id).await?;
            }
            self.qr_login_status_cache.invalidate(&cache_key);
        } else {
            self.qr_login_status_cache
                .insert(cache_key, bilibili_qr_status_to_proto(resp.status));
        }

        Ok(QrStatusResponse {
            status: bilibili_qr_status_to_proto(resp.status),
        })
    }

    pub async fn start_sms_login_with_context(
        &self,
        _req: StartSmsLoginRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<StartSmsLoginResponse, synctv_core::provider::ProviderError> {
        let started = self
            .provider
            .start_sms_login_session_with_context(
                &self.sms_login_token_codec,
                instance_name,
                request_context,
            )
            .await?;

        Ok(StartSmsLoginResponse {
            session_token: started.session_token,
            gt: started.gt,
            challenge: started.challenge,
            expires_at: started.expires_at,
        })
    }

    pub async fn send_sms_with_context(
        &self,
        req: SendSmsRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<SendSmsResponse, synctv_core::provider::ProviderError> {
        let updated = self
            .provider
            .send_sms_with_session_context(
                &self.sms_login_token_codec,
                &req.session_token,
                req.phone,
                &req.validate,
                instance_name,
                request_context,
            )
            .await?;

        Ok(SendSmsResponse {
            session_token: updated.session_token,
            expires_at: updated.expires_at,
        })
    }

    pub async fn login_sms_with_context(
        &self,
        caller_user_id: &UserId,
        req: LoginSmsRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<LoginSmsResponse, synctv_core::provider::ProviderError> {
        let resp = self
            .provider
            .login_with_sms_session_context(
                *caller_user_id,
                &self.sms_login_token_codec,
                &req.session_token,
                req.code,
                instance_name,
                request_context,
            )
            .await?;
        self.publish_login_change(caller_user_id, &resp.server_id)
            .await?;

        Ok(LoginSmsResponse {})
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

        let info_req = BilibiliUserInfoRequest {
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
        let server_id = BilibiliProvider::credential_server_id();
        if self.provider.delete_credential(*caller_user_id).await? {
            self.access_service
                .invalidate(
                    *caller_user_id,
                    synctv_core::provider::BilibiliProvider::NAME,
                    &server_id,
                )
                .await?;
            publish_provider_credential_changed(
                &self.event_service,
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
        let binds = self
            .provider
            .list_binds(*caller_user_id, instance_name)
            .await?
            .into_iter()
            .map(|bind| BindInfo {
                id: bind.id.to_string(),
                server_id: bind.server_id,
                created_at: bind.created_at,
                provider_instance_name: provider_instance_name_for_response(
                    bind.provider_instance_name,
                ),
            })
            .collect();

        Ok(GetBindsResponse { binds })
    }
}

#[cfg(test)]
mod tests {
    use super::{BilibiliApiImpl, ProviderApiRuntime};
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
    use synctv_core_testing::create_test_pool;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn provider_ok<T>(result: Result<T, synctv_core::provider::ProviderError>) -> TestResult<T> {
        result.map_err(|error| test_error(error.to_string()))
    }

    fn provider_err<T>(
        result: Result<T, synctv_core::provider::ProviderError>,
    ) -> TestResult<synctv_core::provider::ProviderError> {
        match result {
            Ok(_) => Err(test_error("expected provider error")),
            Err(error) => Ok(error),
        }
    }

    fn core_ok<T>(result: synctv_core::Result<T>) -> TestResult<T> {
        result.map_err(|error| test_error(error.to_string()))
    }

    fn api_ok<T>(result: Result<T, crate::impls::ApiError>) -> TestResult<T> {
        result.map_err(|error| test_error(format!("{error:?}")))
    }

    fn test_encryption() -> TestResult<CredentialEncryption> {
        Ok(CredentialEncryption::new(&[0x42; 32])?)
    }

    fn test_sms_login_secret() -> &'static [u8] {
        b"test-bilibili-sms-login-secret"
    }

    fn test_api(
        pool: sqlx::PgPool,
        credential_repo: &Arc<UserProviderCredentialRepository>,
    ) -> TestResult<BilibiliApiImpl> {
        let instance_manager = Arc::new(synctv_core::service::RemoteProviderManager::new(
            Arc::new(ProviderInstanceRepository::new(pool.clone())),
        ));
        let provider = Arc::new(BilibiliProvider::with_client_manager(
            instance_manager,
            Arc::new(synctv_core::provider::ProviderClientManager::new()?),
        ));
        let alist_instance_manager = Arc::new(synctv_core::service::RemoteProviderManager::new(
            Arc::new(ProviderInstanceRepository::new(pool)),
        ));
        let alist_provider = Arc::new(synctv_core::provider::AlistProvider::with_client_manager(
            alist_instance_manager,
            Arc::new(synctv_core::provider::ProviderClientManager::new()?),
        ));
        let runtime = ProviderApiRuntime {
            access_service: Arc::new(synctv_core::provider::CachedProviderAccessService::new(
                credential_repo.clone(),
                alist_provider,
            )),
            event_service: Arc::new(crate::runtime::LocalNoopRealtimeEventService::new()),
        };
        provider_ok(BilibiliApiImpl::new_with_runtime(
            &provider,
            credential_repo.clone(),
            test_sms_login_secret(),
            runtime,
        ))
    }

    fn test_user(username: &str) -> User {
        User::new(username.to_string(), SignupMethod::Email)
    }

    async fn create_bilibili_provider_instance(pool: &sqlx::PgPool, name: &str) -> TestResult {
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
                    providers: vec![synctv_core::models::SourceProvider::Bilibili],
                },
            ))
            .await?;
        Ok(())
    }

    #[test]
    fn effective_instance_uses_credential_binding_when_request_omits_instance() -> TestResult {
        let resolved = provider_ok(BilibiliApiImpl::resolve_effective_instance_name(
            None,
            CredentialProviderInstanceName::CredentialBacked(Some(" bilibili_remote ")),
        ))?;

        assert_eq!(resolved.as_deref(), Some("bilibili_remote"));
        Ok(())
    }

    #[test]
    fn effective_instance_rejects_explicit_request_conflicting_with_credential_binding(
    ) -> TestResult {
        let err = provider_err(BilibiliApiImpl::resolve_effective_instance_name(
            Some("bilibili_other"),
            CredentialProviderInstanceName::CredentialBacked(Some("bilibili_remote")),
        ))?;

        assert!(matches!(
            err,
            synctv_core::provider::ProviderError::InvalidConfig(_)
        ));
        Ok(())
    }

    #[test]
    fn effective_instance_rejects_invalid_requested_instance_name() -> TestResult {
        let err = provider_err(BilibiliApiImpl::resolve_effective_instance_name(
            Some("bad instance!"),
            CredentialProviderInstanceName::NotCredentialBacked,
        ))?;

        assert!(matches!(
            err,
            synctv_core::provider::ProviderError::InvalidConfig(message)
                if message.contains("provider instance name")
        ));
        Ok(())
    }

    #[test]
    fn effective_instance_rejects_explicit_instance_for_unbound_credential() -> TestResult {
        let err = provider_err(BilibiliApiImpl::resolve_effective_instance_name(
            Some("bilibili_remote"),
            CredentialProviderInstanceName::CredentialBacked(None),
        ))?;

        assert!(matches!(
            err,
            synctv_core::provider::ProviderError::InvalidConfig(_)
        ));
        Ok(())
    }

    #[test]
    fn qr_login_status_cache_key_rejects_invalid_instance_name() -> TestResult {
        let err = provider_err(BilibiliProvider::qr_login_status_cache_key(
            Some("bad instance!"),
            "qr-key",
        ))?;

        assert!(matches!(
            err,
            synctv_core::provider::ProviderError::InvalidConfig(message)
                if message.contains("provider instance name")
        ));
        Ok(())
    }

    #[test]
    fn login_cookie_validation_rejects_empty_provider_response() -> TestResult {
        let err = provider_err(BilibiliProvider::ensure_login_cookies_present(
            &HashMap::new(),
            "SMS",
        ))?;

        assert!(matches!(
            err,
            synctv_core::provider::ProviderError::Authentication(_)
        ));
        Ok(())
    }

    #[test]
    fn login_cookie_validation_accepts_session_cookies() -> TestResult {
        let cookies = HashMap::from([("SESSDATA".to_string(), "session".to_string())]);

        provider_ok(BilibiliProvider::ensure_login_cookies_present(
            &cookies, "SMS",
        ))?;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn get_user_info_without_binding_reports_logged_out() -> TestResult {
        let (_postgres, pool) = create_test_pool().await;
        let credential_repo = Arc::new(UserProviderCredentialRepository::new(pool.clone()));
        let api = test_api(pool.clone(), &credential_repo)?;

        let response = provider_ok(
            api.get_user_info_with_context(
                &synctv_core::models::UserId::new(),
                synctv_proto::providers::bilibili::UserInfoRequest {
                    instance_name: String::new(),
                },
                None,
                None,
            )
            .await,
        )?;

        assert!(!response.is_login);
        assert!(response.username.is_empty());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn logout_without_binding_is_idempotent() -> TestResult {
        let (_postgres, pool) = create_test_pool().await;
        let credential_repo = Arc::new(UserProviderCredentialRepository::new(pool.clone()));
        let api = test_api(pool.clone(), &credential_repo)?;

        let response = provider_ok(
            api.logout(
                &synctv_core::models::UserId::new(),
                synctv_proto::providers::bilibili::LogoutRequest {
                    instance_name: String::new(),
                },
            )
            .await,
        )?;

        assert_eq!(response.message, "Logout successful");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn persist_cookies_stores_login_provider_instance_binding() -> TestResult {
        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let credential_repo = Arc::new(UserProviderCredentialRepository::new_with_encryption(
            pool.clone(),
            test_encryption()?,
        ));
        let api = test_api(pool.clone(), &credential_repo)?;
        let user = user_repo
            .create(&test_user("bilibili_instance_login"))
            .await?;
        create_bilibili_provider_instance(&pool, "bilibili_remote").await?;
        let cookies = HashMap::from([("SESSDATA".to_string(), "session".to_string())]);

        provider_ok(
            api.provider
                .persist_cookies_credential(user.id, cookies, Some(" bilibili_remote "))
                .await,
        )?;

        let credential = core_ok(
            credential_repo
                .get_by_provider_and_server(
                    user.id,
                    synctv_core::provider::BilibiliProvider::NAME,
                    &BilibiliProvider::credential_server_id(),
                )
                .await,
        )?
        .ok_or_else(|| test_error("credential should exist"))?;
        assert_eq!(
            credential.provider_instance_name.as_deref(),
            Some("bilibili_remote")
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn get_binds_filters_by_provider_instance_binding() -> TestResult {
        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let credential_repo = Arc::new(UserProviderCredentialRepository::new_with_encryption(
            pool.clone(),
            test_encryption()?,
        ));
        let api = test_api(pool.clone(), &credential_repo)?;
        let user = user_repo.create(&test_user("bilibili_bind_filter")).await?;
        create_bilibili_provider_instance(&pool, "bilibili_remote").await?;
        let cookies = HashMap::from([("SESSDATA".to_string(), "session".to_string())]);

        provider_ok(
            api.provider
                .persist_cookies_credential(user.id, cookies, Some("bilibili_remote"))
                .await,
        )?;

        let matching = api_ok(api.get_binds(&user.id, Some("bilibili_remote")).await)?;
        let non_matching = api_ok(api.get_binds(&user.id, Some("bilibili_other")).await)?;

        assert_eq!(matching.binds.len(), 1);
        assert!(non_matching.binds.is_empty());
        Ok(())
    }
}
