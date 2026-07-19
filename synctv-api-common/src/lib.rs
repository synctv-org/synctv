#![recursion_limit = "256"]

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

pub mod admin_settings_mapping;
pub mod api_error_model;
pub mod api_runtime;
pub mod app_state;
pub mod chat_event_dispatcher;
pub mod emby_thumbnail_urls;
pub mod fanout;
pub mod fnos_thumbnail_urls;
pub mod http_error;
pub mod impls;
pub mod media_fanout;
pub mod membership_event_fanout;
pub mod metrics_auth;
pub mod nextcloud_preview_urls;
pub mod observability;
pub mod playback_fanout;
pub mod playback_provider;
pub mod playlist_fanout;
pub mod providers;
pub mod proxy_signature;
pub mod qnap_thumbnail_urls;
pub mod realtime_fanout;
pub mod realtime_lifecycle;
pub mod request_context;
pub mod resource_change;
pub mod room_cache_fanout;
pub mod room_lifecycle_fanout;
pub mod room_settings_mapping;
pub mod runtime;
pub mod runtime_adapters;
pub mod seafile_thumbnail_urls;
pub mod server_settings;
pub mod status;
pub mod synology_image_urls;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod webrtc_status;

pub use api_runtime::*;
pub use app_state::{
    create_app_state_from_options, AppState, ProxyCacheLifecycleRuntime, RouterOptions,
};
pub use http_error::{AppError, AppResult};
pub use impls::*;
pub use metrics_auth::{MetricsAccessController, MetricsAccessError};
pub use proxy_signature::*;
pub use realtime_fanout::*;
pub use runtime::RealtimeAdmissionError;
pub use runtime_adapters::proxy_slice_cache_options_from_runtime_settings;
pub use server_settings::{validate_cors_origin, ApiServerSettings, DEFAULT_PROJECT_URL};
pub use synctv_adapter::{PublicIdCodec, PublicIdConfig, PublicIdKind, PublicIdType};
