use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use synctv_core::models::{RoomId, RoomPlaybackState, UserId};
use synctv_core::provider::store::{ProviderStore, ProviderStoreExt, ProviderStoreResolver};
use synctv_core::provider::{MediaProvider, PlaybackResult, ProviderContext};
use synctv_core::service::{ProvidersManager, RoomService};

use super::ClientApiImpl;
use crate::impls::admin::AdminApiImpl;

const LIFECYCLE_STORE_NAME: &str = "playback_lifecycle";
const LIFECYCLE_TTL: Duration = Duration::from_hours(12);
const LIFECYCLE_LOCK_TTL: Duration = Duration::from_secs(15);
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_secs(10);
const PROGRESS_MIN_POSITION_DELTA: f64 = 1.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ProviderPlaybackSession {
    provider: String,
    provider_instance_name: Option<String>,
    credential_owner_id: Option<String>,
    source_config: Value,
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
    pub provider: &'a dyn MediaProvider,
    pub provider_name: &'a str,
    pub provider_instance_name: Option<&'a str>,
    pub credential_owner_id: Option<&'a UserId>,
    pub source_config: &'a Value,
    pub result: &'a PlaybackResult,
}

#[async_trait]
pub(crate) trait ProviderPlaybackLifecycleApi {
    fn lifecycle_room_service(&self) -> &RoomService;

    fn lifecycle_provider_stores(&self) -> Option<&Arc<dyn ProviderStoreResolver>>;

    fn lifecycle_providers_manager(&self) -> Arc<ProvidersManager>;

    fn lifecycle_provider_context<'a>(
        &'a self,
        session: &'a ProviderPlaybackSession,
        room_id: RoomId,
    ) -> ProviderContext<'a>;

    fn lifecycle_store(&self) -> Option<Arc<dyn ProviderStore>> {
        self.lifecycle_provider_stores()
            .map(|stores| stores.load(LIFECYCLE_STORE_NAME))
    }

    async fn load_lifecycle_sessions(
        store: &dyn ProviderStore,
        room_id: RoomId,
    ) -> ProviderPlaybackSessionSet {
        match store
            .get::<ProviderPlaybackSessionSet>(&room_sessions_key(room_id))
            .await
        {
            Ok(Some(sessions)) => sessions,
            Ok(None) => ProviderPlaybackSessionSet::default(),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %room_id,
                    "Failed to load provider playback lifecycle sessions"
                );
                ProviderPlaybackSessionSet::default()
            }
        }
    }

    async fn save_lifecycle_sessions(
        store: &dyn ProviderStore,
        room_id: RoomId,
        sessions: &ProviderPlaybackSessionSet,
    ) {
        let result = if sessions.sessions.is_empty() {
            store.delete(&room_sessions_key(room_id)).await
        } else {
            store
                .set(&room_sessions_key(room_id), sessions, LIFECYCLE_TTL)
                .await
        };

        if let Err(error) = result {
            tracing::warn!(
                error = %error,
                room_id = %room_id,
                "Failed to persist provider playback lifecycle sessions"
            );
        }
    }

    fn lifecycle_context<'a>(
        &'a self,
        session: &'a ProviderPlaybackSession,
        room_id: RoomId,
    ) -> synctv_core::provider::ProviderContext<'a> {
        let ctx = self.lifecycle_provider_context(session, room_id);
        match self.lifecycle_provider_stores() {
            Some(stores) => ctx.with_store(stores.load(session.provider.as_str())),
            None => ctx,
        }
    }

    async fn resolve_lifecycle_provider(
        &self,
        session: &ProviderPlaybackSession,
    ) -> Option<Arc<dyn MediaProvider>> {
        let providers_manager = self.lifecycle_providers_manager();

        match providers_manager
            .resolve_provider(
                session.provider.as_str(),
                session.provider_instance_name.as_deref(),
            )
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
        let ctx = self.lifecycle_context(session, room_id);
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
    ) {
        let ctx = self.lifecycle_context(session, room_id);
        if let Err(error) = provider
            .on_playback_start(
                &ctx,
                session.provider_session_id.as_str(),
                &session.source_config,
            )
            .await
        {
            tracing::warn!(
                error = %error,
                provider = provider_name,
                session_id = %session.provider_session_id,
                room_id = %room_id,
                "Provider playback start hook failed"
            );
        }
    }

    async fn register_provider_playback_session(
        &self,
        registration: ProviderPlaybackRegistration<'_>,
    ) {
        let ProviderPlaybackRegistration {
            state,
            provider,
            provider_name,
            provider_instance_name,
            credential_owner_id,
            source_config,
            result,
        } = registration;
        let Some(provider_session_id) = provider.playback_lifecycle_session_id(result) else {
            return;
        };
        let Some(room_target_key) = playback_target_key(state) else {
            return;
        };
        let Some(store) = self.lifecycle_store() else {
            tracing::debug!(
                provider = provider_name,
                session_id = %provider_session_id,
                "Provider playback lifecycle store is unavailable; lifecycle hooks disabled"
            );
            return;
        };

        let room_id = state.room_id;
        let lock_key = room_lock_key(room_id);
        let Ok(_guard) = store.lock(&lock_key, LIFECYCLE_LOCK_TTL).await else {
            tracing::warn!(
                room_id = %room_id,
                provider = provider_name,
                session_id = %provider_session_id,
                "Failed to lock provider playback lifecycle state for session registration"
            );
            return;
        };

        let mut sessions = Self::load_lifecycle_sessions(store.as_ref(), room_id).await;
        if let Some(existing) = sessions.sessions.iter_mut().find(|session| {
            session.provider == provider_name
                && session.provider_instance_name.as_deref() == provider_instance_name
                && session.provider_session_id == provider_session_id
        }) {
            if state.is_playing && !existing.started {
                self.start_lifecycle_session(room_id, provider, provider_name, existing)
                    .await;
                existing.started = true;
                Self::save_lifecycle_sessions(store.as_ref(), room_id, &sessions).await;
            }
            return;
        }

        let mut session = ProviderPlaybackSession {
            provider: provider_name.to_string(),
            provider_instance_name: provider_instance_name
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(std::string::ToString::to_string),
            credential_owner_id: credential_owner_id.map(ToString::to_string),
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
            self.start_lifecycle_session(room_id, provider, provider_name, &session)
                .await;
            session.started = true;
        }

        sessions.sessions.push(session);
        Self::save_lifecycle_sessions(store.as_ref(), room_id, &sessions).await;
    }

    async fn stop_provider_sessions_for_state(&self, state: &RoomPlaybackState, position: f64) {
        let Some(target_key) = playback_target_key(state) else {
            return;
        };
        let Some(store) = self.lifecycle_store() else {
            return;
        };

        let room_id = state.room_id;
        let lock_key = room_lock_key(room_id);
        let Ok(_guard) = store.lock(&lock_key, LIFECYCLE_LOCK_TTL).await else {
            tracing::warn!(
                room_id = %room_id,
                "Failed to lock provider playback lifecycle state for session cleanup"
            );
            return;
        };

        let mut sessions = Self::load_lifecycle_sessions(store.as_ref(), room_id).await;
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

        Self::save_lifecycle_sessions(store.as_ref(), room_id, &sessions).await;
    }

    async fn report_provider_progress_for_state(
        &self,
        state: &RoomPlaybackState,
        position: f64,
        is_paused: bool,
        force: bool,
    ) {
        let Some(target_key) = playback_target_key(state) else {
            return;
        };
        let Some(store) = self.lifecycle_store() else {
            return;
        };

        let room_id = state.room_id;
        let lock_key = room_lock_key(room_id);
        let Ok(_guard) = store.lock(&lock_key, LIFECYCLE_LOCK_TTL).await else {
            tracing::debug!(
                room_id = %room_id,
                "Failed to lock provider playback lifecycle state for progress report"
            );
            return;
        };

        let mut sessions = Self::load_lifecycle_sessions(store.as_ref(), room_id).await;
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
                self.start_lifecycle_session(
                    room_id,
                    provider.as_ref(),
                    session.provider.as_str(),
                    session,
                )
                .await;
                session.started = true;
            }
            let ctx = self.lifecycle_context(session, room_id);
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
            Self::save_lifecycle_sessions(store.as_ref(), room_id, &sessions).await;
        }
    }

    async fn handle_provider_lifecycle_transition(
        &self,
        previous: Option<&RoomPlaybackState>,
        current: &RoomPlaybackState,
    ) {
        let previous_target = previous.and_then(playback_target_key);
        let current_target = playback_target_key(current);

        if previous_target.is_some() && previous_target != current_target {
            if let Some(previous_state) = previous {
                self.stop_provider_sessions_for_state(
                    previous_state,
                    previous_state.computed_position(),
                )
                .await;
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
            .await;
        }
    }

    async fn state_before_playback_update(
        &self,
        room_id: &synctv_core::models::RoomId,
    ) -> Option<RoomPlaybackState> {
        match self
            .lifecycle_room_service()
            .get_playback_state(room_id)
            .await
        {
            Ok(state) => Some(state),
            Err(error) => {
                tracing::debug!(
                    error = %error,
                    room_id = %room_id,
                    "Failed to load previous playback state for provider lifecycle transition"
                );
                None
            }
        }
    }
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn dynamic_target_hash(target: &[u8]) -> String {
    hex::encode(Sha256::digest(target))
}

fn playback_target_key(state: &RoomPlaybackState) -> Option<String> {
    if let Some(media_id) = &state.playing_media_id {
        return Some(format!("media:{media_id}"));
    }

    state.playing_playlist_id.as_ref().map(|playlist_id| {
        format!(
            "playlist:{playlist_id}:{}",
            dynamic_target_hash(&state.target)
        )
    })
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
    let interval_elapsed = now_millis().saturating_sub(last_at)
        >= i64::try_from(PROGRESS_MIN_INTERVAL.as_millis()).unwrap_or(i64::MAX);
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

    fn lifecycle_provider_stores(&self) -> Option<&Arc<dyn ProviderStoreResolver>> {
        self.provider_stores.as_ref()
    }

    fn lifecycle_providers_manager(&self) -> Arc<ProvidersManager> {
        self.providers_manager.clone().unwrap_or_else(|| {
            self.room_service
                .media_service()
                .providers_manager()
                .clone()
        })
    }

    fn lifecycle_provider_context<'a>(
        &'a self,
        session: &'a ProviderPlaybackSession,
        room_id: RoomId,
    ) -> ProviderContext<'a> {
        let credential_owner_id = session
            .credential_owner_id
            .as_deref()
            .and_then(|value| value.parse::<UserId>().ok());
        let user_id = credential_owner_id.unwrap_or(UserId::MAX);

        self.build_provider_context(
            &user_id,
            credential_owner_id.as_ref(),
            &room_id,
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

    fn lifecycle_provider_stores(&self) -> Option<&Arc<dyn ProviderStoreResolver>> {
        self.provider_stores.as_ref()
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
    ) -> ProviderContext<'a> {
        let credential_owner_id = session
            .credential_owner_id
            .as_deref()
            .and_then(|value| value.parse::<UserId>().ok());
        let user_id = credential_owner_id.unwrap_or(UserId::MAX);
        let public_user_id = self
            .public_id_codec
            .encode_user_id(user_id)
            .unwrap_or_else(|_| user_id.to_string());
        let public_room_id = self
            .public_id_codec
            .encode_room_id(room_id)
            .unwrap_or_else(|_| room_id.to_string());

        let mut ctx = ProviderContext::new("synctv")
            .with_user_id(user_id)
            .with_public_user_id(public_user_id)
            .with_room_id(room_id)
            .with_public_room_id(public_room_id)
            .with_playback_client_profile(None);
        if let Some(credential_owner_id) = credential_owner_id {
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
        if let Some(access_service) = &self.provider_access_service {
            ctx = ctx.with_provider_access_service(access_service.clone());
        }
        ctx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use async_trait::async_trait;
    use synctv_core::models::RoomId;
    use synctv_core::provider::store::ProviderStoreResolver;
    use synctv_core::provider::{
        PlaybackInfo, PlaybackResult, ProviderContext, ProviderError, ProviderStoreRegistry,
    };

    #[derive(Debug)]
    struct LifecycleTestProvider {
        start_calls: Arc<std::sync::Mutex<Vec<String>>>,
        stop_calls: Arc<std::sync::Mutex<Vec<(String, f64)>>>,
    }

    impl LifecycleTestProvider {
        fn new(
            start_calls: Arc<std::sync::Mutex<Vec<String>>>,
            stop_calls: Arc<std::sync::Mutex<Vec<(String, f64)>>>,
        ) -> Self {
            Self {
                start_calls,
                stop_calls,
            }
        }
    }

    #[async_trait]
    impl MediaProvider for LifecycleTestProvider {
        fn name(&self) -> &'static str {
            "lifecycle_test"
        }

        async fn generate_playback(
            &self,
            _ctx: &ProviderContext<'_>,
            _source_config: &Value,
        ) -> Result<PlaybackResult, ProviderError> {
            Ok(lifecycle_playback_result("session-a"))
        }

        async fn on_playback_start(
            &self,
            _ctx: &ProviderContext<'_>,
            session_id: &str,
            _source_config: &Value,
        ) -> Result<(), ProviderError> {
            self.start_calls
                .lock()
                .expect("start calls lock")
                .push(session_id.to_string());
            Ok(())
        }

        async fn on_playback_stop(
            &self,
            _ctx: &ProviderContext<'_>,
            session_id: &str,
            _source_config: &Value,
            position: f64,
        ) -> Result<(), ProviderError> {
            self.stop_calls
                .lock()
                .expect("stop calls lock")
                .push((session_id.to_string(), position));
            Ok(())
        }

        fn playback_lifecycle_session_id(&self, result: &PlaybackResult) -> Option<String> {
            result
                .metadata
                .get("session_id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        }
    }

    fn lifecycle_playback_result(session_id: &str) -> PlaybackResult {
        let mut playback_infos = std::collections::HashMap::new();
        playback_infos.insert(
            "direct".to_string(),
            PlaybackInfo {
                urls: vec!["https://example.com/video.mp4".to_string()],
                format: "mp4".to_string(),
                headers: std::collections::HashMap::new(),
                subtitles: Vec::new(),
                expires_at: None,
                cors_proxy_required: false,
            },
        );

        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "session_id".to_string(),
            Value::String(session_id.to_string()),
        );

        PlaybackResult {
            playback_infos,
            default_mode: "direct".to_string(),
            metadata,
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
        let mut providers_manager = synctv_core::service::ProvidersManager::new(instance_manager);
        providers_manager.register_factory(
            "lifecycle_test",
            Box::new(move |_instance_id, _config, _instance_manager| {
                Ok(provider_for_factory.clone())
            }),
        );
        providers_manager
            .create_provider("lifecycle_test", "lifecycle_test", &Value::Null)
            .await
            .expect("create lifecycle test provider");

        let jwt_service =
            synctv_core::service::JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!")
                .expect("test jwt service");
        let username_cache =
            synctv_core::cache::UsernameCache::local_only("lifecycle:user:".to_string(), 100, 60);
        let token_blacklist = Arc::new(synctv_core::service::InMemoryTokenBlacklistStore::new(
            1000, 3600, 86400,
        ));
        let user_service = Arc::new(synctv_core::service::UserService::new(
            &pool,
            jwt_service.clone(),
            username_cache,
            synctv_core::config::PasswordComplexityConfig::default(),
            token_blacklist,
            synctv_core::cache::KeyBuilder::new("lifecycle"),
            synctv_core::service::auth::BruteForceProtection::in_memory(
                "lifecycle:brute:".to_string(),
            ),
        ));
        let room_service = Arc::new(synctv_core::service::RoomService::new(
            pool,
            (*user_service).clone(),
        ));

        ClientApiImpl::new(
            user_service,
            room_service,
            Arc::new(synctv_realtime::sync::ConnectionManager::default()),
            Arc::new(synctv_core::Config::default()),
            None,
            synctv_core::service::JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!")
                .expect("test jwt service"),
            None,
            Some(Arc::new(providers_manager)),
            None,
            Arc::new(crate::PublicIdCodec::default_for_tests()),
        )
        .with_provider_stores(stores)
    }

    #[test]
    fn playback_target_key_treats_static_and_dynamic_targets_as_distinct() {
        let room_id = RoomId::expect_positive(1);
        let mut state = RoomPlaybackState::new(room_id);
        state.playing_media_id = Some(synctv_core::models::MediaId::expect_positive(2));
        assert_eq!(playback_target_key(&state).as_deref(), Some("media:2"));

        state.playing_media_id = None;
        state.playing_playlist_id = Some(synctv_core::models::PlaylistId::expect_positive(3));
        state.target = b"target-a".to_vec();
        let first = playback_target_key(&state).expect("dynamic target key");

        state.target = b"target-b".to_vec();
        let second = playback_target_key(&state).expect("dynamic target key");
        assert_ne!(first, second);
        assert!(first.starts_with("playlist:3:"));
    }

    #[test]
    fn progress_report_is_forced_on_pause_state_change() {
        let session = ProviderPlaybackSession {
            provider: "emby".to_string(),
            provider_instance_name: None,
            credential_owner_id: None,
            source_config: serde_json::json!({}),
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
        let mut first_state = RoomPlaybackState::new(room_id);
        first_state.playing_media_id = Some(synctv_core::models::MediaId::expect_positive(11));
        first_state.is_playing = true;
        first_state.position = 42.5;

        let source_config = serde_json::json!({"item_id": "first"});
        let result = lifecycle_playback_result("session-a");
        api.register_provider_playback_session(ProviderPlaybackRegistration {
            state: &first_state,
            provider: provider.as_ref(),
            provider_name: "lifecycle_test",
            provider_instance_name: None,
            credential_owner_id: None,
            source_config: &source_config,
            result: &result,
        })
        .await;

        assert_eq!(
            start_calls.lock().expect("start calls lock").as_slice(),
            ["session-a"],
            "registering an actively playing provider result must call provider start"
        );

        let mut second_state = RoomPlaybackState::new(room_id);
        second_state.playing_media_id = Some(synctv_core::models::MediaId::expect_positive(12));
        second_state.is_playing = true;

        api.handle_provider_lifecycle_transition(Some(&first_state), &second_state)
            .await;

        let stops = stop_calls.lock().expect("stop calls lock").clone();
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
        let sessions =
            ClientApiImpl::load_lifecycle_sessions(lifecycle_store.as_ref(), room_id).await;
        assert!(
            sessions.sessions.is_empty(),
            "stopped lifecycle sessions must be removed from the store"
        );
    }
}
