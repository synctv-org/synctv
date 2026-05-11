use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use synctv_core::models::{RoomId, RoomPlaybackState, UserId};
use synctv_core::provider::store::{ProviderStore, ProviderStoreExt};
use synctv_core::provider::{MediaProvider, PlaybackResult};

use super::ClientApiImpl;

const LIFECYCLE_STORE_NAME: &str = "playback_lifecycle";
const LIFECYCLE_TTL: Duration = Duration::from_hours(12);
const LIFECYCLE_LOCK_TTL: Duration = Duration::from_secs(15);
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_secs(10);
const PROGRESS_MIN_POSITION_DELTA: f64 = 1.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderPlaybackSession {
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
struct ProviderPlaybackSessionSet {
    sessions: Vec<ProviderPlaybackSession>,
}

pub(super) struct ProviderPlaybackRegistration<'a> {
    pub state: &'a RoomPlaybackState,
    pub provider: &'a dyn MediaProvider,
    pub provider_name: &'a str,
    pub provider_instance_name: Option<&'a str>,
    pub credential_owner_id: Option<&'a UserId>,
    pub source_config: &'a Value,
    pub result: &'a PlaybackResult,
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

impl ClientApiImpl {
    fn lifecycle_store(&self) -> Option<std::sync::Arc<dyn ProviderStore>> {
        self.provider_stores
            .as_ref()
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
        let user_id = session
            .credential_owner_id
            .as_ref()
            .and_then(|id| id.parse::<UserId>().ok())
            .unwrap_or(UserId::MAX);
        let credential_owner_id = session
            .credential_owner_id
            .as_ref()
            .and_then(|id| id.parse::<UserId>().ok());
        let ctx = self.build_provider_context(
            &user_id,
            credential_owner_id.as_ref(),
            &room_id,
            session.provider_instance_name.as_deref(),
            None,
            None,
        );
        match &self.provider_stores {
            Some(stores) => ctx.with_store(stores.load(session.provider.as_str())),
            None => ctx,
        }
    }

    async fn resolve_lifecycle_provider(
        &self,
        session: &ProviderPlaybackSession,
    ) -> Option<std::sync::Arc<dyn MediaProvider>> {
        let Some(providers_manager) = &self.providers_manager else {
            tracing::warn!(
                provider = %session.provider,
                session_id = %session.provider_session_id,
                "Cannot dispatch provider playback lifecycle hook without ProvidersManager"
            );
            return None;
        };

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

    pub(super) async fn register_provider_playback_session(
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

    pub(super) async fn stop_provider_sessions_for_state(
        &self,
        state: &RoomPlaybackState,
        position: f64,
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

    pub(super) async fn report_provider_progress_for_state(
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

    pub(super) async fn handle_provider_lifecycle_transition(
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
                    previous_state.computed_current_time(),
                )
                .await;
            }
        }

        if current_target.is_some() {
            let paused = !current.is_playing;
            self.report_provider_progress_for_state(
                current,
                current.computed_current_time(),
                paused,
                previous.is_none_or(|old| old.is_playing != current.is_playing),
            )
            .await;
        }
    }

    pub(super) async fn state_before_playback_update(
        &self,
        room_id: &synctv_core::models::RoomId,
    ) -> Option<RoomPlaybackState> {
        match self.room_service.get_playback_state(room_id).await {
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

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_core::models::RoomId;

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
}
