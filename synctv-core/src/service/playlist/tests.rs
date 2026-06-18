use super::*;
use crate::provider::{
    DirectoryItem, DynamicFolder, DynamicListQuery, MediaProvider, NextPlayItem, PlaybackResult,
    ProviderCredentialDependency, ProviderError,
};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

fn err<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> E {
    match result {
        Ok(_) => std::panic::panic_any(context.to_string()),
        Err(error) => error,
    }
}

fn test_credential_encryption() -> crate::credential_encryption::CredentialEncryption {
    ok(
        crate::credential_encryption::CredentialEncryption::new(&[0x42; 32]),
        "test encryption key should be valid",
    )
}

fn test_provider_instance_manager() -> Arc<crate::service::RemoteProviderManager> {
    crate::service::remote_provider_manager::empty_provider_instance_manager()
}

struct CredentialOwnerCheckProvider;

#[async_trait]
impl MediaProvider for CredentialOwnerCheckProvider {
    fn name(&self) -> &'static str {
        "credential_check"
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        _source_config: &Value,
    ) -> std::result::Result<PlaybackResult, ProviderError> {
        Err(ProviderError::UnsupportedFormat(
            "test provider does not generate playback".to_string(),
        ))
    }

    fn as_dynamic_folder(&self) -> Option<&dyn DynamicFolder> {
        Some(self)
    }

    async fn validate_source_config(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> std::result::Result<(), ProviderError> {
        if !source_config.is_dynamic_playlist() {
            return Err(ProviderError::Internal(
                "credential_check validates dynamic playlist sources only".to_string(),
            ));
        }
        let user_id = ctx
            .user_id
            .as_ref()
            .ok_or_else(|| ProviderError::Internal("missing user_id".to_string()))?;
        let credential_owner_id = ctx
            .credential_owner_id()
            .ok_or_else(|| ProviderError::Internal("missing credential_owner_id".to_string()))?;
        if credential_owner_id != user_id {
            return Err(ProviderError::Internal(
                "credential_owner_id must match playlist creator during validation".to_string(),
            ));
        }

        Ok(())
    }
}

#[async_trait]
impl DynamicFolder for CredentialOwnerCheckProvider {
    async fn list_playlist(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        _target: Option<&[u8]>,
        _query: DynamicListQuery,
    ) -> std::result::Result<Vec<DirectoryItem>, ProviderError> {
        Ok(Vec::new())
    }

    async fn resolve_item(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        _target: &[u8],
    ) -> std::result::Result<Option<NextPlayItem>, ProviderError> {
        Ok(None)
    }

    async fn next(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        _playing_media: &crate::models::Media,
        _target: &[u8],
        _play_mode: crate::models::PlayMode,
    ) -> std::result::Result<Option<NextPlayItem>, ProviderError> {
        Ok(None)
    }
}

struct AlistCredentialDependencyCheckProvider;

#[async_trait]
impl MediaProvider for AlistCredentialDependencyCheckProvider {
    fn name(&self) -> &'static str {
        crate::provider::AlistProvider::NAME
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        _source_config: &Value,
    ) -> std::result::Result<PlaybackResult, ProviderError> {
        Err(ProviderError::UnsupportedFormat(
            "test provider does not generate playback".to_string(),
        ))
    }

    fn as_dynamic_folder(&self) -> Option<&dyn DynamicFolder> {
        Some(self)
    }

    fn credential_dependencies(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> std::result::Result<Vec<ProviderCredentialDependency>, ProviderError> {
        let Some(server_id) = source_config
            .get("credential_server_id")
            .and_then(serde_json::Value::as_str)
        else {
            return Ok(Vec::new());
        };
        let user_id = ctx
            .user_id
            .as_ref()
            .ok_or_else(|| ProviderError::Internal("missing user_id".to_string()))?;

        let optional = source_config
            .get("credential_optional")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let dependency = if optional {
            ProviderCredentialDependency::optional(self.name(), user_id.to_string(), server_id)
        } else {
            ProviderCredentialDependency::new(self.name(), user_id.to_string(), server_id)
        };

        Ok(vec![dependency])
    }
}

#[async_trait]
impl DynamicFolder for AlistCredentialDependencyCheckProvider {
    async fn list_playlist(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        _target: Option<&[u8]>,
        _query: DynamicListQuery,
    ) -> std::result::Result<Vec<DirectoryItem>, ProviderError> {
        Ok(Vec::new())
    }

    async fn resolve_item(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        _target: &[u8],
    ) -> std::result::Result<Option<NextPlayItem>, ProviderError> {
        Ok(None)
    }

    async fn next(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        _playing_media: &crate::models::Media,
        _target: &[u8],
        _play_mode: crate::models::PlayMode,
    ) -> std::result::Result<Option<NextPlayItem>, ProviderError> {
        Ok(None)
    }
}

async fn test_builtin_providers_manager() -> Arc<crate::service::ProvidersManager> {
    let providers_manager = Arc::new(ok(
        crate::service::ProvidersManager::new(test_provider_instance_manager()),
        "providers manager should build",
    ));
    ok(
        providers_manager.create_builtin_defaults().await,
        "builtin providers should initialize",
    );
    providers_manager
}

async fn test_credential_check_providers_manager() -> Arc<crate::service::ProvidersManager> {
    let mut providers_manager = ok(
        crate::service::ProvidersManager::new(test_provider_instance_manager()),
        "providers manager should build",
    );
    providers_manager.register_factory(
        "credential_check",
        Box::new(|_instance_id, _config, _instance_manager| {
            Ok(Arc::new(CredentialOwnerCheckProvider))
        }),
    );
    let providers_manager = Arc::new(providers_manager);
    ok(
        providers_manager.create_builtin_defaults().await,
        "built-in providers should initialize",
    );
    ok(
        providers_manager
            .create_provider(
                "credential_check",
                "credential_check",
                &serde_json::json!({}),
            )
            .await,
        "credential_check provider should initialize",
    );
    providers_manager
}

async fn test_alist_dependency_check_providers_manager() -> Arc<crate::service::ProvidersManager> {
    let mut providers_manager = ok(
        crate::service::ProvidersManager::new(test_provider_instance_manager()),
        "providers manager should build",
    );
    providers_manager.register_factory(
        crate::provider::AlistProvider::NAME,
        Box::new(|_instance_id, _config, _instance_manager| {
            Ok(Arc::new(AlistCredentialDependencyCheckProvider))
        }),
    );
    let providers_manager = Arc::new(providers_manager);
    ok(
        providers_manager
            .create_provider(
                crate::provider::AlistProvider::NAME,
                crate::provider::AlistProvider::NAME,
                &serde_json::json!({}),
            )
            .await,
        "alist dependency check provider should initialize",
    );
    providers_manager
}

#[test]
fn playlist_edit_requires_matching_creator() {
    let creator_id = UserId::expect_positive(20);
    let playlist = Playlist {
        id: PlaylistId::expect_positive(21),
        room_id: RoomId::expect_positive(22),
        creator_id: Some(creator_id),
        name: "Owned".to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        parent_id: None,
        position: 1.0,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 1,
    };

    assert!(ensure_playlist_creator_can_edit(&playlist, &creator_id).is_ok());

    let other_user_id = UserId::expect_positive(23);
    assert!(matches!(
        ensure_playlist_creator_can_edit(&playlist, &other_user_id),
        Err(Error::Authorization(_))
    ));

    let mut unowned_playlist = playlist;
    unowned_playlist.creator_id = None;
    assert!(matches!(
        ensure_playlist_creator_can_edit(&unowned_playlist, &creator_id),
        Err(Error::Authorization(_))
    ));
}

#[test]
fn dynamic_folder_allows_default_provider_instance() {
    let (source_provider, source_config, provider_instance_name) = ok(
        normalize_dynamic_playlist_fields(
            Some("alist".to_string()),
            Some(serde_json::json!({"path": "/movies"})),
            None,
        ),
        "dynamic folder should allow default provider instance",
    );

    assert_eq!(source_provider.as_deref(), Some("alist"));
    assert_eq!(source_config, Some(serde_json::json!({"path": "/movies"})));
    assert!(provider_instance_name.is_none());
}

#[test]
fn static_folder_rejects_dynamic_fields_without_provider() {
    let err = err(
        normalize_dynamic_playlist_fields(
            None,
            Some(serde_json::json!({"path": "/movies"})),
            Some("alist-main".to_string()),
        ),
        "static folder should reject dynamic fields",
    );

    match err {
        Error::InvalidInput(message) => assert!(message.contains("source_provider")),
        other => std::panic::panic_any(format!("expected InvalidInput, got {other:?}")),
    }
}

#[test]
fn dynamic_folder_fields_are_trimmed() {
    let (source_provider, source_config, provider_instance_name) = ok(
        normalize_dynamic_playlist_fields(
            Some("  emby  ".to_string()),
            Some(serde_json::json!({"library_id": "abc123"})),
            Some("  emby-main  ".to_string()),
        ),
        "dynamic folder fields should normalize",
    );

    assert_eq!(source_provider.as_deref(), Some("emby"));
    assert!(source_config.is_some());
    assert_eq!(provider_instance_name.as_deref(), Some("emby-main"));
}

#[tokio::test]
async fn validate_dynamic_playlist_source_requires_credential_repo_wiring() {
    let providers_manager = test_builtin_providers_manager().await;

    let err = err(
        validate_dynamic_playlist_source_with_dependencies(
            DynamicPlaylistValidationDeps {
                providers_manager: &providers_manager,
                credential_encryption: None,
                credential_repo: None,
            },
            &RoomId::new(),
            &UserId::new(),
            "alist".to_string(),
            serde_json::json!({"path": "/movies", "server_id": "srv"}),
            Some("alist".to_string()),
        )
        .await,
        "dynamic playlist source should require credential repo wiring",
    );

    match err {
        Error::ServiceUnavailable(message) => {
            assert!(message.contains("requires credential repository wiring"));
        }
        other => std::panic::panic_any(format!("expected ServiceUnavailable, got {other:?}")),
    }
}

#[tokio::test]
async fn validate_dynamic_playlist_source_requires_provider_registry_for_unknown_provider_instance()
{
    let providers_manager = test_builtin_providers_manager().await;
    let err = err(
        validate_dynamic_playlist_source_with_dependencies(
            DynamicPlaylistValidationDeps {
                providers_manager: &providers_manager,
                credential_encryption: None,
                credential_repo: None,
            },
            &RoomId::new(),
            &UserId::new(),
            "alist".to_string(),
            serde_json::json!({"path": "/movies", "server_id": "srv"}),
            Some("alist-main".to_string()),
        )
        .await,
        "unknown provider instance should fail validation",
    );

    match err {
        Error::NotFound(message) => {
            assert!(message.contains("Provider instance not found"));
        }
        other => std::panic::panic_any(format!("expected NotFound, got {other:?}")),
    }
}

#[tokio::test]
async fn validate_dynamic_playlist_source_rejects_provider_type_mismatch() {
    let providers_manager = test_builtin_providers_manager().await;
    let err = err(
        validate_dynamic_playlist_source_with_dependencies(
            DynamicPlaylistValidationDeps {
                providers_manager: &providers_manager,
                credential_encryption: None,
                credential_repo: None,
            },
            &RoomId::new(),
            &UserId::new(),
            "alist".to_string(),
            serde_json::json!({"url": "https://example.com/video.mp4"}),
            Some("direct_url".to_string()),
        )
        .await,
        "provider type mismatch should fail validation",
    );

    match err {
        Error::InvalidInput(message) => assert!(message.contains("is type 'direct_url'")),
        other => std::panic::panic_any(format!("expected InvalidInput, got {other:?}")),
    }
}

#[tokio::test]
async fn validate_dynamic_playlist_source_rejects_non_dynamic_provider() {
    let providers_manager = test_builtin_providers_manager().await;
    let err = err(
        validate_dynamic_playlist_source_with_dependencies(
            DynamicPlaylistValidationDeps {
                providers_manager: &providers_manager,
                credential_encryption: None,
                credential_repo: None,
            },
            &RoomId::new(),
            &UserId::new(),
            "direct_url".to_string(),
            serde_json::json!({"url": "https://example.com/video.mp4"}),
            Some("direct_url".to_string()),
        )
        .await,
        "non-dynamic provider should fail validation",
    );

    match err {
        Error::InvalidInput(message) => {
            assert!(message.contains("does not support dynamic folders"));
        }
        other => std::panic::panic_any(format!("expected InvalidInput, got {other:?}")),
    }
}

#[tokio::test]
async fn validate_dynamic_playlist_source_rejects_oversized_config_before_provider_use() {
    let providers_manager = test_builtin_providers_manager().await;
    let err = err(
        validate_dynamic_playlist_source_with_dependencies(
            DynamicPlaylistValidationDeps {
                providers_manager: &providers_manager,
                credential_encryption: None,
                credential_repo: None,
            },
            &RoomId::new(),
            &UserId::new(),
            "direct_url".to_string(),
            serde_json::json!({"data": "x".repeat(2 * 1024 * 1024)}),
            Some("direct_url".to_string()),
        )
        .await,
        "oversized dynamic playlist config should fail validation",
    );

    match err {
        Error::InvalidInput(message) => {
            assert!(message.contains("source_config too large"));
            assert!(
                !message.contains("does not support dynamic folders"),
                "size guard should run before provider-specific validation"
            );
        }
        other => std::panic::panic_any(format!("expected InvalidInput, got {other:?}")),
    }
}

#[tokio::test]
async fn validate_dynamic_playlist_source_runs_provider_validation() {
    let providers_manager = test_builtin_providers_manager().await;
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let credential_encryption = test_credential_encryption();
    let credential_repo = Arc::new(UserProviderCredentialRepository::new_with_encryption(
        pool,
        credential_encryption.clone(),
    ));
    let err = err(
        validate_dynamic_playlist_source_with_dependencies(
            DynamicPlaylistValidationDeps {
                providers_manager: &providers_manager,
                credential_encryption: Some(&credential_encryption),
                credential_repo: Some(&credential_repo),
            },
            &RoomId::new(),
            &UserId::new(),
            "alist".to_string(),
            serde_json::json!({"path": "", "server_id": "srv"}),
            Some("alist".to_string()),
        )
        .await,
        "provider validation should reject invalid source config",
    );

    match err {
        Error::InvalidInput(message) => {
            assert!(message.contains("Invalid source_config"));
            assert!(message.contains("must not be empty"));
        }
        other => std::panic::panic_any(format!("expected InvalidInput, got {other:?}")),
    }
}

#[tokio::test]
async fn validate_dynamic_playlist_source_rejects_missing_credential_dependency() {
    let providers_manager = test_alist_dependency_check_providers_manager().await;
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let credential_encryption = test_credential_encryption();
    let credential_repo = Arc::new(UserProviderCredentialRepository::new_with_encryption(
        pool,
        credential_encryption.clone(),
    ));
    let err = err(
        validate_dynamic_playlist_source_with_dependencies(
            DynamicPlaylistValidationDeps {
                providers_manager: &providers_manager,
                credential_encryption: Some(&credential_encryption),
                credential_repo: Some(&credential_repo),
            },
            &RoomId::new(),
            &UserId::new(),
            crate::provider::AlistProvider::NAME.to_string(),
            serde_json::json!({"credential_server_id": "missing-server"}),
            None,
        )
        .await,
        "missing credential dependency should fail validation",
    );

    match err {
        Error::InvalidInput(message) => {
            assert!(message.contains("missing credential"), "{message}");
            assert!(message.contains("missing-server"), "{message}");
        }
        other => std::panic::panic_any(format!("expected InvalidInput, got {other:?}")),
    }
}

#[tokio::test]
async fn validate_dynamic_playlist_source_allows_missing_optional_credential_dependency() {
    let providers_manager = test_alist_dependency_check_providers_manager().await;
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let credential_encryption = test_credential_encryption();
    let credential_repo = Arc::new(UserProviderCredentialRepository::new_with_encryption(
        pool,
        credential_encryption.clone(),
    ));

    ok(
        validate_dynamic_playlist_source_with_dependencies(
            DynamicPlaylistValidationDeps {
                providers_manager: &providers_manager,
                credential_encryption: Some(&credential_encryption),
                credential_repo: Some(&credential_repo),
            },
            &RoomId::new(),
            &UserId::new(),
            crate::provider::AlistProvider::NAME.to_string(),
            serde_json::json!({
                "credential_server_id": "viewer-optional-server",
                "credential_optional": true
            }),
            None,
        )
        .await,
        "optional credential dependency should not block source validation",
    );
}

#[tokio::test]
async fn validate_dynamic_playlist_source_passes_creator_as_credential_owner() {
    let providers_manager = test_credential_check_providers_manager().await;
    let user_id = UserId::new();

    ok(
        validate_dynamic_playlist_source_with_dependencies(
            DynamicPlaylistValidationDeps {
                providers_manager: &providers_manager,
                credential_encryption: None,
                credential_repo: None,
            },
            &RoomId::new(),
            &user_id,
            "credential_check".to_string(),
            serde_json::json!({}),
            Some("credential_check".to_string()),
        )
        .await,
        "dynamic playlist validation should expose creator credentials",
    );
}
