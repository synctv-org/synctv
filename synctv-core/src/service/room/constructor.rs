use sqlx::PgPool;
use std::sync::Arc;

use crate::{
    cache::{CacheInvalidationRuntime, ConsistencyCoordinator},
    repository::{
        realtime_outbox::RealtimeOutboxRepository, ChatRepository, MediaRepository,
        PlaybackSourceMetadataRepository, PlaylistRepository, RoomMemberRepository,
        RoomPasswordRepository, RoomPlaybackStateRepository, RoomRepository,
        RoomSettingsRepository, UserProviderCredentialRepository,
    },
    service::{
        audit::AuditService,
        media::MediaService,
        member::MemberService,
        notification::NotificationService,
        room_settings::{RoomSettingsRuntime, RoomSettingsService},
        OpaquePasswordService, PermissionService, PermissionServiceRuntime, PlaybackService,
        PlaylistService, ProvidersManager, RoomService, UserService,
    },
    Result,
};

use super::{RoomOpaquePasswordLoginSessionStore, RoomOpaquePasswordRegistrationSessionStore};

#[derive(Clone)]
pub struct RoomServiceOptions {
    pub clock: Arc<dyn crate::Clock>,
    pub read_pool: Option<sqlx::PgPool>,
    pub distributed_lock: Option<Arc<dyn crate::service::CoordinationLock>>,
    pub cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
    pub version_fence: Arc<dyn crate::cache::VersionFenceStore>,
    pub playback_l2_cache: crate::cache::PlaybackStateCache,
    pub room_settings_l2_cache: Arc<dyn crate::cache::CacheL2Backend>,
    pub room_settings_cache_key_prefix: String,
    pub member_permission_l2_cache: Arc<dyn crate::cache::CacheL2Backend>,
    pub member_permission_cache_key_prefix: String,
    pub credential_encryption: Option<crate::credential_encryption::CredentialEncryption>,
    pub credential_repo: Option<Arc<UserProviderCredentialRepository>>,
    pub provider_access_service: Option<Arc<dyn crate::provider::ProviderAccessService>>,
    pub provider_stores: Option<Arc<dyn crate::provider::ProviderStoreResolver>>,
    pub audit_service: Option<Arc<AuditService>>,
    pub brute_force_service: Option<Arc<dyn crate::service::BruteForceProtectionService>>,
    pub runtime_settings_store: Option<Arc<crate::service::RuntimeSettingsStore>>,
    pub user_notification_service: Option<Arc<crate::service::UserNotificationService>>,
    pub opaque_password_service: Arc<OpaquePasswordService>,
    pub opaque_password_registration_session_store:
        Arc<dyn RoomOpaquePasswordRegistrationSessionStore>,
    pub opaque_password_login_session_store: Arc<dyn RoomOpaquePasswordLoginSessionStore>,
    pub realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    pub media_file_storage_service: Option<Arc<dyn crate::service::FileStorageService>>,
    pub room_file_storage_service: Option<Arc<dyn crate::service::FileStorageService>>,
    pub playlist_file_storage_service: Option<Arc<dyn crate::service::FileStorageService>>,
}

impl RoomServiceOptions {
    #[must_use]
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_defaults_with_settings(pool: sqlx::PgPool) -> Self {
        let settings_service = Arc::new(crate::service::SettingsService::new(
            crate::repository::SettingsRepository::new(pool.clone()),
            pool,
        ));
        Self {
            runtime_settings_store: Some(Arc::new(crate::service::RuntimeSettingsStore::new(
                settings_service,
            ))),
            ..Self::test_defaults()
        }
    }

    #[must_use]
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_defaults() -> Self {
        Self {
            clock: Arc::new(crate::SystemClock),
            read_pool: None,
            distributed_lock: None,
            cache_invalidation: None,
            version_fence: Arc::new(crate::cache::LocalVersionFenceStore::new()),
            playback_l2_cache: crate::cache::PlaybackStateCache::new(
                Arc::new(crate::cache::NoopCacheL2),
                crate::service::PlaybackService::DEFAULT_CACHE_SIZE,
                crate::service::PlaybackService::DEFAULT_CACHE_TTL_SECS,
                crate::service::PlaybackService::DEFAULT_CACHE_TTL_SECS,
                "playback_state:".to_string(),
            ),
            room_settings_l2_cache: Arc::new(crate::cache::NoopCacheL2),
            room_settings_cache_key_prefix: "room_settings:".to_string(),
            member_permission_l2_cache: Arc::new(crate::cache::NoopCacheL2),
            member_permission_cache_key_prefix: "member_permission:".to_string(),
            credential_encryption: None,
            credential_repo: None,
            provider_access_service: None,
            provider_stores: Some(Arc::new(
                crate::provider::ProviderStoreRegistry::local_only("test:provider:"),
            )),
            audit_service: None,
            brute_force_service: None,
            runtime_settings_store: None,
            user_notification_service: None,
            opaque_password_service: Arc::new(OpaquePasswordService::new_ephemeral_for_process()),
            opaque_password_registration_session_store:
                crate::service::local_room_opaque_password_registration_session_store(),
            opaque_password_login_session_store:
                crate::service::local_room_opaque_password_login_session_store(),
            realtime_outbox: None,
            media_file_storage_service: None,
            room_file_storage_service: None,
            playlist_file_storage_service: None,
        }
    }
}

impl RoomService {
    #[cfg(any(test, feature = "test-support"))]
    pub fn new_for_tests(pool: PgPool, user_service: UserService) -> Result<Self> {
        Self::new_with_options(
            pool.clone(),
            user_service,
            RoomServiceOptions::test_defaults_with_settings(pool),
        )
    }

    pub fn new_with_options(
        pool: PgPool,
        user_service: UserService,
        options: RoomServiceOptions,
    ) -> Result<Self> {
        let provider_instance_repo = Arc::new(crate::repository::ProviderInstanceRepository::new(
            pool.clone(),
        ));
        let provider_instance_manager = Arc::new(crate::service::RemoteProviderManager::new(
            provider_instance_repo,
        ));
        let providers_manager = Arc::new(ProvidersManager::new(provider_instance_manager)?);
        Self::new_with_providers_and_options(pool, user_service, providers_manager, options)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn new_with_providers_for_tests(
        pool: PgPool,
        user_service: UserService,
        providers_manager: Arc<ProvidersManager>,
    ) -> Result<Self> {
        Self::new_with_providers_and_options(
            pool.clone(),
            user_service,
            providers_manager,
            RoomServiceOptions::test_defaults_with_settings(pool),
        )
    }

    pub fn new_with_providers_and_options(
        pool: PgPool,
        user_service: UserService,
        providers_manager: Arc<ProvidersManager>,
        options: RoomServiceOptions,
    ) -> Result<Self> {
        let permission_service = PermissionService::new_with_runtime(
            RoomMemberRepository::new(pool.clone()),
            RoomRepository::new(pool.clone()),
            PermissionServiceRuntime {
                runtime_settings_store: options.runtime_settings_store.clone(),
                room_settings_repo: Some(RoomSettingsRepository::new(pool.clone())),
                invalidation_service: options.cache_invalidation.clone(),
                version_fence: options.version_fence.clone(),
                member_permission_l2_cache: options.member_permission_l2_cache.clone(),
                member_permission_cache_key_prefix: options
                    .member_permission_cache_key_prefix
                    .clone(),
                room_settings_l2_cache: options.room_settings_l2_cache.clone(),
                room_settings_cache_key_prefix: options.room_settings_cache_key_prefix.clone(),
                ..PermissionServiceRuntime::local_only()
            },
        )?;
        Ok(Self::new_with_providers_permission_service_and_options(
            pool,
            user_service,
            providers_manager,
            permission_service,
            options,
        ))
    }

    #[must_use]
    pub fn new_with_providers_permission_service_and_options(
        pool: PgPool,
        user_service: UserService,
        providers_manager: Arc<ProvidersManager>,
        permission_service: PermissionService,
        options: RoomServiceOptions,
    ) -> Self {
        Self::build_with_providers_permission_service_and_options(
            pool,
            user_service,
            providers_manager,
            permission_service,
            options,
        )
    }

    fn build_with_providers_permission_service_and_options(
        pool: PgPool,
        user_service: UserService,
        providers_manager: Arc<ProvidersManager>,
        permission_service: PermissionService,
        options: RoomServiceOptions,
    ) -> Self {
        let read_pool = options.read_pool.clone().unwrap_or_else(|| pool.clone());
        let room_repo = RoomRepository::new_with_read_pool(pool.clone(), read_pool.clone());
        let taxonomy_repo = crate::repository::RoomTaxonomyRepository::new_with_read_pool(
            pool.clone(),
            read_pool.clone(),
        );
        let room_settings_repo =
            RoomSettingsRepository::new_with_read_pool(pool.clone(), read_pool.clone());
        let member_repo = RoomMemberRepository::new_with_read_pool(pool.clone(), read_pool.clone());
        let media_repo = MediaRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());
        let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
        let playback_source_metadata_repo = PlaybackSourceMetadataRepository::new(pool.clone());
        let chat_repo = ChatRepository::new_with_read_pool(pool.clone(), read_pool.clone());
        let room_password_repo =
            RoomPasswordRepository::new_with_read_pool(pool.clone(), read_pool);

        let notification_service = NotificationService::default();

        let member_service = MemberService::new_with_runtime(
            member_repo.clone(),
            room_repo.clone(),
            Some(room_settings_repo.clone()),
            permission_service.clone(),
            options.audit_service.clone(),
            options.cache_invalidation.clone(),
            notification_service.clone(),
        );

        let playlist_service = PlaylistService::new_with_runtime(
            playlist_repo.clone(),
            permission_service.clone(),
            providers_manager.clone(),
            options.credential_encryption.clone(),
            options.credential_repo.clone(),
            options.realtime_outbox.clone(),
            options.playlist_file_storage_service.clone(),
        );
        let media_service = MediaService::new_with_runtime(
            media_repo.clone(),
            playlist_repo.clone(),
            permission_service.clone(),
            providers_manager,
            notification_service.clone(),
            crate::service::MediaServiceRuntime {
                credential_encryption: options.credential_encryption.clone(),
                credential_repo: options.credential_repo.clone(),
                provider_access_service: options.provider_access_service.clone(),
                provider_stores: options.provider_stores.clone(),
                realtime_outbox: options.realtime_outbox.clone(),
                file_storage_service: options.media_file_storage_service.clone(),
            },
        );
        let playback_service = PlaybackService::new_with_runtime(
            playback_repo.clone(),
            permission_service.clone(),
            media_service.clone(),
            user_service.clone(),
            crate::service::PlaybackServiceRuntime {
                clock: options.clock.clone(),
                invalidation_service: options.cache_invalidation.clone(),
                l2_cache: Some(options.playback_l2_cache.clone()),
                version_fence: options.version_fence.clone(),
                realtime_outbox: options.realtime_outbox.clone(),
                source_metadata_repo: Some(playback_source_metadata_repo),
                notification_service: Some(notification_service.clone()),
            },
        );
        let room_settings_service = RoomSettingsService::new_with_version_fence(
            room_settings_repo.clone(),
            options.cache_invalidation.clone(),
            Arc::new(notification_service.clone()),
            RoomSettingsRuntime {
                version_fence: options.version_fence.clone(),
                l2_cache: options.room_settings_l2_cache.clone(),
                cache_key_prefix: options.room_settings_cache_key_prefix.clone(),
                cache_ttl_secs: None,
                cache_max_capacity: None,
            },
        );

        let version_fence = options.version_fence;

        Self {
            pool,
            distributed_lock: options.distributed_lock,
            clock: options.clock,
            room_repo,
            taxonomy_repo,
            room_settings_repo,
            member_repo,
            media_repo,
            playlist_repo,
            playback_repo,
            chat_repo,
            member_service,
            permission_service,
            playlist_service,
            media_service,
            playback_service,
            room_settings_service,
            notification_service,
            user_service,
            room_password_repo,
            cache_invalidation: options.cache_invalidation,
            audit_service: options.audit_service,
            brute_force_service: options.brute_force_service,
            runtime_settings_store: options.runtime_settings_store,
            user_notification_service: options.user_notification_service,
            opaque_password_service: options.opaque_password_service,
            opaque_password_registration_session_store: options
                .opaque_password_registration_session_store,
            opaque_password_login_session_store: options.opaque_password_login_session_store,
            realtime_outbox: options.realtime_outbox,
            media_file_storage_service: options.media_file_storage_service,
            room_file_storage_service: options.room_file_storage_service,
            consistency: ConsistencyCoordinator::new(version_fence),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub const fn has_brute_force_service(&self) -> bool {
        self.brute_force_service.is_some()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub const fn has_distributed_lock(&self) -> bool {
        self.distributed_lock.is_some()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub const fn has_runtime_settings_store(&self) -> bool {
        self.runtime_settings_store.is_some()
    }

    #[doc(hidden)]
    pub const fn runtime_settings_store(
        &self,
    ) -> Option<&Arc<crate::service::RuntimeSettingsStore>> {
        self.runtime_settings_store.as_ref()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn has_playback_l2_cache(&self) -> bool {
        self.playback_service.has_l2_cache()
    }
}
