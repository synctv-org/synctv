use std::sync::Arc;

use async_trait::async_trait;
use synctv_core::models::{
    ProviderPlaybackSessionRecord, ProviderPlaybackStopReason, RoomId, RoomPlaybackState,
};
use synctv_core::provider::{MediaProvider, ProviderContext, ProviderStoreResolver};
use synctv_core::repository::ProviderPlaybackSessionRepository;
use synctv_core::service::{ProvidersManager, RoomService};

use super::ClientApiImpl;
use crate::impls::admin::AdminApiImpl;
use crate::impls::ApiError;

const CLEANUP_BATCH_SIZE: usize = 100;

fn session_provider(
    session: &ProviderPlaybackSessionRecord,
) -> synctv_core::models::SourceProvider {
    session.session.provider()
}

fn provider_name(session: &ProviderPlaybackSessionRecord) -> &'static str {
    session_provider(session).as_str()
}

#[async_trait]
pub trait ProviderPlaybackLifecycleApi: Send + Sync {
    fn lifecycle_room_service(&self) -> &RoomService;

    fn lifecycle_provider_stores(&self) -> &Arc<dyn ProviderStoreResolver>;

    fn lifecycle_providers_manager(&self) -> Arc<ProvidersManager>;

    fn lifecycle_session_repo(&self) -> Result<ProviderPlaybackSessionRepository, ApiError> {
        let credential_repo = self
            .lifecycle_room_service()
            .media_service()
            .credential_repo()
            .ok_or_else(|| {
                ApiError::Internal(
                    "provider playback lifecycle requires credential repository wiring".to_string(),
                )
            })?;
        Ok(ProviderPlaybackSessionRepository::new(
            credential_repo.pool().clone(),
        ))
    }

    fn lifecycle_provider_context<'a>(
        &'a self,
        session: &'a ProviderPlaybackSessionRecord,
    ) -> ProviderContext<'a> {
        let mut ctx = ProviderContext::new("synctv")
            .with_user_id(session.credential_owner_id)
            .with_credential_owner_id(session.credential_owner_id)
            .with_room_id(session.room_id)
            .with_playback_generation(session.playback_generation)
            .with_playback_is_playing(false);
        if let Some(provider_instance_name) = session.provider_instance_name.as_deref() {
            ctx = ctx.with_provider_instance_name(provider_instance_name);
        }
        ctx = self
            .lifecycle_room_service()
            .media_service()
            .attach_provider_credential_context(ctx);
        ctx.with_store(
            self.lifecycle_provider_stores()
                .load(provider_name(session)),
        )
    }

    async fn resolve_session_provider(
        &self,
        session: &ProviderPlaybackSessionRecord,
    ) -> Result<Arc<dyn MediaProvider>, ApiError> {
        self.lifecycle_providers_manager()
            .resolve_provider(
                session_provider(session),
                session.provider_instance_name.as_deref(),
            )
            .await
            .map_err(ApiError::from)
    }

    async fn report_provider_progress_for_state(
        &self,
        state: &RoomPlaybackState,
        position: f64,
        is_paused: bool,
        _force: bool,
    ) -> Result<(), ApiError> {
        if !position.is_finite() || position < 0.0 {
            return Err(ApiError::InvalidInput(
                "provider playback position must be finite and non-negative".to_string(),
            ));
        }
        let repo = self.lifecycle_session_repo()?;
        repo.renew_generation(state.room_id, state.playback_generation, is_paused)
            .await
            .map_err(ApiError::from)?;
        let sessions = repo
            .active_for_generation(state.room_id, state.playback_generation)
            .await
            .map_err(ApiError::from)?;
        let mut first_error = None;
        for session in sessions {
            let result = async {
                let provider = self.resolve_session_provider(&session).await?;
                let lifecycle = provider.as_playback_session_lifecycle().ok_or_else(|| {
                    ApiError::Internal(format!(
                        "Provider '{}' owns a lifecycle session without a lifecycle handler",
                        provider.name()
                    ))
                })?;
                lifecycle
                    .progress(
                        &self.lifecycle_provider_context(&session),
                        &session,
                        position,
                        is_paused,
                    )
                    .await
                    .map_err(ApiError::from)
            }
            .await;
            if let Err(error) = result {
                tracing::warn!(
                    error = %error,
                    session_id = session.id,
                    provider = provider_name(&session),
                    "Provider playback progress failed"
                );
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn handle_provider_lifecycle_transition(
        &self,
        previous: Option<&RoomPlaybackState>,
        current: &RoomPlaybackState,
    ) -> Result<(), ApiError> {
        let repo = self.lifecycle_session_repo()?;
        if let Some(previous) = previous {
            if previous.playback_generation != current.playback_generation {
                let reason = if current.playing_media_id.is_none()
                    && current.playing_playlist_id.is_none()
                {
                    ProviderPlaybackStopReason::Stopped
                } else {
                    ProviderPlaybackStopReason::TargetChanged
                };
                repo.request_generation_stop(
                    previous.room_id,
                    previous.playback_generation,
                    previous.computed_position(),
                    reason,
                )
                .await
                .map_err(ApiError::from)?;
            }
        }
        if current.playing_media_id.is_some() || current.playing_playlist_id.is_some() {
            self.report_provider_progress_for_state(
                current,
                current.computed_position(),
                !current.is_playing,
                previous.is_none_or(|previous| previous.is_playing != current.is_playing),
            )
            .await?;
        }
        Ok(())
    }

    async fn cleanup_claimed_session(
        &self,
        repo: &ProviderPlaybackSessionRepository,
        session: ProviderPlaybackSessionRecord,
    ) {
        let result = async {
            let provider = self.resolve_session_provider(&session).await?;
            let lifecycle = provider.as_playback_session_lifecycle().ok_or_else(|| {
                ApiError::Internal(format!(
                    "Provider '{}' owns a lifecycle session without a lifecycle handler",
                    provider.name()
                ))
            })?;
            lifecycle
                .cleanup(&self.lifecycle_provider_context(&session), &session)
                .await
                .map_err(ApiError::from)?;
            if let Some(version) = session.resource_version.as_deref() {
                self.lifecycle_provider_stores()
                    .load(provider.name())
                    .delete(&format!("v:{version}"))
                    .await
                    .map_err(|error| {
                        ApiError::Internal(format!(
                            "failed to invalidate provider playback version '{version}': {error}"
                        ))
                    })?;
            }
            repo.delete_claimed(session.id, session.cleanup_fence)
                .await
                .map_err(ApiError::from)?;
            Ok::<(), ApiError>(())
        }
        .await;

        if let Err(error) = result {
            tracing::warn!(
                error = %error,
                session_id = session.id,
                provider = provider_name(&session),
                attempts = session.cleanup_attempts,
                "Provider playback cleanup failed; retry scheduled"
            );
            if let Err(retry_error) = repo
                .retry_claimed(session.id, session.cleanup_fence, session.cleanup_attempts)
                .await
            {
                tracing::error!(
                    error = %retry_error,
                    session_id = session.id,
                    "Failed to schedule provider playback cleanup retry"
                );
            }
        }
    }

    async fn reap_provider_lifecycle_sessions(&self, force: bool) -> Result<(), ApiError> {
        let repo = self.lifecycle_session_repo()?;
        if force {
            repo.request_all_stop(ProviderPlaybackStopReason::Shutdown)
                .await
                .map_err(ApiError::from)?;
        }
        loop {
            let sessions = repo
                .claim_cleanup(i64::try_from(CLEANUP_BATCH_SIZE).unwrap_or(i64::MAX))
                .await
                .map_err(ApiError::from)?;
            let count = sessions.len();
            for session in sessions {
                self.cleanup_claimed_session(&repo, session).await;
            }
            if !force || count < CLEANUP_BATCH_SIZE {
                break;
            }
        }
        Ok(())
    }

    async fn state_before_playback_state_update(
        &self,
        room_id: &RoomId,
    ) -> Result<RoomPlaybackState, ApiError> {
        self.lifecycle_room_service()
            .get_playback_state(room_id)
            .await
            .map_err(ApiError::from)
    }
}

#[async_trait]
impl ProviderPlaybackLifecycleApi for ClientApiImpl {
    fn lifecycle_room_service(&self) -> &RoomService {
        self.room_service.as_ref()
    }

    fn lifecycle_provider_stores(&self) -> &Arc<dyn ProviderStoreResolver> {
        &self.provider_stores
    }

    fn lifecycle_providers_manager(&self) -> Arc<ProvidersManager> {
        self.room_service
            .media_service()
            .providers_manager()
            .clone()
    }
}

#[async_trait]
impl ProviderPlaybackLifecycleApi for AdminApiImpl {
    fn lifecycle_room_service(&self) -> &RoomService {
        self.room_service.as_ref()
    }

    fn lifecycle_provider_stores(&self) -> &Arc<dyn ProviderStoreResolver> {
        &self.provider_stores
    }

    fn lifecycle_providers_manager(&self) -> Arc<ProvidersManager> {
        self.room_service
            .media_service()
            .providers_manager()
            .clone()
    }
}
