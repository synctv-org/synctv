use sqlx::PgPool;
use std::sync::Arc;

use crate::{
    cache::ConsistencyCoordinator,
    repository::{
        ChatRepository, MediaRepository, PlaylistRepository, RoomMemberRepository,
        RoomPasswordRepository, RoomPlaybackStateRepository, RoomRepository,
        RoomSettingsRepository,
    },
    service::{
        media::MediaService,
        member::MemberService,
        notification::NotificationService,
        permission::{PermissionService, PermissionServiceRuntime},
        playback::PlaybackService,
        playlist::PlaylistService,
        room::{
            local_room_opaque_password_login_session_store,
            local_room_opaque_password_registration_session_store, RoomService, RoomServiceOptions,
        },
        room_settings::{RoomSettingsRuntime, RoomSettingsService},
        user::UserService,
        ProvidersManager,
    },
    Result,
};

impl RoomService {
    pub fn new_for_tests(pool: PgPool, user_service: UserService) -> Result<Self> {
        Self::new_with_options(pool, user_service, RoomServiceOptions::test_defaults())
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

    pub fn new_with_providers_for_tests(
        pool: PgPool,
        user_service: UserService,
        providers_manager: Arc<ProvidersManager>,
    ) -> Result<Self> {
        Self::new_with_providers_and_options(
            pool,
            user_service,
            providers_manager,
            RoomServiceOptions::test_defaults(),
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
                settings_registry: options.settings_registry.clone(),
                room_settings_repo: Some(RoomSettingsRepository::new(pool.clone())),
                invalidation_service: options.cache_invalidation.clone(),
                version_fence: options.version_fence.clone(),
                member_permission_l2_cache: options.room_settings_l2_cache.clone(),
                member_permission_cache_key_prefix: "member_permission:".to_string(),
                room_settings_l2_cache: options.room_settings_l2_cache.clone(),
                room_settings_cache_key_prefix: options
                    .room_settings_cache_key_prefix
                    .clone()
                    .unwrap_or_else(|| "room_settings:".to_string()),
                ..PermissionServiceRuntime::default()
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

    pub fn new_with_providers_and_permission_service_for_tests(
        pool: PgPool,
        user_service: UserService,
        providers_manager: Arc<ProvidersManager>,
        permission_service: PermissionService,
    ) -> Self {
        Self::new_with_providers_permission_service_and_options(
            pool,
            user_service,
            providers_manager,
            permission_service,
            RoomServiceOptions::test_defaults(),
        )
    }

    #[must_use]
    pub fn new_with_providers_permission_service_and_options(
        pool: PgPool,
        user_service: UserService,
        providers_manager: Arc<ProvidersManager>,
        permission_service: PermissionService,
        options: RoomServiceOptions,
    ) -> Self {
        let room_repo = RoomRepository::new(pool.clone());
        let room_settings_repo = RoomSettingsRepository::new(pool.clone());
        let member_repo = RoomMemberRepository::new(pool.clone());
        let media_repo = MediaRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());
        let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
        let chat_repo = ChatRepository::new(pool.clone());
        let room_password_repo = RoomPasswordRepository::new(pool.clone());

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
            options.credential_encryption.clone(),
            options.credential_repo.clone(),
            options.realtime_outbox.clone(),
            options.media_file_storage_service.clone(),
        );
        let playback_service = PlaybackService::new_with_runtime(
            playback_repo.clone(),
            permission_service.clone(),
            media_service.clone(),
            user_service.clone(),
            options.cache_invalidation.clone(),
            options.playback_l2_cache.clone(),
            options.version_fence.clone(),
            options.realtime_outbox.clone(),
        );
        let room_settings_service = RoomSettingsService::new_with_version_fence(
            room_settings_repo.clone(),
            options.cache_invalidation.clone(),
            Arc::new(notification_service.clone()),
            RoomSettingsRuntime {
                version_fence: options.version_fence.clone(),
                l2_cache: options.room_settings_l2_cache.clone(),
                cache_key_prefix: options
                    .room_settings_cache_key_prefix
                    .unwrap_or_else(|| "room_settings:".to_string()),
                ..RoomSettingsRuntime::default()
            },
        );

        let version_fence = options
            .version_fence
            .unwrap_or_else(|| Arc::new(crate::cache::NoopVersionFenceStore));

        Self {
            pool,
            distributed_lock: options.distributed_lock,
            room_repo,
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
            settings_registry: options.settings_registry,
            user_notification_service: options.user_notification_service,
            opaque_password_service: options.opaque_password_service,
            opaque_password_registration_session_store: options
                .opaque_password_registration_session_store
                .unwrap_or_else(local_room_opaque_password_registration_session_store),
            opaque_password_login_session_store: options
                .opaque_password_login_session_store
                .unwrap_or_else(local_room_opaque_password_login_session_store),
            realtime_outbox: options.realtime_outbox,
            media_file_storage_service: options.media_file_storage_service,
            room_file_storage_service: options.room_file_storage_service,
            consistency: ConsistencyCoordinator::new(version_fence),
        }
    }

    #[cfg(test)]
    pub(crate) const fn has_brute_force_service(&self) -> bool {
        self.brute_force_service.is_some()
    }

    #[cfg(test)]
    pub(crate) const fn has_distributed_lock(&self) -> bool {
        self.distributed_lock.is_some()
    }

    #[cfg(test)]
    pub(crate) const fn has_settings_registry(&self) -> bool {
        self.settings_registry.is_some()
    }

    #[doc(hidden)]
    pub const fn settings_registry(&self) -> Option<&Arc<crate::service::SettingsRegistry>> {
        self.settings_registry.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn has_playback_l2_cache(&self) -> bool {
        self.playback_service.has_l2_cache()
    }
}
