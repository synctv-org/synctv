use crate::{
    cache::{CloneableError, ConsistencyCoordinator, FenceReadResult, PlaybackStateCache},
    models::{RoomId, RoomPlaybackState},
    service::playback::PlaybackService,
    Error, Result,
};

impl PlaybackService {
    #[cfg(any(test, feature = "test-support"))]
    pub fn has_l2_cache(&self) -> bool {
        self.l2_cache.read().is_some()
    }

    pub(super) fn playback_l2_cache(&self) -> Option<PlaybackStateCache> {
        self.l2_cache.read().clone()
    }

    pub(super) async fn write_playback_cache_entry(
        playback_cache: &moka::future::Cache<String, RoomPlaybackState>,
        l2_cache: Option<PlaybackStateCache>,
        state: &RoomPlaybackState,
    ) {
        let cache_key = state.room_id.to_string();
        let new_state = state.clone();
        playback_cache
            .entry(cache_key)
            .and_upsert_with(|maybe_entry| {
                let result = match maybe_entry {
                    Some(entry) => {
                        let current = entry.into_value();
                        if new_state.version >= current.version {
                            new_state.clone()
                        } else {
                            current
                        }
                    }
                    None => new_state.clone(),
                };
                std::future::ready(result)
            })
            .await;

        if let Some(l2_cache) = l2_cache {
            if let Err(error) = l2_cache
                .set_if_version_at_least(&state.room_id, state.clone())
                .await
            {
                tracing::warn!(
                    error = %error,
                    room_id = %state.room_id,
                    "Failed to update playback state in L2 cache"
                );
            }
        }
    }

    pub(super) async fn write_playback_cache(&self, state: &RoomPlaybackState) {
        Self::write_playback_cache_entry(&self.playback_cache, self.playback_l2_cache(), state)
            .await;
    }

    pub async fn get_state(&self, room_id: &RoomId) -> Result<RoomPlaybackState> {
        self.get_state_by_fence(room_id).await
    }

    async fn get_state_by_fence(&self, room_id: &RoomId) -> Result<RoomPlaybackState> {
        let domain = Self::playback_domain(room_id);
        if !self.consistency.is_authoritative() {
            ConsistencyCoordinator::record_db_fallback(&domain, "non_authoritative_fence");
            return self.reload_state_from_store(room_id).await;
        }

        if let Some(l2_cache) = self.playback_l2_cache() {
            if let Some(fence_key) = self.consistency.fence_key(&domain) {
                let cache_key = room_id.to_string();
                let l1_value = self.playback_cache.get(&cache_key).await;
                match l2_cache
                    .get_by_fence_key_with_l1_value(room_id, &fence_key, l1_value)
                    .await
                {
                    Ok(FenceReadResult::Hit(state)) => {
                        self.playback_cache.insert(cache_key, state.clone()).await;
                        return Ok(state);
                    }
                    Ok(FenceReadResult::DbFallback) => {
                        ConsistencyCoordinator::record_db_fallback(&domain, "stale_cache");
                        return self.reload_state_from_store(room_id).await;
                    }
                    Ok(FenceReadResult::Unsupported) => {}
                    Err(error) => {
                        tracing::warn!(
                            room_id = %room_id,
                            error = %error,
                            "Playback fence-key cache read failed; falling back to version read"
                        );
                        ConsistencyCoordinator::record_db_fallback(&domain, "fence_key_read_error");
                    }
                }
            }
        }

        let fence_version = match self.consistency.current_committed_version(&domain).await {
            Ok(Some(version)) => version,
            Ok(None) => {
                ConsistencyCoordinator::record_db_fallback(&domain, "missing_fence");
                return self.reload_state_from_store(room_id).await;
            }
            Err(error) => {
                tracing::warn!(
                    room_id = %room_id,
                    error = %error,
                    "Playback version fence unavailable; bypassing cache"
                );
                ConsistencyCoordinator::record_db_fallback(&domain, "fence_unavailable");
                return self.reload_state_from_store(room_id).await;
            }
        };

        let cache_key = room_id.to_string();
        if let Some(state) = self.playback_cache.get(&cache_key).await {
            if state.version >= fence_version {
                crate::metrics::cache::CACHE_HITS
                    .with_label_values(&["playback", "l1"])
                    .inc();
                return Ok(state);
            }
        }

        if let Some(l2_cache) = self.playback_l2_cache() {
            match l2_cache.get_l2(room_id).await {
                Ok(Some(state)) if state.version >= fence_version => {
                    self.playback_cache
                        .insert(cache_key.clone(), state.clone())
                        .await;
                    crate::metrics::cache::CACHE_HITS
                        .with_label_values(&["playback", "l2"])
                        .inc();
                    return Ok(state);
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        room_id = %room_id,
                        error = %error,
                        "Playback L2 read failed; bypassing cache"
                    );
                    ConsistencyCoordinator::record_db_fallback(&domain, "l2_error");
                }
            }
        }

        ConsistencyCoordinator::record_db_fallback(&domain, "stale_cache");
        self.reload_state_from_store(room_id).await
    }

    pub async fn get_state_eventually_consistent(
        &self,
        room_id: &RoomId,
    ) -> Result<RoomPlaybackState> {
        let cache_key = room_id.to_string();

        if let Some(state) = self.playback_cache.get(&cache_key).await {
            crate::metrics::cache::CACHE_HITS
                .with_label_values(&["playback", "l1"])
                .inc();
            return Ok(state);
        }

        if let Some(l2_cache) = self.playback_l2_cache() {
            if let Some(state) = l2_cache.get(room_id).await? {
                self.playback_cache
                    .insert(cache_key.clone(), state.clone())
                    .await;
                crate::metrics::cache::CACHE_HITS
                    .with_label_values(&["playback", "l2"])
                    .inc();
                tracing::debug!(
                    room_id = %room_id,
                    version = state.version,
                    "Playback state cache hit (L2)"
                );
                return Ok(state);
            }
        }

        let repo = self.playback_repo.clone();
        let cache = self.playback_cache.clone();
        let l2_cache = self.playback_l2_cache();
        let room_id_clone = *room_id;

        let state = self
            .single_flight
            .do_work(cache_key, async move {
                let state = match repo.get(&room_id_clone).await {
                    Ok(Some(state)) => state,
                    Ok(None) => match repo.create_or_get(&room_id_clone).await {
                        Ok(state) => state,
                        Err(error) => return Err(CloneableError::from(error)),
                    },
                    Err(error) => return Err(CloneableError::from(error)),
                };

                cache.insert(state.room_id.to_string(), state.clone()).await;

                if let Some(ref l2_cache) = l2_cache {
                    if let Err(error) = l2_cache
                        .set_if_version_at_least(&state.room_id, state.clone())
                        .await
                    {
                        tracing::warn!(
                            room_id = %state.room_id,
                            error = %error,
                            "Failed to set playback state in L2 cache"
                        );
                    }
                }

                Ok(state)
            })
            .await
            .map_err(|error| match error {
                crate::cache::SingleFlightError::WorkerFailed => Error::Internal(
                    "SingleFlight worker failed during playback state fetch".to_string(),
                ),
                crate::cache::SingleFlightError::Inner(error) => Error::from(error),
            })?;

        crate::metrics::cache::CACHE_MISSES
            .with_label_values(&["playback", "l1_l2"])
            .inc();

        Ok(state)
    }

    pub async fn reload_state_from_store(&self, room_id: &RoomId) -> Result<RoomPlaybackState> {
        let cache_key = room_id.to_string();
        self.playback_cache.invalidate(&cache_key).await;
        if let Some(l2_cache) = self.playback_l2_cache() {
            if let Err(error) = l2_cache.invalidate(room_id).await {
                tracing::warn!(
                    error = %error,
                    room_id = %room_id,
                    "Failed to invalidate playback state from L2 cache before DB reload"
                );
            }
        }

        let state = match self.playback_repo.get(room_id).await? {
            Some(state) => state,
            None => self.playback_repo.create_or_get(room_id).await?,
        };

        self.consistency
            .repair_after_db_read(&Self::playback_domain(room_id), state.version)
            .await;
        self.seed_playback_version_fence_after_reload(room_id, state.version)
            .await;

        if let Some(l2_cache) = self.playback_l2_cache() {
            match l2_cache
                .set_if_version_at_least(room_id, state.clone())
                .await
            {
                Ok(true) => {
                    self.playback_cache.insert(cache_key, state.clone()).await;
                }
                Ok(false) => {
                    tracing::debug!(
                        room_id = %room_id,
                        version = state.version,
                        "Skipped playback cache update after DB reload because L2 has newer state"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        "Failed to update playback state in L2 cache after DB reload"
                    );
                }
            }
        } else {
            self.playback_cache.insert(cache_key, state.clone()).await;
        }

        Ok(state)
    }
}
