use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use synctv_core::models::{MediaSourceConfig, RoomId, RoomPlaybackState, SourceProvider, UserId};
use synctv_core::provider::store::{ProviderStore, ProviderStoreExt, ProviderStoreResolver};
use synctv_core::provider::{MediaProvider, PlaybackResult, ProviderContext};
use synctv_core::service::{ProvidersManager, RoomService};

use super::ClientApiImpl;
use crate::impls::admin::AdminApiImpl;
use crate::impls::ApiError;

const LIFECYCLE_STORE_NAME: &str = "playback_lifecycle";
const LIFECYCLE_TTL: Duration = Duration::from_hours(12);
const LIFECYCLE_LOCK_TTL: Duration = Duration::from_secs(15);
const PROGRESS_MIN_INTERVAL_MILLIS: i64 = 10_000;
const PROGRESS_MIN_POSITION_DELTA: f64 = 1.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ProviderPlaybackSession {
    provider: String,
    provider_instance_name: Option<String>,
    actor_user_id: UserId,
    credential_owner_id: Option<UserId>,
    source_config: MediaSourceConfig,
    room_target_key: String,
    provider_session_id: String,
    #[serde(default)]
    started: bool,
    started_at_millis: i64,
    last_progress_position: Option<f64>,
    last_progress_at_millis: Option<i64>,
    last_paused: Option<bool>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct ProviderPlaybackSessionSet {
    sessions: Vec<ProviderPlaybackSession>,
}

pub(crate) struct ProviderPlaybackRegistration<'a> {
    pub state: &'a RoomPlaybackState,
    pub actor_user_id: &'a UserId,
    pub provider: &'a dyn MediaProvider,
    pub provider_name: &'a str,
    pub provider_instance_name: Option<&'a str>,
    pub credential_owner_id: Option<&'a UserId>,
    pub source_config: &'a MediaSourceConfig,
    pub result: &'a PlaybackResult,
}

#[async_trait]
pub(crate) trait ProviderPlaybackLifecycleApi {
    fn lifecycle_room_service(&self) -> &RoomService;

    fn lifecycle_provider_stores(&self) -> &Arc<dyn ProviderStoreResolver>;

    fn lifecycle_providers_manager(&self) -> Arc<ProvidersManager>;

    fn lifecycle_provider_context<'a>(
        &'a self,
        session: &'a ProviderPlaybackSession,
        room_id: RoomId,
    ) -> Result<ProviderContext<'a>, ApiError>;

    fn lifecycle_store(&self) -> Arc<dyn ProviderStore> {
        self.lifecycle_provider_stores().load(LIFECYCLE_STORE_NAME)
    }

    async fn load_lifecycle_sessions(
        store: &dyn ProviderStore,
        room_id: RoomId,
    ) -> Result<ProviderPlaybackSessionSet, ApiError> {
        match store
            .get::<ProviderPlaybackSessionSet>(&room_sessions_key(room_id))
            .await
        {
            Ok(Some(sessions)) => Ok(sessions),
            Ok(None) => Ok(ProviderPlaybackSessionSet::default()),
            Err(error) => Err(ApiError::Internal(format!(
                "Failed to load provider playback lifecycle sessions for room {room_id}: {error}"
            ))),
        }
    }

    async fn save_lifecycle_sessions(
        store: &dyn ProviderStore,
        room_id: RoomId,
        sessions: &ProviderPlaybackSessionSet,
    ) -> Result<(), ApiError> {
        let result = if sessions.sessions.is_empty() {
            store.delete(&room_sessions_key(room_id)).await
        } else {
            store
                .set(&room_sessions_key(room_id), sessions, LIFECYCLE_TTL)
                .await
        };

        result.map_err(|error| {
            ApiError::Internal(format!(
                "Failed to persist provider playback lifecycle sessions for room {room_id}: {error}"
            ))
        })
    }

    fn lifecycle_context<'a>(
        &'a self,
        session: &'a ProviderPlaybackSession,
        room_id: RoomId,
    ) -> Result<synctv_core::provider::ProviderContext<'a>, ApiError> {
        let ctx = self.lifecycle_provider_context(session, room_id)?;
        Ok(ctx.with_store(
            self.lifecycle_provider_stores()
                .load(session.provider.as_str()),
        ))
    }

    async fn resolve_lifecycle_provider(
        &self,
        session: &ProviderPlaybackSession,
    ) -> Option<Arc<dyn MediaProvider>> {
        let providers_manager = self.lifecycle_providers_manager();
        let provider = match session.provider.parse::<SourceProvider>() {
            Ok(provider) => provider,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    provider = %session.provider,
                    session_id = %session.provider_session_id,
                    "provider playback lifecycle skipped unknown provider"
                );
                return None;
            }
        };

        match providers_manager
            .resolve_provider(provider, session.provider_instance_name.as_deref())
            .await
        {
            Ok(provider) => Some(provider),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    provider = %session.provider,
                    session_id = %session.provider_session_id,
                    "Failed to resolve provider for playback lifecycle hook"
                );
                None
            }
        }
    }

    async fn stop_lifecycle_session(
        &self,
        room_id: RoomId,
        session: &ProviderPlaybackSession,
        position: f64,
    ) {
        let Some(provider) = self.resolve_lifecycle_provider(session).await else {
            return;
        };
        let ctx = match self.lifecycle_context(session, room_id) {
            Ok(ctx) => ctx,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    provider = %session.provider,
                    session_id = %session.provider_session_id,
                    room_id = %room_id,
                    "Provider playback stop hook skipped"
                );
                return;
            }
        };
        if let Err(error) = provider
            .on_playback_stop(
                &ctx,
                session.provider_session_id.as_str(),
                &session.source_config,
                position,
            )
            .await
        {
            tracing::warn!(
                error = %error,
                provider = %session.provider,
                session_id = %session.provider_session_id,
                room_id = %room_id,
                "Provider playback stop hook failed"
            );
        }
    }

    async fn start_lifecycle_session(
        &self,
        room_id: RoomId,
        provider: &dyn MediaProvider,
        provider_name: &str,
        session: &ProviderPlaybackSession,
    ) -> bool {
        let ctx = match self.lifecycle_context(session, room_id) {
            Ok(ctx) => ctx,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    provider = %provider_name,
                    session_id = %session.provider_session_id,
                    room_id = %room_id,
                    "Provider playback start hook skipped"
                );
                return false;
            }
        };
        match provider
            .on_playback_start(
                &ctx,
                session.provider_session_id.as_str(),
                &session.source_config,
            )
            .await
        {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    provider = provider_name,
                    session_id = %session.provider_session_id,
                    room_id = %room_id,
                    "Provider playback start hook failed"
                );
                false
            }
        }
    }

    async fn register_provider_playback_session(
        &self,
        registration: ProviderPlaybackRegistration<'_>,
    ) -> Result<(), ApiError> {
        let ProviderPlaybackRegistration {
            state,
            provider,
            provider_name,
            provider_instance_name,
            actor_user_id,
            credential_owner_id,
            source_config,
            result,
        } = registration;
        let Some(provider_session_id) = provider.playback_lifecycle_session_id(result) else {
            return Ok(());
        };
        let Some(room_target_key) = playback_target_key(state)? else {
            return Ok(());
        };
        let store = self.lifecycle_store();

        let room_id = state.room_id;
        let lock_key = room_lock_key(room_id);
        let _guard = store
            .lock(&lock_key, LIFECYCLE_LOCK_TTL)
            .await
            .map_err(|error| {
                ApiError::Internal(format!(
                    "Failed to lock provider playback lifecycle state for room {room_id}: {error}"
                ))
            })?;

        let normalized_provider_instance_name =
            normalize_lifecycle_provider_instance_name(provider_instance_name);
        let mut sessions = Self::load_lifecycle_sessions(store.as_ref(), room_id).await?;
        if let Some(existing) = sessions.sessions.iter_mut().find(|session| {
            session.provider == provider_name
                && session.provider_instance_name == normalized_provider_instance_name
                && session.provider_session_id == provider_session_id
        }) {
            existing.actor_user_id = *actor_user_id;
            existing.credential_owner_id = credential_owner_id.copied();
            existing.source_config = source_config.clone();
            existing.room_target_key = room_target_key;
            existing.last_paused = Some(!state.is_playing);
            if state.is_playing && !existing.started {
                existing.started = self
                    .start_lifecycle_session(room_id, provider, provider_name, existing)
                    .await;
            }
            Self::save_lifecycle_sessions(store.as_ref(), room_id, &sessions).await?;
            return Ok(());
        }

        let mut session = ProviderPlaybackSession {
            provider: provider_name.to_string(),
            provider_instance_name: normalized_provider_instance_name,
            actor_user_id: *actor_user_id,
            credential_owner_id: credential_owner_id.copied(),
            source_config: source_config.clone(),
            room_target_key,
            provider_session_id,
            started: false,
            started_at_millis: now_millis(),
            last_progress_position: None,
            last_progress_at_millis: None,
            last_paused: Some(!state.is_playing),
        };

        if state.is_playing {
            session.started = self
                .start_lifecycle_session(room_id, provider, provider_name, &session)
                .await;
        }

        sessions.sessions.push(session);
        Self::save_lifecycle_sessions(store.as_ref(), room_id, &sessions).await
    }

    async fn stop_provider_sessions_for_state(
        &self,
        state: &RoomPlaybackState,
        position: f64,
    ) -> Result<(), ApiError> {
        let Some(target_key) = playback_target_key(state)? else {
            return Ok(());
        };
        let store = self.lifecycle_store();

        let room_id = state.room_id;
        let lock_key = room_lock_key(room_id);
        let _guard = store
            .lock(&lock_key, LIFECYCLE_LOCK_TTL)
            .await
            .map_err(|error| {
                ApiError::Internal(format!(
                    "Failed to lock provider playback lifecycle state for room {room_id}: {error}"
                ))
            })?;

        let mut sessions = Self::load_lifecycle_sessions(store.as_ref(), room_id).await?;
        let mut stopped = Vec::new();
        sessions.sessions.retain(|session| {
            let should_stop = session.room_target_key == target_key;
            if should_stop {
                stopped.push(session.clone());
            }
            !should_stop
        });

        for session in &stopped {
            self.stop_lifecycle_session(room_id, session, position)
                .await;
        }

        Self::save_lifecycle_sessions(store.as_ref(), room_id, &sessions).await
    }

    async fn report_provider_progress_for_state(
        &self,
        state: &RoomPlaybackState,
        position: f64,
        is_paused: bool,
        force: bool,
    ) -> Result<(), ApiError> {
        let Some(target_key) = playback_target_key(state)? else {
            return Ok(());
        };
        let store = self.lifecycle_store();

        let room_id = state.room_id;
        let lock_key = room_lock_key(room_id);
        let _guard = store
            .lock(&lock_key, LIFECYCLE_LOCK_TTL)
            .await
            .map_err(|error| {
                ApiError::Internal(format!(
                    "Failed to lock provider playback lifecycle state for room {room_id}: {error}"
                ))
            })?;

        let mut sessions = Self::load_lifecycle_sessions(store.as_ref(), room_id).await?;
        let mut changed = false;
        for session in &mut sessions.sessions {
            if session.room_target_key != target_key {
                continue;
            }
            if !force && !should_report_progress(session, position, is_paused) {
                continue;
            }
            let Some(provider) = self.resolve_lifecycle_provider(session).await else {
                continue;
            };
            if !is_paused && !session.started {
                session.started = self
                    .start_lifecycle_session(
                        room_id,
                        provider.as_ref(),
                        session.provider.as_str(),
                        session,
                    )
                    .await;
            }
            let ctx = match self.lifecycle_context(session, room_id) {
                Ok(ctx) => ctx,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        provider = %session.provider,
                        session_id = %session.provider_session_id,
                        room_id = %room_id,
                        "Provider playback progress hook skipped"
                    );
                    continue;
                }
            };
            if let Err(error) = provider
                .on_playback_progress(
                    &ctx,
                    session.provider_session_id.as_str(),
                    &session.source_config,
                    position,
                    is_paused,
                )
                .await
            {
                tracing::debug!(
                    error = %error,
                    provider = %session.provider,
                    session_id = %session.provider_session_id,
                    room_id = %room_id,
                    "Provider playback progress hook failed"
                );
                continue;
            }
            session.last_progress_position = Some(position);
            session.last_progress_at_millis = Some(now_millis());
            session.last_paused = Some(is_paused);
            changed = true;
        }

        if changed {
            Self::save_lifecycle_sessions(store.as_ref(), room_id, &sessions).await?;
        }
        Ok(())
    }

    async fn handle_provider_lifecycle_transition(
        &self,
        previous: Option<&RoomPlaybackState>,
        current: &RoomPlaybackState,
    ) -> Result<(), ApiError> {
        let previous_target = previous.map(playback_target_key).transpose()?.flatten();
        let current_target = playback_target_key(current)?;

        if previous_target.is_some() && previous_target != current_target {
            if let Some(previous_state) = previous {
                self.stop_provider_sessions_for_state(
                    previous_state,
                    previous_state.computed_position(),
                )
                .await?;
            }
        }

        if current_target.is_some() {
            let paused = !current.is_playing;
            self.report_provider_progress_for_state(
                current,
                current.computed_position(),
                paused,
                previous.is_none_or(|old| old.is_playing != current.is_playing),
            )
            .await?;
        }
        Ok(())
    }

    async fn state_before_playback_state_update(
        &self,
        room_id: &synctv_core::models::RoomId,
    ) -> Result<RoomPlaybackState, ApiError> {
        self.lifecycle_room_service()
            .get_playback_state(room_id)
            .await
            .map_err(ApiError::from)
    }
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn playback_target_key(state: &RoomPlaybackState) -> Result<Option<String>, ApiError> {
    if let Some(media_id) = &state.playing_media_id {
        return Ok(Some(format!("media:{media_id}")));
    }

    state
        .playing_playlist_id
        .as_ref()
        .map(|playlist_id| Ok(format!("playlist:{playlist_id}:{}", state.target_hash()?)))
        .transpose()
}

fn normalize_lifecycle_provider_instance_name(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn room_sessions_key(room_id: RoomId) -> String {
    format!("room:{room_id}:sessions")
}

fn room_lock_key(room_id: RoomId) -> String {
    format!("room:{room_id}:lock")
}

fn should_report_progress(
    session: &ProviderPlaybackSession,
    position: f64,
    is_paused: bool,
) -> bool {
    if session.last_paused != Some(is_paused) {
        return true;
    }

    let Some(last_at) = session.last_progress_at_millis else {
        return true;
    };
    let interval_elapsed = now_millis().saturating_sub(last_at) >= PROGRESS_MIN_INTERVAL_MILLIS;
    let position_changed = session
        .last_progress_position
        .is_none_or(|last| (position - last).abs() >= PROGRESS_MIN_POSITION_DELTA);

    interval_elapsed && position_changed
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

    fn lifecycle_provider_context<'a>(
        &'a self,
        session: &'a ProviderPlaybackSession,
        room_id: RoomId,
    ) -> Result<ProviderContext<'a>, ApiError> {
        self.build_provider_context(
            &session.actor_user_id,
            session.credential_owner_id.as_ref(),
            &room_id,
            None,
            session.provider_instance_name.as_deref(),
            None,
            None,
        )
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

    fn lifecycle_provider_context<'a>(
        &'a self,
        session: &'a ProviderPlaybackSession,
        room_id: RoomId,
    ) -> Result<ProviderContext<'a>, ApiError> {
        let public_user_id = self
            .public_id_codec
            .encode_user_id(session.actor_user_id)
            .map_err(|error| {
                ApiError::Internal(format!("Failed to encode user public id: {error}"))
            })?;
        let public_room_id = self
            .public_id_codec
            .encode_room_id(room_id)
            .map_err(|error| {
                ApiError::Internal(format!("Failed to encode room public id: {error}"))
            })?;

        let mut ctx = ProviderContext::new("synctv")
            .with_user_id(session.actor_user_id)
            .with_public_user_id(public_user_id)
            .with_room_id(room_id)
            .with_public_room_id(public_room_id)
            .with_playback_client_profile(None);
        if let Some(credential_owner_id) = session.credential_owner_id {
            ctx = ctx.with_credential_owner_id(credential_owner_id);
        }
        if let Some(provider_instance_name) = session
            .provider_instance_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            ctx = ctx.with_provider_instance_name(provider_instance_name);
        }
        if let Some(repo) = self.room_service.media_service().credential_repo() {
            ctx = ctx.with_credential_repo(repo.as_ref());
        }
        if let Some(enc) = self.room_service.media_service().credential_encryption() {
            ctx = ctx.with_credential_encryption(enc);
        }
        ctx = ctx.with_provider_access_service(self.provider_access_service.clone());
        Ok(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use async_trait::async_trait;
    use synctv_core::models::{MediaSourceConfig, RoomId};
    use synctv_core::provider::store::ProviderStoreResolver;
    use synctv_core::provider::{
        PlaybackInfo, PlaybackResult, ProviderContext, ProviderError, ProviderStoreRegistry,
    };
    use synctv_core_testing::{TestOptionExt, TestResultExt};

    const LIFECYCLE_TEST_PROVIDER_NAME: &str = synctv_core::provider::DirectUrlProvider::NAME;

    fn locked_clone<T: Clone>(mutex: &std::sync::Mutex<T>) -> T {
        mutex
            .lock()
            .unwrap_or_else(|_| panic!("test mutex lock"))
            .clone()
    }

    fn unwrap_ok<T, E: std::fmt::Debug>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error:?}"),
        }
    }

    fn locked_push<T>(mutex: &std::sync::Mutex<Vec<T>>, value: T) -> Result<(), ProviderError> {
        mutex
            .lock()
            .map_err(|_| ProviderError::ApiError("test mutex was poisoned".to_string()))?
            .push(value);
        Ok(())
    }

    #[derive(Debug)]
    struct LifecycleTestProvider {
        start_calls: Arc<std::sync::Mutex<Vec<String>>>,
        stop_calls: Arc<std::sync::Mutex<Vec<(String, f64)>>>,
        start_failures_remaining: std::sync::atomic::AtomicUsize,
    }

    impl LifecycleTestProvider {
        fn new(
            start_calls: Arc<std::sync::Mutex<Vec<String>>>,
            stop_calls: Arc<std::sync::Mutex<Vec<(String, f64)>>>,
        ) -> Self {
            Self {
                start_calls,
                stop_calls,
                start_failures_remaining: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn with_start_failures(mut self, failures: usize) -> Self {
            self.start_failures_remaining = std::sync::atomic::AtomicUsize::new(failures);
            self
        }
    }

    #[async_trait]
    impl MediaProvider for LifecycleTestProvider {
        fn name(&self) -> &'static str {
            LIFECYCLE_TEST_PROVIDER_NAME
        }

        async fn generate_playback(
            &self,
            _ctx: &ProviderContext<'_>,
            _source_config: &MediaSourceConfig,
        ) -> Result<PlaybackResult, ProviderError> {
            Ok(lifecycle_playback_result("session-a"))
        }

        async fn on_playback_start(
            &self,
            _ctx: &ProviderContext<'_>,
            session_id: &str,
            _source_config: &MediaSourceConfig,
        ) -> Result<(), ProviderError> {
            if self
                .start_failures_remaining
                .fetch_update(
                    std::sync::atomic::Ordering::Relaxed,
                    std::sync::atomic::Ordering::Relaxed,
                    |remaining| remaining.checked_sub(1),
                )
                .is_ok()
            {
                return Err(ProviderError::ApiError(
                    "injected start failure".to_string(),
                ));
            }
            locked_push(&self.start_calls, session_id.to_string())
        }

        async fn on_playback_stop(
            &self,
            _ctx: &ProviderContext<'_>,
            session_id: &str,
            _source_config: &MediaSourceConfig,
            position: f64,
        ) -> Result<(), ProviderError> {
            locked_push(&self.stop_calls, (session_id.to_string(), position))
        }

        fn playback_lifecycle_session_id(&self, result: &PlaybackResult) -> Option<String> {
            result
                .metadata
                .emby
                .as_ref()
                .and_then(|metadata| metadata.play_session_id.clone())
        }
    }

    fn lifecycle_playback_result(session_id: &str) -> PlaybackResult {
        let mut playback_infos = std::collections::HashMap::new();
        playback_infos.insert(
            "direct".to_string(),
            test_provider_playback_info("https://example.com/video.mp4"),
        );

        PlaybackResult {
            playback_infos,
            default_mode: "direct".to_string(),
            provider: "test".to_string(),
            provider_instance_name: None,
            duration_seconds: None,
            is_live: Some(false),
            metadata: synctv_core::models::PlaybackMetadata {
                emby: Some(synctv_core::models::EmbyPlaybackMetadata {
                    play_session_id: Some(session_id.to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        }
    }

    fn test_provider_playback_info(url: &str) -> PlaybackInfo {
        PlaybackInfo {
            medias: vec![synctv_core::models::PlaybackMedia {
                name: String::new(),
                format: "mp4".to_string(),
                expire_at: None,
                metadata: None,
                provider: synctv_core::models::PlaybackMediaProvider::External(
                    synctv_core::models::PlaybackExternalMedia {
                        url: url.to_string(),
                        headers: std::collections::HashMap::new(),
                    },
                ),
            }],
            default_media_index: None,
            subtitles: Vec::new(),
            default_subtitle_index: None,
            danmakus: Vec::new(),
            default_danmaku_index: None,
        }
    }

    async fn lifecycle_test_api(
        provider: Arc<dyn MediaProvider>,
        stores: Arc<dyn ProviderStoreResolver>,
    ) -> ClientApiImpl {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let provider_for_factory = provider.clone();
        let instance_manager =
            Arc::new(synctv_core::service::RemoteProviderManager::new(Arc::new(
                synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
            )));
        let mut providers_manager = synctv_core::service::ProvidersManager::new(instance_manager)
            .checked("providers manager should build");
        providers_manager.register_factory(
            LIFECYCLE_TEST_PROVIDER_NAME,
            Box::new(move |_instance_id, _config, _instance_manager| {
                Ok(provider_for_factory.clone())
            }),
        );
        providers_manager
            .create_provider_with_default_config(
                LIFECYCLE_TEST_PROVIDER_NAME,
                LIFECYCLE_TEST_PROVIDER_NAME,
            )
            .await
            .map(|_| ())
            .checked("create lifecycle test provider");

        let jwt_service =
            synctv_core::service::JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!")
                .checked("test jwt service");
        let username_cache =
            synctv_core::cache::UsernameCache::local_only("lifecycle:user:".to_string(), 100, 60);
        let token_blacklist = Arc::new(synctv_core::service::InMemoryTokenBlacklistStore::new(
            1000, 3600, 86400,
        ));
        let user_service = Arc::new(synctv_core::service::UserService::new_for_tests(
            &pool,
            jwt_service.clone(),
            username_cache,
            token_blacklist,
            synctv_core::cache::KeyBuilder::new("lifecycle"),
            synctv_core::service::auth::BruteForceProtection::in_memory(
                "lifecycle:brute:".to_string(),
            ),
        ));
        let room_service = Arc::new(
            synctv_core::service::RoomService::new_with_providers_and_options(
                pool.clone(),
                (*user_service).clone(),
                Arc::new(providers_manager),
                synctv_core::service::room::RoomServiceOptions::test_defaults_with_settings(pool),
            )
            .checked("room service should build"),
        );

        ClientApiImpl::new_with_runtime(
            crate::impls::ClientApiConfig {
                read_pool: None,
                user_service,
                room_service,
                connection_service: Arc::new(synctv_realtime::sync::ConnectionManager::default()),
                config: Arc::new(synctv_core::Config::default()),
                publish_key_service: None,
                jwt_service: synctv_core::service::JwtService::new(
                    "Test_Secret_Key_For_JWT_Tokens_32Bytes!!",
                )
                .checked("test jwt service"),
                live_streaming_infrastructure: None,
                settings_registry: None,
                public_id_codec: Arc::new(synctv_core::PublicIdCodec::plain()),
                chat_service: None,
                provider_stores: stores,
                email_api: None,
                passkey_service: None,
            },
            crate::test_support::client_api_runtime(),
        )
    }

    #[test]
    fn playback_target_key_treats_static_and_dynamic_targets_as_distinct() {
        let room_id = RoomId::expect_positive(1);
        let mut state = RoomPlaybackState::new(room_id);
        state.playing_media_id = Some(synctv_core::models::MediaId::expect_positive(2));
        assert_eq!(
            playback_target_key(&state)
                .expect("target key should compute")
                .as_deref(),
            Some("media:2")
        );

        state.playing_media_id = None;
        state.playing_playlist_id = Some(synctv_core::models::PlaylistId::expect_positive(3));
        state.target = Some(synctv_core::models::ProviderTarget::alist(
            "target-a".to_string(),
        ));
        let first = playback_target_key(&state)
            .expect("dynamic target key should compute")
            .checked("dynamic target key");

        state.target = Some(synctv_core::models::ProviderTarget::alist(
            "target-b".to_string(),
        ));
        let second = playback_target_key(&state)
            .expect("dynamic target key should compute")
            .checked("dynamic target key");
        assert_ne!(first, second);
        assert!(first.starts_with("playlist:3:"));
    }

    #[test]
    fn progress_report_is_forced_on_pause_state_change() {
        let session = ProviderPlaybackSession {
            provider: "emby".to_string(),
            provider_instance_name: None,
            actor_user_id: UserId::expect_positive(100),
            credential_owner_id: None,
            source_config: synctv_core_testing::direct_url_media_source_config(
                "https://example.com/video.mp4",
            ),
            room_target_key: "media:m".to_string(),
            provider_session_id: "session".to_string(),
            started: true,
            started_at_millis: now_millis(),
            last_progress_position: Some(10.0),
            last_progress_at_millis: Some(now_millis()),
            last_paused: Some(false),
        };

        assert!(should_report_progress(&session, 10.0, true));
        assert!(!should_report_progress(&session, 10.0, false));
    }

    #[tokio::test]
    async fn lifecycle_context_preserves_actor_and_credential_owner_identity() {
        let provider: Arc<dyn MediaProvider> = Arc::new(LifecycleTestProvider::new(
            Arc::new(std::sync::Mutex::new(Vec::new())),
            Arc::new(std::sync::Mutex::new(Vec::new())),
        ));
        let stores: Arc<dyn ProviderStoreResolver> =
            Arc::new(ProviderStoreRegistry::local_only("lifecycle-context-test:"));
        let api = lifecycle_test_api(provider, stores).await;
        let room_id = RoomId::expect_positive(10);
        let actor_user_id = UserId::expect_positive(20);
        let credential_owner_id = UserId::expect_positive(30);
        let session = ProviderPlaybackSession {
            provider: LIFECYCLE_TEST_PROVIDER_NAME.to_string(),
            provider_instance_name: Some("primary".to_string()),
            actor_user_id,
            credential_owner_id: Some(credential_owner_id),
            source_config: synctv_core_testing::direct_url_media_source_config(
                "https://example.com/video.mp4",
            ),
            room_target_key: "media:1".to_string(),
            provider_session_id: "session".to_string(),
            started: true,
            started_at_millis: now_millis(),
            last_progress_position: None,
            last_progress_at_millis: None,
            last_paused: None,
        };

        let ctx = unwrap_ok(
            api.lifecycle_context(&session, room_id),
            "lifecycle context",
        );

        assert_eq!(ctx.user_id(), Some(&actor_user_id));
        assert_eq!(ctx.credential_owner_id(), Some(&credential_owner_id));
        assert_eq!(ctx.provider_instance_name(), Some("primary"));
    }

    #[tokio::test]
    async fn lifecycle_registration_refreshes_existing_session_metadata() {
        let provider: Arc<dyn MediaProvider> = Arc::new(LifecycleTestProvider::new(
            Arc::new(std::sync::Mutex::new(Vec::new())),
            Arc::new(std::sync::Mutex::new(Vec::new())),
        ));
        let stores: Arc<dyn ProviderStoreResolver> =
            Arc::new(ProviderStoreRegistry::local_only("lifecycle-refresh-test:"));
        let api = lifecycle_test_api(provider.clone(), stores.clone()).await;
        let room_id = RoomId::expect_positive(11);
        let first_actor = UserId::expect_positive(21);
        let second_actor = UserId::expect_positive(22);
        let credential_owner = UserId::expect_positive(31);
        let mut state = RoomPlaybackState::new(room_id);
        state.playing_media_id = Some(synctv_core::models::MediaId::expect_positive(41));
        state.is_playing = true;

        let first_source =
            synctv_core_testing::direct_url_media_source_config("https://example.com/old.mp4");
        let second_source =
            synctv_core_testing::direct_url_media_source_config("https://example.com/new.mp4");
        let result = lifecycle_playback_result("session-refresh");

        api.register_provider_playback_session(ProviderPlaybackRegistration {
            state: &state,
            actor_user_id: &first_actor,
            provider: provider.as_ref(),
            provider_name: provider.name(),
            provider_instance_name: Some("  primary  "),
            credential_owner_id: None,
            source_config: &first_source,
            result: &result,
        })
        .await
        .checked("register initial lifecycle session");
        api.register_provider_playback_session(ProviderPlaybackRegistration {
            state: &state,
            actor_user_id: &second_actor,
            provider: provider.as_ref(),
            provider_name: provider.name(),
            provider_instance_name: Some("primary"),
            credential_owner_id: Some(&credential_owner),
            source_config: &second_source,
            result: &result,
        })
        .await
        .checked("refresh lifecycle session");

        let lifecycle_store = stores.load(LIFECYCLE_STORE_NAME);
        let sessions = ClientApiImpl::load_lifecycle_sessions(lifecycle_store.as_ref(), room_id)
            .await
            .checked("load lifecycle sessions");
        assert_eq!(sessions.sessions.len(), 1);
        let session = &sessions.sessions[0];
        assert_eq!(session.actor_user_id, second_actor);
        assert_eq!(session.credential_owner_id, Some(credential_owner));
        assert_eq!(session.source_config, second_source);
        assert_eq!(session.provider_instance_name.as_deref(), Some("primary"));
    }

    #[tokio::test]
    async fn failed_lifecycle_start_remains_retryable() {
        let start_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider: Arc<dyn MediaProvider> = Arc::new(
            LifecycleTestProvider::new(
                start_calls.clone(),
                Arc::new(std::sync::Mutex::new(Vec::new())),
            )
            .with_start_failures(1),
        );
        let stores: Arc<dyn ProviderStoreResolver> = Arc::new(ProviderStoreRegistry::local_only(
            "lifecycle-retry-start-test:",
        ));
        let api = lifecycle_test_api(provider.clone(), stores.clone()).await;
        let room_id = RoomId::expect_positive(12);
        let actor_user_id = UserId::expect_positive(23);
        let mut state = RoomPlaybackState::new(room_id);
        state.playing_media_id = Some(synctv_core::models::MediaId::expect_positive(42));
        state.is_playing = true;
        let source_config =
            synctv_core_testing::direct_url_media_source_config("https://example.com/retry.mp4");
        let result = lifecycle_playback_result("session-retry");

        api.register_provider_playback_session(ProviderPlaybackRegistration {
            state: &state,
            actor_user_id: &actor_user_id,
            provider: provider.as_ref(),
            provider_name: provider.name(),
            provider_instance_name: None,
            credential_owner_id: None,
            source_config: &source_config,
            result: &result,
        })
        .await
        .checked("register lifecycle session");

        let lifecycle_store = stores.load(LIFECYCLE_STORE_NAME);
        let sessions = ClientApiImpl::load_lifecycle_sessions(lifecycle_store.as_ref(), room_id)
            .await
            .checked("load lifecycle sessions after failed start");
        assert_eq!(sessions.sessions.len(), 1);
        assert!(
            !sessions.sessions[0].started,
            "failed start hook must leave lifecycle session retryable"
        );
        assert!(locked_clone(start_calls.as_ref()).is_empty());

        api.report_provider_progress_for_state(&state, 1.0, false, true)
            .await
            .checked("progress should retry lifecycle start");

        assert_eq!(locked_clone(start_calls.as_ref()), vec!["session-retry"]);
        let sessions = ClientApiImpl::load_lifecycle_sessions(lifecycle_store.as_ref(), room_id)
            .await
            .checked("load lifecycle sessions after retry");
        assert!(sessions.sessions[0].started);
    }

    #[tokio::test]
    async fn lifecycle_registration_and_target_switch_start_stop_provider_session() {
        let start_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stop_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider: Arc<dyn MediaProvider> = Arc::new(LifecycleTestProvider::new(
            start_calls.clone(),
            stop_calls.clone(),
        ));
        let stores: Arc<dyn ProviderStoreResolver> =
            Arc::new(ProviderStoreRegistry::local_only("lifecycle-test:"));
        let api = lifecycle_test_api(provider.clone(), stores.clone()).await;

        let room_id = RoomId::expect_positive(1);
        let actor_user_id = UserId::expect_positive(2);
        let mut first_state = RoomPlaybackState::new(room_id);
        first_state.playing_media_id = Some(synctv_core::models::MediaId::expect_positive(11));
        first_state.is_playing = true;
        first_state.position = 42.5;

        let source_config =
            synctv_core_testing::direct_url_media_source_config("https://example.com/first.mp4");
        let result = lifecycle_playback_result("session-a");
        api.register_provider_playback_session(ProviderPlaybackRegistration {
            state: &first_state,
            actor_user_id: &actor_user_id,
            provider: provider.as_ref(),
            provider_name: provider.name(),
            provider_instance_name: None,
            credential_owner_id: None,
            source_config: &source_config,
            result: &result,
        })
        .await
        .checked("register lifecycle session");

        assert_eq!(
            locked_clone(start_calls.as_ref()),
            vec!["session-a"],
            "registering an actively playing provider result must call provider start"
        );

        let mut second_state = RoomPlaybackState::new(room_id);
        second_state.playing_media_id = Some(synctv_core::models::MediaId::expect_positive(12));
        second_state.is_playing = true;

        api.handle_provider_lifecycle_transition(Some(&first_state), &second_state)
            .await
            .checked("handle lifecycle transition");

        let stops = locked_clone(stop_calls.as_ref());
        assert_eq!(
            stops.len(),
            1,
            "switching targets must call provider stop for the old session exactly once"
        );
        assert_eq!(stops[0].0, "session-a");
        assert!(
            (stops[0].1 - first_state.position).abs() < 1.0,
            "provider stop should receive the old playback position, got {}",
            stops[0].1
        );

        let lifecycle_store = stores.load(LIFECYCLE_STORE_NAME);
        let sessions = ClientApiImpl::load_lifecycle_sessions(lifecycle_store.as_ref(), room_id)
            .await
            .checked("load lifecycle sessions");
        assert!(
            sessions.sessions.is_empty(),
            "stopped lifecycle sessions must be removed from the store"
        );
    }
}
