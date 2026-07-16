//! Unified API Implementation Layer
//!
//! This module contains the actual implementation of all APIs.
//! Both HTTP and gRPC handlers are thin wrappers that call these implementations.
//!
//! All methods use grpc-generated types for parameters and return values.
//! API impl entrypoints receive `synctv_proto` request messages directly so
//! HTTP, gRPC, and management can share the same implementation path. Caller
//! owned `Command`/`Query` DTOs stay in their caller layer; impls validate
//! protobuf first, then build the core request/query structs needed by services.
//! Keep shared behavior here or in `synctv-core`; transport modules own only
//! parsing, encoding, metadata extraction, status mapping, and stream adapters.
//! See `docs/src/content/docs/en/develop/implementation-contracts.mdx`.
use std::sync::Arc;
use synctv_livestream::StreamError;

pub(crate) mod admin;
pub(crate) mod client;
pub(crate) mod email;
pub(crate) mod messaging;
pub(crate) mod notification;
pub(crate) mod oauth2;
pub(crate) mod pagination;
pub(crate) mod playback;
pub(crate) mod playback_provider;
pub(crate) mod playlist_items_snapshot;
pub(crate) mod providers;
pub(crate) mod request_context;
pub(crate) mod room_members_snapshot;
pub(crate) mod room_settings_snapshot;
pub(crate) mod source_provider;
pub(crate) mod stored_files;
pub(crate) mod validation;

// Re-export for convenience
pub use admin::{
    ActiveStreamListSortBy, AdminApiImpl, AdminApiOptions, AdminApiRuntime, AdminAuthValidator,
    AdminReadServices, RequestContext as AdminRequestContext, ValidatedAdmin,
    LOCAL_MANAGEMENT_ACTOR_USER_ID,
};
pub use client::{
    ClientApiImpl, ClientApiOptions, ClientApiRuntime, ClientApiRuntimeServices, GuestRoomAccess,
    RoomActor,
};
pub use email::EmailApiImpl;
pub use messaging::{
    spawn_observed_playback_lifecycle_event_source, HeartbeatSchedule, MessageConcurrencyConfig,
    MessageSender, PlaybackAutoAdvanceSubscriber, ProtoCodec, ProviderPlaybackProgressSubscriber,
    StreamMessageHandler,
};
pub use notification::NotificationApiImpl;
pub use oauth2::OAuth2ApiImpl;
pub use providers::{
    AlistApiImpl, BilibiliApiImpl, CloudreveApiImpl, DouyinApiImpl, EmbyApiImpl, FnosApiImpl,
    NextcloudApiImpl, ProviderApiRuntime, ProviderCommonApiImpl, ProviderCommonApiRuntime,
    QnapApiImpl, SeafileApiImpl, SynologyApiImpl, TikTokApiImpl, TrueNasApiImpl, TwitchApiImpl,
    YoutubeApiImpl,
};
pub use request_context::{
    ApiRequestContext, EndpointRateLimitCategory, EndpointRateLimitScope, RequestExecutor,
    RequestMetadata, TransportProtocol,
};

const LIVESTREAM_NOT_AVAILABLE_MESSAGE: &str = "Live stream is not currently available";
const LIVESTREAM_PERMISSION_DENIED_MESSAGE: &str =
    "You do not have permission to access this live stream";
const LIVESTREAM_RATE_LIMITED_MESSAGE: &str =
    "Live streaming capacity limit reached. Please try again later.";
const LIVESTREAM_UNAVAILABLE_MESSAGE: &str =
    "Live streaming service is temporarily unavailable. Please try again later.";
const LIVESTREAM_REQUEST_FAILED_MESSAGE: &str = "Live streaming request failed";
const UPSTREAM_PROVIDER_UNAVAILABLE_MESSAGE: &str =
    "Upstream provider service is temporarily unavailable.";

pub(crate) fn normalize_member_remark_name(value: impl AsRef<str>) -> String {
    value.as_ref().trim().to_string()
}

pub(crate) fn normalize_member_display_tag(value: impl AsRef<str>) -> String {
    value.as_ref().trim().to_string()
}

#[derive(Debug)]
struct DisabledProviderAccessService;

#[async_trait::async_trait]
impl synctv_core::provider::ProviderAccessService for DisabledProviderAccessService {
    async fn alist_binding(
        &self,
        _user_id: synctv_core::models::UserId,
        _server_id: &str,
        _provider_instance_name: Option<&str>,
        _request_context: Option<&synctv_core::provider::ExecutionControl>,
    ) -> Result<synctv_core::provider::AlistBinding, synctv_core::provider::ProviderError> {
        Err(disabled_provider_access_error())
    }

    async fn alist_access(
        &self,
        _user_id: synctv_core::models::UserId,
        _server_id: &str,
        _provider_instance_name: Option<&str>,
        _request_context: Option<&synctv_core::provider::ExecutionControl>,
    ) -> Result<synctv_core::provider::AlistAccess, synctv_core::provider::ProviderError> {
        Err(disabled_provider_access_error())
    }

    async fn bilibili_access(
        &self,
        _user_id: synctv_core::models::UserId,
        _request_context: Option<&synctv_core::provider::ExecutionControl>,
    ) -> Result<synctv_core::provider::BilibiliAccess, synctv_core::provider::ProviderError> {
        Err(disabled_provider_access_error())
    }

    async fn emby_access(
        &self,
        _user_id: synctv_core::models::UserId,
        _server_id: &str,
        _provider_instance_name: Option<&str>,
        _request_context: Option<&synctv_core::provider::ExecutionControl>,
    ) -> Result<synctv_core::provider::EmbyAccess, synctv_core::provider::ProviderError> {
        Err(disabled_provider_access_error())
    }

    async fn invalidate(
        &self,
        _user_id: synctv_core::models::UserId,
        _provider: &str,
        _server_id: &str,
    ) -> Result<(), synctv_core::provider::ProviderError> {
        Ok(())
    }
}

fn disabled_provider_access_error() -> synctv_core::provider::ProviderError {
    synctv_core::provider::ProviderError::InvalidConfig(
        "provider access service is disabled for this test runtime".to_string(),
    )
}

pub(crate) fn disabled_provider_access_service(
) -> Arc<dyn synctv_core::provider::ProviderAccessService> {
    Arc::new(DisabledProviderAccessService)
}

pub fn validate_proto_request<M>(message: &M) -> Result<(), ApiError>
where
    M: prost_reflect::ReflectMessage,
{
    synctv_proto::validate(message).map_err(ApiError::from_proto_validation_error)
}

pub fn validate_room_name_input(name: &str) -> Result<String, ApiError> {
    synctv_core::validation::validate_room_name_input(name)
        .map_err(|error| ApiError::InvalidInput(error.to_string()))
}

pub fn validate_room_description_input(description: &str) -> Result<String, ApiError> {
    synctv_core::validation::validate_room_description_input(description)
        .map_err(|error| ApiError::InvalidInput(error.to_string()))
}

pub fn validate_media_name_input(name: &str) -> Result<String, ApiError> {
    synctv_core::validation::validate_media_name_input(name)
        .map_err(|error| ApiError::InvalidInput(error.to_string()))
}

pub fn add_media_request_from_client_proto(
    request: synctv_proto::client::AddMediaRequest,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_core::service::AddMediaRequest, ApiError> {
    synctv_adapter::client::add_media_request_from_client_proto(request, public_id_codec)
        .map_err(|error| ApiError::InvalidInput(error.to_string()))
}

pub fn search_chat_messages_query_from_client_proto(
    room_id: synctv_core::models::RoomId,
    request: &synctv_proto::client::SearchChatMessagesRequest,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_core::models::ChatSearchMessagesQuery, ApiError> {
    client::build_search_chat_messages_query(room_id, request, public_id_codec)
}

pub fn chat_message_receive_to_client_proto(
    message: &synctv_core::models::ChatMessageWithAttachments,
    public_id_codec: &synctv_adapter::PublicIdCodec,
    username: String,
) -> Result<synctv_proto::client::ChatMessageReceive, ApiError> {
    synctv_adapter::chat::chat_message_receive_to_proto(message, public_id_codec, username)
        .map_err(|error| ApiError::Internal(error.to_string()))
}

pub fn chat_history_cursor_to_client_proto(
    cursor: synctv_core::models::ChatHistoryCursor,
) -> String {
    format!(
        "{}|{}",
        synctv_common::time::format_datetime_rfc3339(cursor.created_at),
        cursor.id
    )
}

pub(crate) fn proto_page_params(
    page: i32,
    page_size: i32,
    default_page_size: u32,
    max_page_size: u32,
) -> synctv_core::models::PageParams {
    let page = if page > 0 { page.cast_unsigned() } else { 1 };
    let default_page_size = default_page_size.clamp(1, max_page_size);
    let page_size = if page_size > 0 {
        page_size.cast_unsigned().clamp(1, max_page_size)
    } else {
        default_page_size
    };

    synctv_core::models::PageParams::new(Some(page), Some(page_size))
}

pub(crate) fn proto_page_size_usize(
    page_size: i32,
    default_page_size: u32,
    max_page_size: u32,
) -> Result<usize, ApiError> {
    usize::try_from(proto_page_params(1, page_size, default_page_size, max_page_size).page_size)
        .map_err(|_| ApiError::Internal("page size exceeds usize::MAX".to_string()))
}

pub(crate) fn proto_page_params_u32(
    page: u32,
    page_size: u32,
    default_page_size: u32,
    max_page_size: u32,
) -> synctv_core::models::PageParams {
    let page = page.max(1);
    let default_page_size = default_page_size.clamp(1, max_page_size);
    let page_size = if page_size > 0 {
        page_size.clamp(1, max_page_size)
    } else {
        default_page_size
    };

    synctv_core::models::PageParams::new(Some(page), Some(page_size))
}

pub(crate) fn proto_page_size_u32_usize(
    page_size: u32,
    default_page_size: u32,
    max_page_size: u32,
) -> Result<usize, ApiError> {
    usize::try_from(proto_page_params_u32(1, page_size, default_page_size, max_page_size).page_size)
        .map_err(|_| ApiError::Internal("page size exceeds usize::MAX".to_string()))
}

pub(crate) fn playlist_media_count_or_zero(
    counts: &std::collections::HashMap<synctv_core::models::PlaylistId, i64>,
    playlist_id: &synctv_core::models::PlaylistId,
) -> i64 {
    counts.get(playlist_id).copied().unwrap_or(0)
}

pub(crate) fn room_member_count_or_zero(
    counts: &std::collections::HashMap<synctv_core::models::RoomId, i32>,
    room_id: &synctv_core::models::RoomId,
) -> i32 {
    counts.get(room_id).copied().unwrap_or(0)
}

fn invalid_id_input(field: &'static str, err: impl std::fmt::Display) -> ApiError {
    ApiError::InvalidInput(format!("Invalid {field}: {err}"))
}

pub fn parse_id_param<T>(
    value: &str,
    field: &'static str,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<T, ApiError>
where
    T: synctv_adapter::PublicIdType,
{
    public_id_codec
        .decode::<T>(value.trim())
        .map_err(|err| invalid_id_input(field, err))
}

pub fn parse_user_id_param(
    value: &str,
    field: &'static str,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_core::models::UserId, ApiError> {
    parse_id_param(value, field, public_id_codec)
}

pub fn parse_room_id_param(
    value: &str,
    field: &'static str,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_core::models::RoomId, ApiError> {
    parse_id_param(value, field, public_id_codec)
}

pub fn parse_media_id_param(
    value: &str,
    field: &'static str,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_core::models::MediaId, ApiError> {
    parse_id_param(value, field, public_id_codec)
}

pub fn parse_playlist_id_param(
    value: &str,
    field: &'static str,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_core::models::PlaylistId, ApiError> {
    parse_id_param(value, field, public_id_codec)
}

pub fn proto_validated_id<T>(
    value: impl AsRef<str>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<T, ApiError>
where
    T: synctv_adapter::PublicIdType,
{
    parse_id_param(value.as_ref(), T::TYPE_NAME, public_id_codec)
}

pub fn proto_validated_user_id(
    value: impl AsRef<str>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_core::models::UserId, ApiError> {
    proto_validated_id(value, public_id_codec)
}

pub fn proto_validated_room_id(
    value: impl AsRef<str>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_core::models::RoomId, ApiError> {
    proto_validated_id(value, public_id_codec)
}

pub fn proto_validated_media_id(
    value: impl AsRef<str>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_core::models::MediaId, ApiError> {
    proto_validated_id(value, public_id_codec)
}

pub fn proto_validated_playlist_id(
    value: impl AsRef<str>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_core::models::PlaylistId, ApiError> {
    proto_validated_id(value, public_id_codec)
}

pub fn proto_validated_optional_id<T>(
    value: impl AsRef<str>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<Option<T>, ApiError>
where
    T: synctv_adapter::PublicIdType,
{
    let value = value.as_ref();
    if value.is_empty() {
        Ok(None)
    } else {
        proto_validated_id(value, public_id_codec).map(Some)
    }
}

pub fn proto_validated_optional_media_id(
    value: impl AsRef<str>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<Option<synctv_core::models::MediaId>, ApiError> {
    proto_validated_optional_id(value, public_id_codec)
}

pub fn proto_validated_optional_playlist_id(
    value: impl AsRef<str>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<Option<synctv_core::models::PlaylistId>, ApiError> {
    proto_validated_optional_id(value, public_id_codec)
}

pub fn proto_validated_optional_room_id(
    value: impl AsRef<str>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<Option<synctv_core::models::RoomId>, ApiError> {
    proto_validated_optional_id(value, public_id_codec)
}

pub fn proto_validated_media_ids(
    values: Vec<String>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<Vec<synctv_core::models::MediaId>, ApiError> {
    values
        .into_iter()
        .map(|value| proto_validated_media_id(&value, public_id_codec))
        .collect()
}

pub fn proto_validated_playlist_ids(
    values: Vec<String>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<Vec<synctv_core::models::PlaylistId>, ApiError> {
    values
        .into_iter()
        .map(|value| proto_validated_playlist_id(&value, public_id_codec))
        .collect()
}

pub fn parse_optional_media_id_param(
    value: &str,
    field: &'static str,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<Option<synctv_core::models::MediaId>, ApiError> {
    parse_optional_id_param(value, field, public_id_codec)
}

pub fn parse_optional_playlist_id_param(
    value: &str,
    field: &'static str,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<Option<synctv_core::models::PlaylistId>, ApiError> {
    parse_optional_id_param(value, field, public_id_codec)
}

pub fn parse_optional_id_param<T>(
    value: &str,
    field: &'static str,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<Option<T>, ApiError>
where
    T: synctv_adapter::PublicIdType,
{
    if value.trim().is_empty() {
        Ok(None)
    } else {
        parse_id_param(value, field, public_id_codec).map(Some)
    }
}

pub(crate) use synctv_adapter::error::error_codes;
pub use synctv_adapter::error::ErrorKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiFieldViolation {
    pub field: String,
    pub description: String,
}

/// Structured API error that wraps `synctv_core::Error` variants for
/// type-safe status code mapping. This allows callers that propagate
/// typed errors to bypass keyword matching entirely.
///
/// Use `ApiError::from(core_error)` to convert, then call
/// `.classify()` for the `ErrorKind`.
#[derive(Debug, Clone)]
pub enum ApiError {
    NotFound(String),
    Authentication(String),
    Authorization(String),
    AlreadyExists(String),
    Conflict(String),
    InvalidInput(String),
    InvalidRequest {
        message: String,
        violations: Vec<ApiFieldViolation>,
    },
    PayloadTooLarge(String),
    RangeNotSatisfiable {
        total_size: u64,
    },
    BadGateway(String),
    RequestTimeout(String),
    RateLimited(String),
    RateLimitedWithRetry {
        message: String,
        retry_after_seconds: u64,
    },
    ServiceUnavailable(String),
    Timeout(String),
    Internal(String),
    OAuth2InvalidState {
        message: String,
    },
    OAuth2ProviderExchangeFailed {
        operation: synctv_core::service::OAuth2Operation,
        message: String,
    },
    OAuth2MissingTargetUser {
        operation: synctv_core::service::OAuth2Operation,
        message: String,
    },
    OAuth2UnexpectedTargetUser {
        operation: synctv_core::service::OAuth2Operation,
        message: String,
    },
    OAuth2TargetUserMismatch {
        operation: synctv_core::service::OAuth2Operation,
        message: String,
    },
    OAuth2ProviderAccountLinkedElsewhere {
        operation: synctv_core::service::OAuth2Operation,
        message: String,
    },
    OAuth2ProviderLookupFailed {
        operation: synctv_core::service::OAuth2Operation,
        kind: ErrorKind,
        message: String,
    },
    OAuth2ProviderLinkFailed {
        operation: synctv_core::service::OAuth2Operation,
        kind: ErrorKind,
        message: String,
    },
    OAuth2LoginFailed {
        operation: synctv_core::service::OAuth2Operation,
        kind: ErrorKind,
        message: String,
    },
    OAuth2ResponseBuildFailed {
        operation: synctv_core::service::OAuth2Operation,
        message: String,
    },
    OAuth2General {
        operation: Option<synctv_core::service::OAuth2Operation>,
        kind: ErrorKind,
        message: String,
        retry_after_seconds: Option<u64>,
    },
}

impl From<synctv_core::Error> for ApiError {
    fn from(err: synctv_core::Error) -> Self {
        match err {
            synctv_core::Error::NotFound(msg) => Self::NotFound(msg),
            synctv_core::Error::Authentication(msg) => Self::Authentication(msg),
            synctv_core::Error::Authorization(msg) => Self::Authorization(msg),
            synctv_core::Error::KickCooldownDenied => {
                Self::Authorization(synctv_core::Error::kick_cooldown_denied_message().to_string())
            }
            synctv_core::Error::AlreadyExists(msg) => Self::AlreadyExists(msg),
            synctv_core::Error::Conflict(msg) | synctv_core::Error::LockConflict(msg) => {
                Self::Conflict(msg)
            }
            synctv_core::Error::OptimisticLockConflict => {
                Self::Conflict("Resource modified concurrently".to_string())
            }
            synctv_core::Error::InvalidInput(msg) => Self::InvalidInput(msg),
            synctv_core::Error::RangeNotSatisfiable { total_size } => {
                Self::RangeNotSatisfiable { total_size }
            }
            synctv_core::Error::RateLimited(msg) => Self::RateLimited(msg),
            synctv_core::Error::ServiceUnavailable(msg) => Self::ServiceUnavailable(msg),
            synctv_core::Error::Timeout(msg) => Self::Timeout(msg),
            synctv_core::Error::Internal(msg) if msg.starts_with("Redis timeout:") => {
                Self::ServiceUnavailable(msg)
            }
            synctv_core::Error::Database(err) => {
                tracing::error!("Database error mapped to service unavailable: {}", err);
                Self::ServiceUnavailable(
                    "Service temporarily unavailable. Please try again later.".to_string(),
                )
            }
            synctv_core::Error::Redis(err) => {
                tracing::error!("Redis error mapped to service unavailable: {}", err);
                Self::ServiceUnavailable(
                    "Service temporarily unavailable. Please try again later.".to_string(),
                )
            }
            other => Self::Internal(other.to_string()),
        }
    }
}

impl From<synctv_core::provider::ProviderError> for ApiError {
    fn from(err: synctv_core::provider::ProviderError) -> Self {
        use synctv_core::provider::ProviderError;

        match err {
            ProviderError::NetworkError(msg) | ProviderError::ApiError(msg) => {
                Self::ServiceUnavailable(msg)
            }
            ProviderError::UpstreamHttp { status, .. } => match status {
                401 | 403 => {
                    tracing::warn!(status, "Upstream provider authentication failure");
                    Self::Authentication("Provider authentication failed".to_string())
                }
                404 => {
                    tracing::info!(status, "Upstream provider resource not found");
                    Self::NotFound(synctv_common::messages::PROVIDER_RESOURCE_NOT_FOUND.to_string())
                }
                409 => {
                    tracing::warn!(status, "Upstream provider reported a request conflict");
                    Self::Conflict("Upstream provider reported a request conflict.".to_string())
                }
                429 => {
                    tracing::warn!(status, "Upstream provider rate limited request");
                    Self::RateLimited("Upstream provider rate limited the request.".to_string())
                }
                408 => {
                    tracing::warn!(status, "Upstream provider unavailable");
                    Self::ServiceUnavailable(UPSTREAM_PROVIDER_UNAVAILABLE_MESSAGE.to_string())
                }
                status if status >= 500 => {
                    tracing::warn!(status, "Upstream provider unavailable");
                    Self::ServiceUnavailable(UPSTREAM_PROVIDER_UNAVAILABLE_MESSAGE.to_string())
                }
                _ => {
                    tracing::warn!(status, "Upstream provider rejected request");
                    Self::InvalidInput("Upstream provider rejected the request.".to_string())
                }
            },
            ProviderError::ParseError(msg)
            | ProviderError::InvalidConfig(msg)
            | ProviderError::InvalidUrl(msg)
            | ProviderError::MissingField(msg)
            | ProviderError::UnsupportedFormat(msg) => Self::InvalidInput(msg),
            ProviderError::NotFound => {
                Self::NotFound(synctv_common::messages::RESOURCE_NOT_FOUND.to_string())
            }
            ProviderError::InstanceNotFound(msg) | ProviderError::CredentialNotFound(msg) => {
                Self::NotFound(msg)
            }
            ProviderError::MissingInstance => {
                Self::NotFound("Provider instance not configured".to_string())
            }
            ProviderError::AuthRequired => {
                Self::Authentication(synctv_common::messages::AUTHENTICATION_REQUIRED.to_string())
            }
            ProviderError::Authentication(msg) | ProviderError::CredentialExpired(msg) => {
                Self::Authentication(msg)
            }
            ProviderError::CredentialRequired => {
                Self::Authentication("Credential required".to_string())
            }
            ProviderError::InvalidCredentialType => {
                Self::InvalidInput("Invalid credential type".to_string())
            }
            ProviderError::RouteRegistrationFailed(msg) | ProviderError::Internal(msg) => {
                Self::Internal(msg)
            }
            ProviderError::IoError(e) => Self::Internal(e.to_string()),
            ProviderError::JsonError(e) => Self::InvalidInput(format!("Invalid data format: {e}")),
            ProviderError::EncryptionRequired(provider) => Self::InvalidInput(format!(
                "Credential encryption required for provider '{provider}'"
            )),
        }
    }
}

impl ApiError {
    /// Convert this structured error into an `ErrorKind`.
    #[must_use]
    pub fn classify(&self) -> ErrorKind {
        match self {
            Self::NotFound(_) => ErrorKind::NotFound,
            Self::Authentication(_) | Self::OAuth2InvalidState { .. } => ErrorKind::Unauthenticated,
            Self::Authorization(_) | Self::OAuth2TargetUserMismatch { .. } => {
                ErrorKind::PermissionDenied
            }
            Self::AlreadyExists(_) | Self::OAuth2ProviderAccountLinkedElsewhere { .. } => {
                ErrorKind::AlreadyExists
            }
            Self::Conflict(_) => ErrorKind::Conflict,
            Self::InvalidInput(_)
            | Self::InvalidRequest { .. }
            | Self::RangeNotSatisfiable { .. } => ErrorKind::InvalidArgument,
            Self::BadGateway(_) => ErrorKind::ServiceUnavailable,
            Self::RequestTimeout(_) | Self::Timeout(_) => ErrorKind::Timeout,
            Self::PayloadTooLarge(_) | Self::RateLimited(_) | Self::RateLimitedWithRetry { .. } => {
                ErrorKind::RateLimited
            }
            Self::ServiceUnavailable(_) | Self::OAuth2ProviderExchangeFailed { .. } => {
                ErrorKind::ServiceUnavailable
            }
            Self::Internal(_)
            | Self::OAuth2MissingTargetUser { .. }
            | Self::OAuth2UnexpectedTargetUser { .. }
            | Self::OAuth2ResponseBuildFailed { .. } => ErrorKind::Internal,
            Self::OAuth2ProviderLookupFailed { kind, .. }
            | Self::OAuth2ProviderLinkFailed { kind, .. }
            | Self::OAuth2LoginFailed { kind, .. }
            | Self::OAuth2General { kind, .. } => *kind,
        }
    }

    #[must_use]
    pub fn is_invalid_argument(&self) -> bool {
        self.classify() == ErrorKind::InvalidArgument
    }

    /// Get the error message.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::NotFound(msg)
            | Self::Authentication(msg)
            | Self::Authorization(msg)
            | Self::AlreadyExists(msg)
            | Self::Conflict(msg)
            | Self::InvalidInput(msg)
            | Self::PayloadTooLarge(msg)
            | Self::BadGateway(msg)
            | Self::RequestTimeout(msg)
            | Self::RateLimited(msg)
            | Self::ServiceUnavailable(msg)
            | Self::Timeout(msg)
            | Self::Internal(msg) => msg,
            Self::RateLimitedWithRetry { message, .. }
            | Self::OAuth2InvalidState { message }
            | Self::OAuth2ProviderExchangeFailed { message, .. }
            | Self::OAuth2MissingTargetUser { message, .. }
            | Self::OAuth2UnexpectedTargetUser { message, .. }
            | Self::OAuth2TargetUserMismatch { message, .. }
            | Self::OAuth2ProviderAccountLinkedElsewhere { message, .. }
            | Self::OAuth2ProviderLookupFailed { message, .. }
            | Self::OAuth2ProviderLinkFailed { message, .. }
            | Self::OAuth2LoginFailed { message, .. }
            | Self::OAuth2ResponseBuildFailed { message, .. }
            | Self::OAuth2General { message, .. }
            | Self::InvalidRequest { message, .. } => message,
            Self::RangeNotSatisfiable { .. } => "Requested byte range is not satisfiable",
        }
    }

    #[must_use]
    pub fn retry_after_seconds(&self) -> Option<u64> {
        match self {
            Self::RateLimitedWithRetry {
                retry_after_seconds,
                ..
            } => Some(*retry_after_seconds),
            Self::OAuth2General {
                retry_after_seconds,
                ..
            } => *retry_after_seconds,
            _ => None,
        }
    }

    /// Get the application-level error code for this error.
    #[must_use]
    pub fn code(&self) -> i32 {
        self.classify().to_code()
    }

    #[must_use]
    pub const fn oauth2_operation(&self) -> Option<synctv_core::service::OAuth2Operation> {
        match self {
            Self::OAuth2ProviderExchangeFailed { operation, .. }
            | Self::OAuth2MissingTargetUser { operation, .. }
            | Self::OAuth2UnexpectedTargetUser { operation, .. }
            | Self::OAuth2TargetUserMismatch { operation, .. }
            | Self::OAuth2ProviderAccountLinkedElsewhere { operation, .. }
            | Self::OAuth2ProviderLookupFailed { operation, .. }
            | Self::OAuth2ProviderLinkFailed { operation, .. }
            | Self::OAuth2LoginFailed { operation, .. }
            | Self::OAuth2ResponseBuildFailed { operation, .. } => Some(*operation),
            Self::OAuth2General { operation, .. } => *operation,
            _ => None,
        }
    }

    fn from_proto_validation_error(error: prost_protovalidate::Error) -> Self {
        match error {
            prost_protovalidate::Error::Validation(error) => {
                let violations = error
                    .violations()
                    .iter()
                    .map(|violation| ApiFieldViolation {
                        field: violation.field_path(),
                        description: proto_violation_description(violation),
                    })
                    .collect();
                Self::InvalidRequest {
                    message: error.to_string(),
                    violations,
                }
            }
            prost_protovalidate::Error::Compilation(error) => {
                tracing::error!("proto validation rule compilation failed: {error}");
                Self::Internal("Validation rule compilation failed".to_string())
            }
            prost_protovalidate::Error::Runtime(error) => {
                tracing::error!("proto validation rule evaluation failed: {error}");
                Self::Internal("Validation rule evaluation failed".to_string())
            }
            error => {
                tracing::error!("unexpected proto validation failure: {error}");
                Self::Internal("Validation failed".to_string())
            }
        }
    }
}

fn proto_violation_description(violation: &prost_protovalidate::Violation) -> String {
    if !violation.message().is_empty() {
        violation.message().to_string()
    } else if !violation.rule_id().is_empty() {
        format!("[{}]", violation.rule_id())
    } else {
        "invalid value".to_string()
    }
}

impl synctv_adapter::error::ClassifiedError for ApiError {
    fn classify(&self) -> ErrorKind {
        self.classify()
    }

    fn message(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed(self.message())
    }
}

pub(crate) fn find_livestream_stream_error<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a StreamError> {
    let mut current = Some(error);
    while let Some(err) = current {
        if let Some(stream_error) = err.downcast_ref::<StreamError>() {
            return Some(stream_error);
        }
        current = err.source();
    }

    None
}

#[must_use]
pub(crate) fn map_livestream_stream_error(stream_error: &StreamError) -> ApiError {
    match stream_error {
        StreamError::NoPublisher(_)
        | StreamError::StreamNotFound(_)
        | StreamError::InvalidStreamKey(_) => {
            ApiError::NotFound(LIVESTREAM_NOT_AVAILABLE_MESSAGE.to_string())
        }
        StreamError::PermissionDenied(_) => {
            ApiError::Authorization(LIVESTREAM_PERMISSION_DENIED_MESSAGE.to_string())
        }
        StreamError::ResourceExhausted(_) => {
            ApiError::RateLimited(LIVESTREAM_RATE_LIMITED_MESSAGE.to_string())
        }
        StreamError::InvalidAddress(_)
        | StreamError::InvalidState(_)
        | StreamError::RedisError(_)
        | StreamError::RegistryError(_)
        | StreamError::GrpcError(_)
        | StreamError::ConnectionFailed(_)
        | StreamError::StaleEpoch(_)
        | StreamError::StreamHubError(_) => {
            ApiError::ServiceUnavailable(LIVESTREAM_UNAVAILABLE_MESSAGE.to_string())
        }
        StreamError::IoError(_) | StreamError::Internal(_) => {
            ApiError::Internal(LIVESTREAM_REQUEST_FAILED_MESSAGE.to_string())
        }
    }
}

#[must_use]
pub(crate) fn map_livestream_backend_error(error: &(dyn std::error::Error + 'static)) -> ApiError {
    if let Some(stream_error) = find_livestream_stream_error(error) {
        return map_livestream_stream_error(stream_error);
    }

    tracing::error!(error = %error, "Unexpected livestream backend error");
    ApiError::Internal(LIVESTREAM_REQUEST_FAILED_MESSAGE.to_string())
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl From<ApiError> for String {
    fn from(err: ApiError) -> Self {
        err.to_string()
    }
}

impl From<String> for ApiError {
    fn from(msg: String) -> Self {
        // Try to classify the string using the existing prefix/keyword matching
        // so that pre-existing string errors map to the right variant.
        match classify_error(&msg) {
            ErrorKind::NotFound => Self::NotFound(msg),
            ErrorKind::Unauthenticated => Self::Authentication(msg),
            ErrorKind::PermissionDenied => Self::Authorization(msg),
            ErrorKind::AlreadyExists => Self::AlreadyExists(msg),
            ErrorKind::Conflict => Self::Conflict(msg),
            ErrorKind::InvalidArgument => Self::InvalidInput(msg),
            ErrorKind::RateLimited => Self::RateLimited(msg),
            ErrorKind::ServiceUnavailable => Self::ServiceUnavailable(msg),
            ErrorKind::Timeout => Self::Timeout(msg),
            ErrorKind::Internal => Self::Internal(msg),
        }
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(err: serde_json::Error) -> Self {
        Self::InvalidInput(err.to_string())
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        Self::Internal(err.to_string())
    }
}

impl ApiError {
    /// Convert this error into a proto `ErrorMessage` with proper code and detail.
    ///
    /// The detail field is left empty to avoid leaking sensitive information.
    #[must_use]
    pub fn to_proto_error(&self) -> synctv_proto::client::ErrorMessage {
        // Sanitize Internal errors to avoid leaking sensitive implementation details
        // (e.g. database connection strings, stack traces) to clients.
        let message = match self {
            Self::Internal(_) => "Internal error".to_string(),
            _ => self.message().to_string(),
        };
        synctv_proto::client::ErrorMessage {
            message,
            code: self.code(),
            detail: String::new(),
        }
    }
}

/// Classify an impls-layer error string into a semantic error kind.
///
/// First attempts to match known `synctv_core::Error` display prefixes
/// for structured classification. Falls back to keyword matching for
/// errors that don't originate from the core layer.
#[must_use]
pub fn classify_error(err: &str) -> ErrorKind {
    // Try structured prefix matching first (matches synctv_core::Error::Display output)
    if let Some(kind) = classify_by_prefix(err) {
        return kind;
    }

    // Fallback: keyword-based classification for untyped error strings
    let lower = err.to_lowercase();
    if lower.contains("not found") {
        ErrorKind::NotFound
    } else if lower.contains("unauthenticated")
        || lower.contains("invalid token")
        || lower.contains("token expired")
        || lower.contains("not authenticated")
    {
        ErrorKind::Unauthenticated
    } else if lower.contains("permission")
        || lower.contains("forbidden")
        || lower.contains("not allowed")
        || lower.contains("no longer allowed")
        || lower.contains("not accepting new connections")
        || lower.contains("account is no longer available")
        || lower.contains("banned")
    {
        ErrorKind::PermissionDenied
    } else if lower.contains("already exists")
        || lower.contains("already taken")
        || lower.contains("already registered")
    {
        ErrorKind::AlreadyExists
    } else if lower.contains("optimistic lock conflict")
        || lower.contains("modified concurrently")
        || lower.contains("lock conflict")
    {
        ErrorKind::Conflict
    } else if lower.contains("room at capacity")
        || lower.contains("user at capacity")
        || lower.contains("server at capacity")
        || lower.contains("room capacity exceeded")
        || lower.contains("realtime room capacity exceeded")
        || lower.contains("too many connections for this user")
    {
        ErrorKind::RateLimited
    } else if lower.contains("distributed room capacity check unavailable")
        || lower.contains("distributed user connection check unavailable")
        || lower.contains("distributed total connection check unavailable")
    {
        ErrorKind::ServiceUnavailable
    } else if lower.contains("invalid")
        || lower.contains("playback position")
        || lower.contains("playback progress drift")
        || lower.contains("drift too large")
        || lower.contains("too short")
        || lower.contains("too long")
        || lower.contains("cannot be empty")
        || lower.contains("too many")
        || lower.contains("required")
        || lower.contains("must be")
        || lower.contains("must be formatted")
        || lower.contains("is no longer active")
        || lower.contains("does not match")
        || lower.contains("not currently joined")
        || lower.contains("not in this room")
        || lower.contains("has not joined webrtc")
    {
        ErrorKind::InvalidArgument
    } else {
        ErrorKind::Internal
    }
}

/// Try to classify an error string by matching the display prefixes
/// produced by `synctv_core::Error` variants (e.g., "Not found: ...",
/// "Authentication error: ..."). Returns `None` if no prefix matches.
fn classify_by_prefix(err: &str) -> Option<ErrorKind> {
    if err.starts_with("Not found: ") {
        Some(ErrorKind::NotFound)
    } else if err.starts_with("Authentication error: ") {
        Some(ErrorKind::Unauthenticated)
    } else if err.starts_with("Authorization error: ") {
        Some(ErrorKind::PermissionDenied)
    } else if err.starts_with("Already exists: ") {
        Some(ErrorKind::AlreadyExists)
    } else if err.starts_with("Conflict: ")
        || err.starts_with("Optimistic lock conflict")
        || err.starts_with("Distributed lock conflict: ")
    {
        Some(ErrorKind::Conflict)
    } else if err.starts_with("Invalid input: ") {
        Some(ErrorKind::InvalidArgument)
    } else if err.starts_with("Rate limited: ") {
        Some(ErrorKind::RateLimited)
    } else if err.starts_with("Service unavailable: ") {
        Some(ErrorKind::ServiceUnavailable)
    } else if err.starts_with("Operation timeout: ") {
        Some(ErrorKind::Timeout)
    } else if err.starts_with("Internal error: ")
        || err.starts_with("Database error: ")
        || err.starts_with("Redis error: ")
        || err.starts_with("Serialization error: ")
    {
        Some(ErrorKind::Internal)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn require_invalid_proto<M>(request: &M) -> TestResult<String>
    where
        M: prost::Message + prost_reflect::ReflectMessage + Default,
    {
        match validate_proto_request(request) {
            Ok(()) => Err(test_error("proto request should fail validation")),
            Err(error) if error.classify() == ErrorKind::InvalidArgument => {
                Ok(error.message().to_string())
            }
            Err(other) => Err(test_error(format!("expected invalid input, got {other:?}"))),
        }
    }

    #[test]
    fn test_validate_proto_request_maps_protovalidate_error_to_invalid_input() -> TestResult {
        let request = synctv_proto::client::StartOpaqueRegistrationRequest {
            username: "ab".to_string(),
            email: Some("not-an-email".to_string()),
            registration_request: Vec::new(),
        };

        let message = require_invalid_proto(&request)?;
        assert!(message.contains("username"), "{message}");
        assert!(message.contains("email"), "{message}");
        assert!(message.contains("registration_request"), "{message}");
        Ok(())
    }

    fn assert_invalid_proto_request<M>(request: &M, expected: &str) -> TestResult
    where
        M: prost::Message + prost_reflect::ReflectMessage + Default,
    {
        let message = require_invalid_proto(request)?;
        assert!(message.contains(expected), "{message}");
        Ok(())
    }

    #[test]
    fn test_validate_proto_request_rejects_empty_admin_batch_ids() -> TestResult {
        assert_invalid_proto_request(
            &synctv_proto::admin::BatchBanUsersRequest::default(),
            "user_ids",
        )?;
        assert_invalid_proto_request(
            &synctv_proto::admin::BatchDeleteUsersRequest::default(),
            "user_ids",
        )?;
        assert_invalid_proto_request(
            &synctv_proto::admin::BatchBanRoomsRequest::default(),
            "room_ids",
        )?;
        assert_invalid_proto_request(
            &synctv_proto::admin::BatchDeleteRoomsRequest::default(),
            "room_ids",
        )?;
        Ok(())
    }

    #[test]
    fn test_validate_proto_request_rejects_admin_batch_ban_reason_over_limit() -> TestResult {
        assert_invalid_proto_request(
            &synctv_proto::admin::BatchBanUsersRequest {
                user_ids: vec!["usr_abc123".to_string()],
                reason: "x".repeat(501),
            },
            "reason",
        )?;
        assert_invalid_proto_request(
            &synctv_proto::admin::BatchBanRoomsRequest {
                room_ids: vec!["room_abc123".to_string()],
                reason: "x".repeat(501),
            },
            "reason",
        )?;
        Ok(())
    }

    #[test]
    fn test_classify_error_not_found() {
        assert!(matches!(
            classify_error("User not found"),
            ErrorKind::NotFound
        ));
        assert!(matches!(
            classify_error("Room Not Found"),
            ErrorKind::NotFound
        ));
        assert!(matches!(
            classify_error("resource NOT FOUND"),
            ErrorKind::NotFound
        ));
    }

    #[test]
    fn test_classify_error_unauthenticated() {
        assert!(matches!(
            classify_error("Unauthenticated"),
            ErrorKind::Unauthenticated
        ));
        assert!(matches!(
            classify_error("invalid token"),
            ErrorKind::Unauthenticated
        ));
        assert!(matches!(
            classify_error("Token expired"),
            ErrorKind::Unauthenticated
        ));
        assert!(matches!(
            classify_error("Not authenticated"),
            ErrorKind::Unauthenticated
        ));
    }

    #[test]
    fn test_classify_error_permission_denied() {
        assert!(matches!(
            classify_error("Permission denied"),
            ErrorKind::PermissionDenied
        ));
        assert!(matches!(
            classify_error("Forbidden access"),
            ErrorKind::PermissionDenied
        ));
        assert!(matches!(
            classify_error("Operation not allowed"),
            ErrorKind::PermissionDenied
        ));
        assert!(matches!(
            classify_error("User is banned"),
            ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn test_classify_error_already_exists() {
        assert!(matches!(
            classify_error("User already exists"),
            ErrorKind::AlreadyExists
        ));
        assert!(matches!(
            classify_error("Username already taken"),
            ErrorKind::AlreadyExists
        ));
        assert!(matches!(
            classify_error("Email already registered"),
            ErrorKind::AlreadyExists
        ));
    }

    #[test]
    fn test_classify_error_invalid_argument() {
        assert!(matches!(
            classify_error("Invalid email format"),
            ErrorKind::InvalidArgument
        ));
        assert!(matches!(
            classify_error("Password too short"),
            ErrorKind::InvalidArgument
        ));
        assert!(matches!(
            classify_error("Username too long"),
            ErrorKind::InvalidArgument
        ));
        assert!(matches!(
            classify_error("Field cannot be empty"),
            ErrorKind::InvalidArgument
        ));
        assert!(matches!(
            classify_error("Too many rooms"),
            ErrorKind::InvalidArgument
        ));
        assert!(matches!(
            classify_error("Email required"),
            ErrorKind::InvalidArgument
        ));
        assert!(matches!(
            classify_error("Password must be alphanumeric"),
            ErrorKind::InvalidArgument
        ));
    }

    #[test]
    fn test_classify_error_internal() {
        assert!(matches!(
            classify_error("Something went wrong"),
            ErrorKind::Internal
        ));
        assert!(matches!(
            classify_error("Database connection failed"),
            ErrorKind::Internal
        ));
        assert!(matches!(
            classify_error("Unexpected error"),
            ErrorKind::Internal
        ));
    }

    #[test]
    fn test_classify_error_case_insensitive() {
        assert!(matches!(classify_error("NOT FOUND"), ErrorKind::NotFound));
        assert!(matches!(
            classify_error("PERMISSION denied"),
            ErrorKind::PermissionDenied
        ));
        assert!(matches!(
            classify_error("INVALID token"),
            ErrorKind::Unauthenticated
        ));
    }

    #[test]
    fn test_classify_error_not_found_takes_priority_over_invalid() {
        // "not found" contains "not" but should match NotFound, not InvalidArgument
        assert!(matches!(
            classify_error("Resource not found"),
            ErrorKind::NotFound
        ));
    }

    #[test]
    fn test_classify_error_invalid_token_is_unauthenticated_not_invalid_argument() {
        // "invalid token" should match Unauthenticated (checked before InvalidArgument)
        assert!(matches!(
            classify_error("invalid token supplied"),
            ErrorKind::Unauthenticated
        ));
    }

    #[test]
    fn test_classify_error_banned_is_permission_denied() {
        // "banned" should match PermissionDenied
        assert!(matches!(
            classify_error("User has been banned from the room"),
            ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn test_classify_error_empty_string_is_internal() {
        assert!(matches!(classify_error(""), ErrorKind::Internal));
    }

    #[test]
    fn test_classify_error_whitespace_only_is_internal() {
        assert!(matches!(classify_error("   "), ErrorKind::Internal));
    }

    #[test]
    fn test_classify_error_must_be_is_invalid_argument() {
        assert!(matches!(
            classify_error("Username must be alphanumeric"),
            ErrorKind::InvalidArgument
        ));
    }

    #[test]
    fn test_classify_error_not_allowed_is_permission_denied() {
        assert!(matches!(
            classify_error("This action is not allowed"),
            ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn test_classify_error_already_registered_is_already_exists() {
        assert!(matches!(
            classify_error("User already registered"),
            ErrorKind::AlreadyExists
        ));
    }

    #[test]
    fn test_classify_error_not_authenticated_is_unauthenticated() {
        assert!(matches!(
            classify_error("User is not authenticated"),
            ErrorKind::Unauthenticated
        ));
    }

    #[test]
    fn test_classify_error_mixed_case_keywords() {
        assert!(matches!(
            classify_error("Token Expired"),
            ErrorKind::Unauthenticated
        ));
        assert!(matches!(
            classify_error("Already Taken"),
            ErrorKind::AlreadyExists
        ));
        assert!(matches!(
            classify_error("Cannot Be Empty"),
            ErrorKind::InvalidArgument
        ));
    }

    #[test]
    fn test_classify_by_prefix_core_error_display() {
        // These match the exact Display output of synctv_core::Error variants
        assert!(matches!(
            classify_error("Not found: room 123"),
            ErrorKind::NotFound
        ));
        assert!(matches!(
            classify_error("Authentication error: expired"),
            ErrorKind::Unauthenticated
        ));
        assert!(matches!(
            classify_error("Authorization error: forbidden"),
            ErrorKind::PermissionDenied
        ));
        assert!(matches!(
            classify_error("Already exists: user"),
            ErrorKind::AlreadyExists
        ));
        assert!(matches!(
            classify_error("Conflict: upstream provider reported a request conflict"),
            ErrorKind::Conflict
        ));
        assert!(matches!(
            classify_error("Invalid input: bad field"),
            ErrorKind::InvalidArgument
        ));
        assert!(matches!(
            classify_error("Internal error: oops"),
            ErrorKind::Internal
        ));
        assert!(matches!(
            classify_error("Database error: connection refused"),
            ErrorKind::Internal
        ));
    }

    #[test]
    fn test_api_error_classify() {
        let err = ApiError::NotFound("room".to_string());
        assert!(matches!(err.classify(), ErrorKind::NotFound));
        assert_eq!(err.message(), "room");

        let err = ApiError::Authentication("bad token".to_string());
        assert!(matches!(err.classify(), ErrorKind::Unauthenticated));

        let err = ApiError::Authorization("denied".to_string());
        assert!(matches!(err.classify(), ErrorKind::PermissionDenied));

        let err = ApiError::AlreadyExists("dup".to_string());
        assert!(matches!(err.classify(), ErrorKind::AlreadyExists));

        let err = ApiError::InvalidInput("bad".to_string());
        assert!(matches!(err.classify(), ErrorKind::InvalidArgument));

        let err = ApiError::RateLimited("too fast".to_string());
        assert!(matches!(err.classify(), ErrorKind::RateLimited));

        let err = ApiError::ServiceUnavailable("redis unavailable".to_string());
        assert!(matches!(err.classify(), ErrorKind::ServiceUnavailable));

        let err = ApiError::Timeout("request timed out".to_string());
        assert!(matches!(err.classify(), ErrorKind::Timeout));

        let err = ApiError::Internal("boom".to_string());
        assert!(matches!(err.classify(), ErrorKind::Internal));

        let err = ApiError::RateLimitedWithRetry {
            message: "too fast".to_string(),
            retry_after_seconds: 30,
        };
        assert!(matches!(err.classify(), ErrorKind::RateLimited));
        assert_eq!(err.retry_after_seconds(), Some(30));
    }

    #[test]
    fn test_api_error_display_uses_user_message() {
        let cases = [
            ApiError::NotFound("room not found".into()),
            ApiError::Authentication("invalid token".into()),
            ApiError::Authorization("not a member of this room".into()),
            ApiError::AlreadyExists("user already exists".into()),
            ApiError::InvalidInput("username is required".into()),
            ApiError::RateLimited("room at capacity".into()),
            ApiError::RateLimitedWithRetry {
                message: "email rate limited".into(),
                retry_after_seconds: 42,
            },
            ApiError::ServiceUnavailable("backend unavailable".into()),
            ApiError::Internal("internal details".into()),
        ];

        for api_err in cases {
            assert_eq!(api_err.to_string(), api_err.message());
        }
    }

    #[test]
    fn test_classify_error_room_capacity_is_rate_limited() {
        assert!(matches!(
            classify_error("Room at capacity (42 connections, max: 40)"),
            ErrorKind::RateLimited
        ));
        assert!(matches!(
            classify_error("Room at capacity across all replicas (42 connections)"),
            ErrorKind::RateLimited
        ));
        assert!(matches!(
            classify_error("Server at capacity across all replicas (42 connections)"),
            ErrorKind::RateLimited
        ));
    }

    #[test]
    fn test_classify_error_user_capacity_is_rate_limited() {
        assert!(matches!(
            classify_error("User at capacity (4 connections, max: 3)"),
            ErrorKind::RateLimited
        ));
        assert!(matches!(
            classify_error("Too many connections for this user across all replicas (max 3)"),
            ErrorKind::RateLimited
        ));
    }

    #[test]
    fn test_classify_error_distributed_capacity_check_is_service_unavailable() {
        assert!(matches!(
            classify_error(
                "Distributed room capacity check unavailable; refusing room join while cluster Redis is degraded"
            ),
            ErrorKind::ServiceUnavailable
        ));
        assert!(matches!(
            classify_error(
                "Distributed user connection check unavailable; refusing new connection while cluster Redis is degraded"
            ),
            ErrorKind::ServiceUnavailable
        ));
        assert!(matches!(
            classify_error(
                "Distributed total connection check unavailable; refusing new connection while cluster Redis is degraded"
            ),
            ErrorKind::ServiceUnavailable
        ));
    }

    #[test]
    fn test_api_error_from_core_rate_limited() {
        // Test that synctv_core::Error::RateLimited is correctly converted to ApiError::RateLimited
        let core_err = synctv_core::Error::RateLimited("too many requests".to_string());
        let api_err = ApiError::from(core_err);
        assert!(matches!(api_err, ApiError::RateLimited(ref msg) if msg == "too many requests"));
        assert!(matches!(api_err.classify(), ErrorKind::RateLimited));
        assert_eq!(api_err.code(), error_codes::RESOURCE_EXHAUSTED);
    }

    #[test]
    fn test_api_error_from_core_service_unavailable() {
        let core_err = synctv_core::Error::ServiceUnavailable("redis unavailable".to_string());
        let api_err = ApiError::from(core_err);
        assert!(matches!(
            api_err,
            ApiError::ServiceUnavailable(ref msg) if msg == "redis unavailable"
        ));
        assert!(matches!(api_err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(api_err.code(), error_codes::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_api_error_from_core_timeout_maps_to_timeout() {
        let core_err = synctv_core::Error::Timeout("oauth2 provider timed out".to_string());
        let api_err = ApiError::from(core_err);
        assert!(matches!(
            api_err,
            ApiError::Timeout(ref msg) if msg == "oauth2 provider timed out"
        ));
        assert!(matches!(api_err.classify(), ErrorKind::Timeout));
        assert_eq!(api_err.code(), error_codes::TIMEOUT);
    }

    #[test]
    fn test_api_error_from_core_database_maps_to_service_unavailable() {
        let core_err = synctv_core::Error::Database(sqlx::Error::PoolTimedOut);
        let api_err = ApiError::from(core_err);
        assert!(
            matches!(api_err, ApiError::ServiceUnavailable(ref msg) if msg == "Service temporarily unavailable. Please try again later."),
            "database infrastructure failures must remain service unavailable, got: {api_err:?}"
        );
        assert!(matches!(api_err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(api_err.code(), error_codes::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_api_error_from_core_redis_maps_to_service_unavailable() {
        let redis_err = redis::RedisError::from((redis::ErrorKind::Io, "connection reset by peer"));
        let api_err = ApiError::from(synctv_core::Error::Redis(redis_err));
        assert!(
            matches!(api_err, ApiError::ServiceUnavailable(ref msg) if msg == "Service temporarily unavailable. Please try again later."),
            "redis infrastructure failures must remain service unavailable, got: {api_err:?}"
        );
        assert!(matches!(api_err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(api_err.code(), error_codes::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_api_error_from_provider_network_error_maps_to_service_unavailable() {
        let provider_err =
            synctv_core::provider::ProviderError::NetworkError("connection refused".to_string());
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::ServiceUnavailable(ref msg) if msg == "connection refused"
        ));
        assert!(matches!(api_err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(api_err.code(), error_codes::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_api_error_from_provider_api_error_maps_to_service_unavailable() {
        let provider_err =
            synctv_core::provider::ProviderError::ApiError("upstream provider down".to_string());
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::ServiceUnavailable(ref msg) if msg == "upstream provider down"
        ));
        assert!(matches!(api_err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(api_err.code(), error_codes::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_api_error_from_provider_authentication_maps_to_authentication() {
        let provider_err = synctv_core::provider::ProviderError::Authentication(
            "provider rejected credentials".to_string(),
        );
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::Authentication(ref msg) if msg == "provider rejected credentials"
        ));
        assert!(matches!(api_err.classify(), ErrorKind::Unauthenticated));
        assert_eq!(api_err.code(), error_codes::UNAUTHENTICATED);
    }

    #[test]
    fn test_api_error_from_provider_upstream_5xx_maps_to_service_unavailable() {
        let provider_err = synctv_core::provider::ProviderError::UpstreamHttp {
            status: 503,
            url: "https://provider.example/api".to_string(),
        };
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::ServiceUnavailable(ref msg)
                if msg == "Upstream provider service is temporarily unavailable."
        ));
        assert!(matches!(api_err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(api_err.code(), error_codes::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_api_error_from_provider_upstream_408_maps_to_service_unavailable() {
        let provider_err = synctv_core::provider::ProviderError::UpstreamHttp {
            status: 408,
            url: "https://provider.example/api?token=secret".to_string(),
        };
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::ServiceUnavailable(ref msg)
                if msg == "Upstream provider service is temporarily unavailable."
        ));
        assert!(matches!(api_err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(api_err.code(), error_codes::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_api_error_from_provider_upstream_409_maps_to_conflict() {
        let provider_err = synctv_core::provider::ProviderError::UpstreamHttp {
            status: 409,
            url: "https://provider.example/api?token=secret".to_string(),
        };
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::Conflict(ref msg)
                if msg == "Upstream provider reported a request conflict."
        ));
        assert!(matches!(api_err.classify(), ErrorKind::Conflict));
        assert_eq!(api_err.code(), error_codes::CONFLICT);
    }

    #[test]
    fn test_api_error_from_provider_upstream_429_maps_to_rate_limited() {
        let provider_err = synctv_core::provider::ProviderError::UpstreamHttp {
            status: 429,
            url: "https://provider.example/api?token=secret".to_string(),
        };
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::RateLimited(ref msg)
                if msg == "Upstream provider rate limited the request."
        ));
        assert!(matches!(api_err.classify(), ErrorKind::RateLimited));
        assert_eq!(api_err.code(), error_codes::RESOURCE_EXHAUSTED);
    }

    #[test]
    fn test_api_error_from_provider_upstream_404_maps_to_not_found() {
        let provider_err = synctv_core::provider::ProviderError::UpstreamHttp {
            status: 404,
            url: "https://provider.example/api".to_string(),
        };
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::NotFound(ref msg)
                if msg == synctv_common::messages::PROVIDER_RESOURCE_NOT_FOUND
        ));
        assert!(matches!(api_err.classify(), ErrorKind::NotFound));
        assert_eq!(api_err.code(), error_codes::NOT_FOUND);
    }

    #[test]
    fn test_api_error_from_provider_upstream_400_maps_to_invalid_input() {
        let provider_err = synctv_core::provider::ProviderError::UpstreamHttp {
            status: 400,
            url: "https://provider.example/api".to_string(),
        };
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::InvalidInput(ref msg) if msg == "Upstream provider rejected the request."
        ));
        assert!(matches!(api_err.classify(), ErrorKind::InvalidArgument));
        assert_eq!(api_err.code(), error_codes::INVALID_ARGUMENT);
    }

    #[test]
    fn test_api_error_from_provider_encryption_required_maps_to_invalid_input() {
        let provider_err = synctv_core::provider::ProviderError::EncryptionRequired("bilibili");
        let api_err = ApiError::from(provider_err);

        assert!(
            matches!(
                api_err,
                ApiError::InvalidInput(ref msg)
                    if msg == "Credential encryption required for provider 'bilibili'"
            ),
            "credential encryption precondition failures must not be hidden as internal errors, got: {api_err:?}"
        );
        assert!(matches!(api_err.classify(), ErrorKind::InvalidArgument));
        assert_eq!(api_err.code(), error_codes::INVALID_ARGUMENT);
    }

    #[test]
    fn test_classify_by_prefix_rate_limited() {
        assert!(matches!(
            classify_error("Rate limited: too fast"),
            ErrorKind::RateLimited
        ));
        assert!(matches!(
            classify_error("Operation timeout: request budget exceeded"),
            ErrorKind::Timeout
        ));
    }

    #[test]
    fn test_proto_page_params_uses_endpoint_default_for_zero_page_size() {
        let params = proto_page_params(0, 0, 50, 100);

        assert_eq!(params.page, 1);
        assert_eq!(params.page_size, 50);
    }

    #[test]
    fn test_proto_page_params_clamps_explicit_page_size() {
        let params = proto_page_params(2, 500, 50, 100);

        assert_eq!(params.page, 2);
        assert_eq!(params.page_size, 100);
    }

    #[test]
    fn test_proto_page_size_usize_uses_endpoint_default() -> Result<(), ApiError> {
        assert_eq!(proto_page_size_usize(0, 50, 100)?, 50);
        Ok(())
    }
}
