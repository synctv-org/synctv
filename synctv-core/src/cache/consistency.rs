//! Cache consistency policy and version-fence primitives.
//!
//! Strong correctness paths must not rely on async invalidation delivery. A
//! version fence is a small authoritative consistency token that lets readers
//! verify whether a cached value was written after the latest mutation for the
//! same logical domain. It is Redis-backed whenever a shared Redis runtime is
//! configured and memory-backed only for local-only deployments without Redis.
//! If the fence cannot be read, callers on authorization or resource-existence
//! paths must bypass cache and read the database.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::models::{MediaId, PlaylistId, RoomId, UserId};
use crate::{
    Error, RedisConnectionRuntime, Result, SharedStateMode, SharedStateProfile,
    redis_runtime_snapshot,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsistencyPolicy {
    EventuallyConsistent,
    ReadYourWrites,
    Strong,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CacheDomain {
    Permission {
        room_id: RoomId,
        user_id: UserId,
    },
    RoomMembership {
        room_id: RoomId,
        user_id: UserId,
    },
    RoomSettings {
        room_id: RoomId,
    },
    Playback {
        room_id: RoomId,
    },
    MediaResource {
        room_id: RoomId,
        media_id: MediaId,
    },
    Playlist {
        room_id: RoomId,
        playlist_id: PlaylistId,
    },
    UserAuthSecurity {
        user_id: UserId,
    },
    RuntimeSetting {
        key: String,
    },
}

impl CacheDomain {
    #[must_use]
    pub fn fence_key_suffix(&self) -> String {
        match self {
            Self::Permission { room_id, user_id } => {
                format!("permission:{room_id}:{user_id}")
            }
            Self::RoomMembership { room_id, user_id } => {
                format!("room_membership:{room_id}:{user_id}")
            }
            Self::RoomSettings { room_id } => format!("room_settings:{room_id}"),
            Self::Playback { room_id } => format!("playback:{room_id}"),
            Self::MediaResource { room_id, media_id } => {
                format!("media_resource:{room_id}:{media_id}")
            }
            Self::Playlist {
                room_id,
                playlist_id,
            } => format!("playlist:{room_id}:{playlist_id}"),
            Self::UserAuthSecurity { user_id } => format!("user_auth_security:{user_id}"),
            Self::RuntimeSetting { key } => format!("runtime_setting:{key}"),
        }
    }

    fn from_fence_key_suffix(suffix: &str) -> Option<Self> {
        let parts = suffix.split(':').collect::<Vec<_>>();
        match parts.as_slice() {
            ["permission", room_id, user_id] => Some(Self::Permission {
                room_id: room_id.parse().ok()?,
                user_id: user_id.parse().ok()?,
            }),
            ["room_membership", room_id, user_id] => Some(Self::RoomMembership {
                room_id: room_id.parse().ok()?,
                user_id: user_id.parse().ok()?,
            }),
            ["room_settings", room_id] => Some(Self::RoomSettings {
                room_id: room_id.parse().ok()?,
            }),
            ["playback", room_id] => Some(Self::Playback {
                room_id: room_id.parse().ok()?,
            }),
            ["media_resource", room_id, media_id] => Some(Self::MediaResource {
                room_id: room_id.parse().ok()?,
                media_id: media_id.parse().ok()?,
            }),
            ["playlist", room_id, playlist_id] => Some(Self::Playlist {
                room_id: room_id.parse().ok()?,
                playlist_id: playlist_id.parse().ok()?,
            }),
            ["user_auth_security", user_id] => Some(Self::UserAuthSecurity {
                user_id: user_id.parse().ok()?,
            }),
            ["runtime_setting", key] if !key.is_empty() => Some(Self::RuntimeSetting {
                key: (*key).to_string(),
            }),
            _ => None,
        }
    }
}

impl fmt::Display for CacheDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.fence_key_suffix())
    }
}

#[must_use]
fn cache_domain_metric_label(domain: &CacheDomain) -> &'static str {
    match domain {
        CacheDomain::Permission { .. } => "permission",
        CacheDomain::RoomMembership { .. } => "room_membership",
        CacheDomain::RoomSettings { .. } => "room_settings",
        CacheDomain::Playback { .. } => "playback",
        CacheDomain::MediaResource { .. } => "media_resource",
        CacheDomain::Playlist { .. } => "playlist",
        CacheDomain::UserAuthSecurity { .. } => "user_auth_security",
        CacheDomain::RuntimeSetting { .. } => "runtime_setting",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionedCacheValue<T> {
    pub version: i64,
    pub value: T,
}

#[async_trait]
pub trait VersionFenceStore: Send + Sync {
    async fn current_version(&self, domain: &CacheDomain) -> Result<Option<i64>>;

    async fn current_state(&self, domain: &CacheDomain) -> Result<Option<VersionFenceState>> {
        Ok(self
            .current_version(domain)
            .await?
            .map(VersionFenceState::committed))
    }

    async fn current_versions(&self, domains: &[CacheDomain]) -> Result<Vec<Option<i64>>> {
        let mut versions = Vec::with_capacity(domains.len());
        for domain in domains {
            let state = self.current_state(domain).await?;
            versions.push(
                if state.as_ref().is_some_and(VersionFenceState::has_pending) {
                    None
                } else {
                    state.map(|state| state.committed_version)
                },
            );
        }
        Ok(versions)
    }

    async fn bump_version(&self, domain: &CacheDomain) -> Result<i64>;

    async fn set_version_at_least(&self, domain: &CacheDomain, version: i64) -> Result<i64>;

    async fn reserve_next_after_observed_version(
        &self,
        domain: &CacheDomain,
        observed_version: i64,
    ) -> Result<i64>;

    async fn begin_write(
        &self,
        domain: &CacheDomain,
        observed_version: i64,
    ) -> Result<VersionFenceReservation> {
        let version = self
            .reserve_next_after_observed_version(domain, observed_version)
            .await?;
        Ok(VersionFenceReservation {
            version,
            token: String::new(),
            started_at_ms: None,
        })
    }

    async fn commit_write(
        &self,
        domain: &CacheDomain,
        reservation: &VersionFenceReservation,
    ) -> Result<i64> {
        self.set_version_at_least(domain, reservation.version).await
    }

    async fn abort_write(
        &self,
        _domain: &CacheDomain,
        _reservation: &VersionFenceReservation,
    ) -> Result<()> {
        Ok(())
    }

    async fn pending_domains(&self) -> Result<Vec<CacheDomain>> {
        Ok(Vec::new())
    }

    fn is_authoritative(&self) -> bool;

    fn fence_key(&self, _domain: &CacheDomain) -> Option<String> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionFenceState {
    pub committed_version: i64,
    pub pending_version: Option<i64>,
    pub pending_token: Option<String>,
    pub pending_started_at_ms: Option<i64>,
}

impl VersionFenceState {
    #[must_use]
    pub const fn committed(version: i64) -> Self {
        Self {
            committed_version: version,
            pending_version: None,
            pending_token: None,
            pending_started_at_ms: None,
        }
    }

    #[must_use]
    pub const fn has_pending(&self) -> bool {
        self.pending_version.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionFenceReservation {
    pub version: i64,
    pub token: String,
    pub started_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceRepairDecision {
    KeepPending,
    FinalizePending,
    ExpirePending,
    AdvanceCommitted,
    Noop,
}

#[derive(Debug, Default)]
pub struct LocalVersionFenceStore {
    state: Mutex<LocalVersionFenceState>,
}

#[derive(Debug, Default)]
struct LocalVersionFenceState {
    versions: HashMap<CacheDomain, i64>,
    pending: HashMap<CacheDomain, VersionFenceReservation>,
}

impl LocalVersionFenceStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl VersionFenceStore for LocalVersionFenceStore {
    async fn current_version(&self, domain: &CacheDomain) -> Result<Option<i64>> {
        Ok(self.state.lock().versions.get(domain).copied())
    }

    async fn current_state(&self, domain: &CacheDomain) -> Result<Option<VersionFenceState>> {
        let state = self.state.lock();
        let committed = state.versions.get(domain).copied();
        let pending = state.pending.get(domain).cloned();
        match (committed, pending) {
            (None, None) => Ok(None),
            (committed, pending) => Ok(Some(VersionFenceState {
                committed_version: committed.unwrap_or(0),
                pending_version: pending.as_ref().map(|reservation| reservation.version),
                pending_token: pending
                    .as_ref()
                    .map(|reservation| reservation.token.clone()),
                pending_started_at_ms: pending
                    .as_ref()
                    .and_then(|reservation| reservation.started_at_ms),
            })),
        }
    }

    async fn current_versions(&self, domains: &[CacheDomain]) -> Result<Vec<Option<i64>>> {
        let state = self.state.lock();
        Ok(domains
            .iter()
            .map(|domain| {
                if state.pending.contains_key(domain) {
                    None
                } else {
                    state.versions.get(domain).copied()
                }
            })
            .collect())
    }

    async fn bump_version(&self, domain: &CacheDomain) -> Result<i64> {
        let mut state = self.state.lock();
        let version = state.versions.entry(domain.clone()).or_insert(0);
        *version += 1;
        Ok(*version)
    }

    async fn set_version_at_least(&self, domain: &CacheDomain, version: i64) -> Result<i64> {
        let mut state = self.state.lock();
        let current = state.versions.entry(domain.clone()).or_insert(0);
        if version > *current {
            *current = version;
        }
        let committed = *current;

        if state
            .pending
            .get(domain)
            .is_some_and(|reservation| reservation.version <= committed)
        {
            state.pending.remove(domain);
        }

        Ok(committed)
    }

    async fn reserve_next_after_observed_version(
        &self,
        domain: &CacheDomain,
        observed_version: i64,
    ) -> Result<i64> {
        let mut state = self.state.lock();
        let current = state.versions.entry(domain.clone()).or_insert(0);
        if *current > observed_version {
            return Err(Error::OptimisticLockConflict);
        }

        *current = observed_version + 1;
        Ok(*current)
    }

    async fn begin_write(
        &self,
        domain: &CacheDomain,
        observed_version: i64,
    ) -> Result<VersionFenceReservation> {
        let mut state = self.state.lock();
        let current_version = state.versions.get(domain).copied().unwrap_or(0);
        if current_version > observed_version {
            return Err(Error::OptimisticLockConflict);
        }

        if let Some(pending) = state.pending.get(domain) {
            if observed_version < current_version || observed_version != pending.version {
                return Err(Error::OptimisticLockConflict);
            }

            // The caller's DB snapshot has already observed the pending
            // version, so that earlier reservation must have committed even if
            // its fence commit was lost. Finalize it before installing a new
            // pending reservation; otherwise a later abort could erase the only
            // evidence of the committed write.
            state.versions.insert(domain.clone(), observed_version);
            state.pending.remove(domain);
        } else if observed_version > current_version {
            state.versions.insert(domain.clone(), observed_version);
        }

        let reservation = VersionFenceReservation {
            version: observed_version + 1,
            token: Uuid::new_v4().to_string(),
            started_at_ms: Some(crate::SystemClock.now_millis()),
        };
        state.pending.insert(domain.clone(), reservation.clone());
        Ok(reservation)
    }

    async fn commit_write(
        &self,
        domain: &CacheDomain,
        reservation: &VersionFenceReservation,
    ) -> Result<i64> {
        let mut state = self.state.lock();
        match state.pending.get(domain) {
            Some(current)
                if current == reservation
                    || (reservation.token.is_empty() && current.version == reservation.version) =>
            {
                state.pending.remove(domain);
            }
            Some(_) => return Err(Error::OptimisticLockConflict),
            None => {}
        }

        let current = state.versions.entry(domain.clone()).or_insert(0);
        if reservation.version > *current {
            *current = reservation.version;
        }
        let committed = *current;
        if state
            .pending
            .get(domain)
            .is_some_and(|pending| pending.version <= committed)
        {
            state.pending.remove(domain);
        }
        Ok(committed)
    }

    async fn abort_write(
        &self,
        domain: &CacheDomain,
        reservation: &VersionFenceReservation,
    ) -> Result<()> {
        let mut state = self.state.lock();
        if state
            .pending
            .get(domain)
            .is_some_and(|current| current == reservation)
        {
            state.pending.remove(domain);
        }
        Ok(())
    }

    async fn pending_domains(&self) -> Result<Vec<CacheDomain>> {
        Ok(self.state.lock().pending.keys().cloned().collect())
    }

    fn is_authoritative(&self) -> bool {
        true
    }
}

#[derive(Clone)]
pub struct RedisVersionFenceStore {
    runtime: Arc<dyn RedisConnectionRuntime>,
    key_prefix: String,
}

impl fmt::Debug for RedisVersionFenceStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RedisVersionFenceStore")
            .field("key_prefix", &self.key_prefix)
            .finish_non_exhaustive()
    }
}

impl RedisVersionFenceStore {
    #[must_use]
    pub fn new(runtime: Arc<dyn RedisConnectionRuntime>, key_prefix: impl Into<String>) -> Self {
        Self {
            runtime,
            key_prefix: key_prefix.into(),
        }
    }

    fn key(&self, domain: &CacheDomain) -> String {
        format!(
            "{}cache:fence:{}",
            self.key_prefix,
            domain.fence_key_suffix()
        )
    }

    fn pending_key_pattern(&self) -> String {
        format!("{}cache:fence:*:pending", self.key_prefix)
    }

    fn domain_from_pending_key(&self, key: &str) -> Option<CacheDomain> {
        let prefix = format!("{}cache:fence:", self.key_prefix);
        let suffix = key.strip_prefix(&prefix)?.strip_suffix(":pending")?;
        CacheDomain::from_fence_key_suffix(suffix)
    }

    async fn conn(&self, operation: impl Into<String>) -> Result<redis::aio::ConnectionManager> {
        redis_runtime_snapshot(&*self.runtime, operation).await
    }

    fn timeout(&self) -> Duration {
        self.runtime.operation_timeout()
    }

    fn parse_required_state_i64(
        value: Option<&String>,
        field: &'static str,
        domain: &CacheDomain,
    ) -> Result<i64> {
        let Some(value) = value else {
            return Err(Error::Internal(format!(
                "Invalid cache version fence state for {domain}: missing {field}"
            )));
        };
        value.parse::<i64>().map_err(|error| {
            Error::Internal(format!(
                "Invalid cache version fence state for {domain}: field {field} contains {value:?}: {error}"
            ))
        })
    }

    fn parse_optional_state_i64(
        value: Option<&String>,
        field: &'static str,
        domain: &CacheDomain,
    ) -> Result<Option<i64>> {
        let Some(value) = value.filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        value.parse::<i64>().map(Some).map_err(|error| {
            Error::Internal(format!(
                "Invalid cache version fence state for {domain}: field {field} contains {value:?}: {error}"
            ))
        })
    }
}

#[async_trait]
impl VersionFenceStore for RedisVersionFenceStore {
    async fn current_version(&self, domain: &CacheDomain) -> Result<Option<i64>> {
        let key = self.key(domain);
        let mut conn = self.conn("get cache version fence").await?;
        tokio::time::timeout(self.timeout(), conn.get::<_, Option<i64>>(key))
            .await
            .map_err(|_| Error::Timeout("Redis timeout: get cache version fence".to_string()))?
            .map_err(Error::from)
    }

    async fn current_state(&self, domain: &CacheDomain) -> Result<Option<VersionFenceState>> {
        static CURRENT_STATE: std::sync::LazyLock<redis::Script> = std::sync::LazyLock::new(|| {
            redis::Script::new(
                r"
                    local committed_raw = redis.call('GET', KEYS[1])
                    local pending_version_raw = redis.call('HGET', KEYS[2], 'version')
                    local pending_token = redis.call('HGET', KEYS[2], 'token')
                    local pending_started_raw = redis.call('HGET', KEYS[2], 'started_at_ms')

                    if committed_raw == false and pending_version_raw == false then
                        return {}
                    end

                    return {
                        committed_raw or '0',
                        pending_version_raw or '',
                        pending_token or '',
                        pending_started_raw or ''
                    }
                    ",
            )
        });

        let key = self.key(domain);
        let pending_key = format!("{key}:pending");
        let mut conn = self.conn("get cache version fence state").await?;
        let values = tokio::time::timeout(
            self.timeout(),
            CURRENT_STATE
                .key(key)
                .key(pending_key)
                .invoke_async::<Vec<String>>(&mut conn),
        )
        .await
        .map_err(|_| Error::Timeout("Redis timeout: get cache version fence state".to_string()))?
        .map_err(Error::from)?;

        if values.is_empty() {
            return Ok(None);
        }

        let committed_version =
            Self::parse_required_state_i64(values.first(), "committed_version", domain)?;
        let pending_version =
            Self::parse_optional_state_i64(values.get(1), "pending_version", domain)?;
        let pending_token = values.get(2).filter(|value| !value.is_empty()).cloned();
        let pending_started_at_ms =
            Self::parse_optional_state_i64(values.get(3), "pending_started_at_ms", domain)?;

        Ok(Some(VersionFenceState {
            committed_version,
            pending_version,
            pending_token,
            pending_started_at_ms,
        }))
    }

    async fn current_versions(&self, domains: &[CacheDomain]) -> Result<Vec<Option<i64>>> {
        let mut versions = Vec::with_capacity(domains.len());
        for domain in domains {
            let state = self.current_state(domain).await?;
            versions.push(
                if state.as_ref().is_some_and(VersionFenceState::has_pending) {
                    None
                } else {
                    state.map(|state| state.committed_version)
                },
            );
        }
        Ok(versions)
    }

    async fn bump_version(&self, domain: &CacheDomain) -> Result<i64> {
        let key = self.key(domain);
        let mut conn = self.conn("bump cache version fence").await?;
        tokio::time::timeout(self.timeout(), conn.incr::<_, _, i64>(key, 1_i64))
            .await
            .map_err(|_| Error::Timeout("Redis timeout: bump cache version fence".to_string()))?
            .map_err(Error::from)
    }

    async fn set_version_at_least(&self, domain: &CacheDomain, version: i64) -> Result<i64> {
        static SET_AT_LEAST: std::sync::LazyLock<redis::Script> = std::sync::LazyLock::new(|| {
            redis::Script::new(
                r"
                    local raw_current = redis.call('GET', KEYS[1])
                    local current = tonumber(raw_current or '0')
                    local requested = tonumber(ARGV[1])
                    if raw_current == false or requested > current then
                        redis.call('SET', KEYS[1], requested)
                        current = requested
                    end
                    local pending_version_raw = redis.call('HGET', KEYS[2], 'version')
                    local pending_version = tonumber(pending_version_raw or '')
                    if pending_version and pending_version <= current then
                        redis.call('DEL', KEYS[2])
                    end
                    return current
                    ",
            )
        });

        let key = self.key(domain);
        let pending_key = format!("{key}:pending");
        let mut conn = self.conn("set cache version fence").await?;
        tokio::time::timeout(
            self.timeout(),
            SET_AT_LEAST
                .key(key)
                .key(pending_key)
                .arg(version)
                .invoke_async(&mut conn),
        )
        .await
        .map_err(|_| Error::Timeout("Redis timeout: set cache version fence".to_string()))?
        .map_err(Error::from)
    }

    async fn reserve_next_after_observed_version(
        &self,
        domain: &CacheDomain,
        observed_version: i64,
    ) -> Result<i64> {
        static RESERVE_NEXT: std::sync::LazyLock<redis::Script> = std::sync::LazyLock::new(|| {
            redis::Script::new(
                r"
                    local raw_current = redis.call('GET', KEYS[1])
                    local current = tonumber(raw_current or '0')
                    local observed = tonumber(ARGV[1])
                    if not observed then
                        return redis.error_reply('invalid observed version')
                    end
                    if current > observed then
                        return -1
                    end
                    local reserved = observed + 1
                    redis.call('SET', KEYS[1], reserved)
                    return reserved
                    ",
            )
        });

        let key = self.key(domain);
        let mut conn = self.conn("reserve next cache version fence").await?;
        let reserved = tokio::time::timeout(
            self.timeout(),
            RESERVE_NEXT
                .key(key)
                .arg(observed_version)
                .invoke_async::<i64>(&mut conn),
        )
        .await
        .map_err(|_| Error::Timeout("Redis timeout: reserve next cache version fence".to_string()))?
        .map_err(Error::from)?;

        if reserved < 0 {
            return Err(Error::OptimisticLockConflict);
        }

        Ok(reserved)
    }

    async fn begin_write(
        &self,
        domain: &CacheDomain,
        observed_version: i64,
    ) -> Result<VersionFenceReservation> {
        static BEGIN_WRITE: std::sync::LazyLock<redis::Script> = std::sync::LazyLock::new(|| {
            redis::Script::new(
                r"
                    local raw_committed = redis.call('GET', KEYS[1])
                    local committed = tonumber(raw_committed or '0')
                    local pending_raw = redis.call('HGET', KEYS[2], 'version')
                    local pending = tonumber(pending_raw or '')
                    local observed = tonumber(ARGV[1])
                    local token = ARGV[2]
                    local started_at_ms = ARGV[3]
                    if not observed then
                        return redis.error_reply('invalid observed version')
                    end
                    if committed > observed then
                        return -1
                    end
                    if pending then
                        if observed < committed or observed ~= pending then
                            return -1
                        end
                        redis.call('SET', KEYS[1], observed)
                        redis.call('DEL', KEYS[2])
                        committed = observed
                    elseif observed > committed then
                        redis.call('SET', KEYS[1], observed)
                        committed = observed
                    end
                    local reserved = observed + 1
                    redis.call('HSET', KEYS[2],
                        'version', reserved,
                        'token', token,
                        'started_at_ms', started_at_ms)
                    return reserved
                    ",
            )
        });

        let key = self.key(domain);
        let pending_key = format!("{key}:pending");
        let token = Uuid::new_v4().to_string();
        let started_at_ms = crate::SystemClock.now_millis();
        let mut conn = self.conn("begin cache version fence write").await?;
        let reserved = tokio::time::timeout(
            self.timeout(),
            BEGIN_WRITE
                .key(key)
                .key(pending_key)
                .arg(observed_version)
                .arg(&token)
                .arg(started_at_ms)
                .invoke_async::<i64>(&mut conn),
        )
        .await
        .map_err(|_| Error::Timeout("Redis timeout: begin cache version fence write".to_string()))?
        .map_err(Error::from)?;

        if reserved < 0 {
            return Err(Error::OptimisticLockConflict);
        }

        Ok(VersionFenceReservation {
            version: reserved,
            token,
            started_at_ms: Some(started_at_ms),
        })
    }

    async fn commit_write(
        &self,
        domain: &CacheDomain,
        reservation: &VersionFenceReservation,
    ) -> Result<i64> {
        static COMMIT_WRITE: std::sync::LazyLock<redis::Script> = std::sync::LazyLock::new(|| {
            redis::Script::new(
                r"
                    local pending_version_raw = redis.call('HGET', KEYS[2], 'version')
                    local pending_token = redis.call('HGET', KEYS[2], 'token')
                    local requested_version = tonumber(ARGV[1])
                    local requested_token = ARGV[2]

                    if not requested_version then
                        return redis.error_reply('invalid requested version')
                    end

                    if pending_version_raw ~= false and requested_token ~= '' then
                        local pending_version = tonumber(pending_version_raw)
                        if pending_version ~= requested_version or pending_token ~= requested_token then
                            return -1
                        end
                    end

                    local raw_committed = redis.call('GET', KEYS[1])
                    local committed = tonumber(raw_committed or '0')
                    if requested_version > committed then
                        redis.call('SET', KEYS[1], requested_version)
                        committed = requested_version
                    end
                    local pending_after_raw = redis.call('HGET', KEYS[2], 'version')
                    local pending_after = tonumber(pending_after_raw or '')
                    if pending_after and pending_after <= committed then
                        redis.call('DEL', KEYS[2])
                    end
                    return committed
                    ",
            )
        });

        let key = self.key(domain);
        let pending_key = format!("{key}:pending");
        let mut conn = self.conn("commit cache version fence write").await?;
        let committed = tokio::time::timeout(
            self.timeout(),
            COMMIT_WRITE
                .key(key)
                .key(pending_key)
                .arg(reservation.version)
                .arg(&reservation.token)
                .invoke_async::<i64>(&mut conn),
        )
        .await
        .map_err(|_| Error::Timeout("Redis timeout: commit cache version fence write".to_string()))?
        .map_err(Error::from)?;

        if committed < 0 {
            return Err(Error::OptimisticLockConflict);
        }

        Ok(committed)
    }

    async fn abort_write(
        &self,
        domain: &CacheDomain,
        reservation: &VersionFenceReservation,
    ) -> Result<()> {
        static ABORT_WRITE: std::sync::LazyLock<redis::Script> = std::sync::LazyLock::new(|| {
            redis::Script::new(
                r"
                    local pending_version_raw = redis.call('HGET', KEYS[1], 'version')
                    local pending_token = redis.call('HGET', KEYS[1], 'token')
                    local requested_version = tonumber(ARGV[1])
                    local requested_token = ARGV[2]

                    if pending_version_raw == false then
                        return 0
                    end

                    local pending_version = tonumber(pending_version_raw)
                    if pending_version == requested_version and pending_token == requested_token then
                        redis.call('DEL', KEYS[1])
                        return 1
                    end

                    return 0
                    ",
            )
        });

        let key = self.key(domain);
        let pending_key = format!("{key}:pending");
        let mut conn = self.conn("abort cache version fence write").await?;
        tokio::time::timeout(
            self.timeout(),
            ABORT_WRITE
                .key(pending_key)
                .arg(reservation.version)
                .arg(&reservation.token)
                .invoke_async::<i64>(&mut conn),
        )
        .await
        .map_err(|_| Error::Timeout("Redis timeout: abort cache version fence write".to_string()))?
        .map_err(Error::from)?;

        Ok(())
    }

    async fn pending_domains(&self) -> Result<Vec<CacheDomain>> {
        let mut conn = self.conn("scan pending cache version fences").await?;
        let pattern = self.pending_key_pattern();
        let mut cursor = 0_u64;
        let mut domains = Vec::new();
        loop {
            let (next_cursor, keys): (u64, Vec<String>) = tokio::time::timeout(
                self.timeout(),
                redis::cmd("SCAN")
                    .cursor_arg(cursor)
                    .arg("MATCH")
                    .arg(&pattern)
                    .arg("COUNT")
                    .arg(128)
                    .query_async(&mut conn),
            )
            .await
            .map_err(|_| {
                Error::Timeout("Redis timeout: scan pending cache version fences".to_string())
            })?
            .map_err(Error::from)?;

            for key in keys {
                if let Some(domain) = self.domain_from_pending_key(&key) {
                    domains.push(domain);
                } else {
                    tracing::warn!(
                        key = %key,
                        "Ignoring malformed cache version fence pending key during repair scan"
                    );
                }
            }
            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }
        Ok(domains)
    }

    fn is_authoritative(&self) -> bool {
        true
    }
}

#[derive(Clone)]
pub struct ConsistencyCoordinator {
    fence_store: Arc<dyn VersionFenceStore>,
}

impl fmt::Debug for ConsistencyCoordinator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConsistencyCoordinator")
            .field("authoritative", &self.is_authoritative())
            .finish()
    }
}

impl ConsistencyCoordinator {
    const DEFAULT_PENDING_LEASE: Duration = Duration::from_mins(2);

    #[must_use]
    pub fn new(fence_store: Arc<dyn VersionFenceStore>) -> Self {
        Self { fence_store }
    }

    #[must_use]
    pub fn is_authoritative(&self) -> bool {
        self.fence_store.is_authoritative()
    }

    pub async fn current_state(&self, domain: &CacheDomain) -> Result<Option<VersionFenceState>> {
        let result = self.fence_store.current_state(domain).await;
        Self::record_result(domain, "current_state", &result);
        if let Ok(Some(state)) = &result {
            Self::record_pending_gauge(domain, state.has_pending());
        }
        result
    }

    pub async fn current_committed_version(&self, domain: &CacheDomain) -> Result<Option<i64>> {
        let state = self.current_state(domain).await?;
        if state.as_ref().is_some_and(VersionFenceState::has_pending) {
            Self::record_db_fallback(domain, "pending_fence");
            return Ok(None);
        }
        Ok(state.map(|state| state.committed_version))
    }

    #[must_use]
    pub fn fence_key(&self, domain: &CacheDomain) -> Option<String> {
        self.fence_store.fence_key(domain)
    }

    pub async fn current_versions(&self, domains: &[CacheDomain]) -> Result<Vec<Option<i64>>> {
        let mut versions = Vec::with_capacity(domains.len());
        for domain in domains {
            let state = self.current_state(domain).await?;
            if state.as_ref().is_some_and(VersionFenceState::has_pending) {
                Self::record_db_fallback(domain, "pending_fence");
                versions.push(None);
            } else {
                versions.push(state.map(|state| state.committed_version));
            }
        }
        Ok(versions)
    }

    pub async fn reserve_next_after_observed_version(
        &self,
        domain: &CacheDomain,
        observed_version: i64,
    ) -> Result<i64> {
        if !self.is_authoritative() {
            Self::record_success(domain, "reserve_bypass");
            return Ok(0);
        }

        let result = self.fence_store.begin_write(domain, observed_version).await;
        Self::record_result(domain, "begin_write", &result);
        result.map(|reservation| reservation.version)
    }

    pub async fn begin_observed_write(
        &self,
        domain: &CacheDomain,
        observed_version: i64,
    ) -> Result<Option<VersionFenceReservation>> {
        if !self.is_authoritative() {
            Self::record_success(domain, "begin_bypass");
            return Ok(None);
        }

        let result = self.fence_store.begin_write(domain, observed_version).await;
        Self::record_result(domain, "begin_write", &result);
        result.map(Some)
    }

    pub async fn commit_observed_write(&self, domain: &CacheDomain, version: i64) -> Result<i64> {
        if !self.is_authoritative() {
            Self::record_success(domain, "commit_bypass");
            return Ok(version);
        }

        let reservation = VersionFenceReservation {
            version,
            token: String::new(),
            started_at_ms: None,
        };
        let result = self.fence_store.commit_write(domain, &reservation).await;
        Self::record_result(domain, "commit_write", &result);
        result
    }

    pub async fn commit_reserved_write(
        &self,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
        version: i64,
    ) -> Result<i64> {
        if !self.is_authoritative() {
            Self::record_success(domain, "commit_bypass");
            return Ok(version);
        }

        let fallback;
        let reservation = if let Some(reservation) = reservation {
            reservation
        } else {
            fallback = VersionFenceReservation {
                version,
                token: String::new(),
                started_at_ms: None,
            };
            &fallback
        };
        let result = self.fence_store.commit_write(domain, reservation).await;
        Self::record_result(domain, "commit_write", &result);
        match result {
            Ok(version) => Ok(version),
            Err(error) => {
                let repair = self.fence_store.set_version_at_least(domain, version).await;
                Self::record_result(domain, "commit_repair", &repair);
                match repair {
                    Ok(committed) => {
                        tracing::warn!(
                            domain = %domain,
                            version,
                            error = %error,
                            "Recovered cache version fence after commit finalization failed"
                        );
                        Ok(committed)
                    }
                    Err(repair_error) => {
                        tracing::warn!(
                            domain = %domain,
                            version,
                            commit_error = %error,
                            repair_error = %repair_error,
                            "Failed to repair cache version fence after commit finalization failed"
                        );
                        self.abort_committed_reservation_if_safe(domain, reservation, version)
                            .await;
                        Err(error)
                    }
                }
            }
        }
    }

    async fn abort_committed_reservation_if_safe(
        &self,
        domain: &CacheDomain,
        reservation: &VersionFenceReservation,
        version: i64,
    ) {
        match self.fence_store.current_state(domain).await {
            Ok(Some(state)) if state.committed_version >= version => {
                let abort = self.fence_store.abort_write(domain, reservation).await;
                Self::record_result(domain, "commit_repair_abort", &abort);
                if let Err(error) = abort {
                    tracing::warn!(
                        domain = %domain,
                        version,
                        error = %error,
                        "Failed to clear already-committed cache version fence reservation"
                    );
                }
            }
            Ok(Some(state)) => {
                tracing::warn!(
                    domain = %domain,
                    version,
                    committed_version = state.committed_version,
                    pending_version = ?state.pending_version,
                    "Leaving pending cache version fence in place because committed fence did not reach the DB version"
                );
            }
            Ok(None) => {
                tracing::warn!(
                    domain = %domain,
                    version,
                    "Leaving cache version fence unresolved because no committed fence state is visible"
                );
            }
            Err(error) => {
                tracing::warn!(
                    domain = %domain,
                    version,
                    error = %error,
                    "Failed to inspect cache version fence state after commit finalization failure"
                );
            }
        }
    }

    pub async fn abort_reserved_write(
        &self,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
    ) {
        if !self.is_authoritative() {
            return;
        }
        let Some(reservation) = reservation else {
            return;
        };

        let result = self.fence_store.abort_write(domain, reservation).await;
        Self::record_result(domain, "abort_write", &result);
        if let Err(error) = result {
            tracing::warn!(
                domain = %domain,
                version = reservation.version,
                error = %error,
                "Failed to abort pending cache version fence write"
            );
        }
    }

    pub async fn repair_after_db_read(&self, domain: &CacheDomain, db_version: i64) {
        self.repair_after_db_read_with_pending_lease(
            domain,
            db_version,
            Self::DEFAULT_PENDING_LEASE,
        )
        .await;
    }

    pub async fn repair_after_db_read_with_pending_lease(
        &self,
        domain: &CacheDomain,
        db_version: i64,
        pending_lease: Duration,
    ) {
        if !self.is_authoritative() {
            return;
        }

        match self.current_state(domain).await {
            Ok(Some(state)) => {
                Self::record_db_compare(domain, Self::db_compare_relation(&state, db_version));
                match Self::repair_decision(&state, db_version, pending_lease) {
                    FenceRepairDecision::FinalizePending => {
                        match self.set_version_at_least(domain, db_version).await {
                            Ok(_) => Self::record_repair(domain, "finalized_pending"),
                            Err(error) => Self::record_repair_error(domain, &error),
                        }
                    }
                    FenceRepairDecision::AdvanceCommitted => {
                        match self.set_version_at_least(domain, db_version).await {
                            Ok(_) => Self::record_repair(domain, "advanced_committed"),
                            Err(error) => Self::record_repair_error(domain, &error),
                        }
                    }
                    FenceRepairDecision::KeepPending => {
                        Self::record_repair(domain, "pending_still_ahead");
                    }
                    FenceRepairDecision::ExpirePending => {
                        match self
                            .expire_pending_reservation(domain, &state, db_version)
                            .await
                        {
                            Ok(()) => Self::record_repair(domain, "expired_pending"),
                            Err(error) => Self::record_repair_error(domain, &error),
                        }
                    }
                    FenceRepairDecision::Noop => {}
                }
            }
            Err(error) => Self::record_repair_error(domain, &error),
            Ok(None) => Self::record_db_compare(domain, "missing_fence"),
        }
    }

    async fn expire_pending_reservation(
        &self,
        domain: &CacheDomain,
        state: &VersionFenceState,
        db_version: i64,
    ) -> Result<()> {
        let (Some(version), Some(token)) = (state.pending_version, state.pending_token.clone())
        else {
            return Ok(());
        };

        let reservation = VersionFenceReservation {
            version,
            token,
            started_at_ms: state.pending_started_at_ms,
        };
        let result = self.fence_store.abort_write(domain, &reservation).await;
        Self::record_result(domain, "expire_pending", &result);
        result?;

        let result = self
            .fence_store
            .set_version_at_least(domain, db_version)
            .await;
        Self::record_result(domain, "expire_pending_seed", &result);
        result.map(|_| ())
    }

    fn db_compare_relation(state: &VersionFenceState, db_version: i64) -> &'static str {
        if let Some(pending) = state.pending_version {
            if pending > db_version {
                return "pending_ahead_db";
            }
            if pending <= db_version {
                return "pending_observed_in_db";
            }
        }

        match state.committed_version.cmp(&db_version) {
            std::cmp::Ordering::Less => "fence_behind_db",
            std::cmp::Ordering::Equal => "fence_equal_db",
            std::cmp::Ordering::Greater => "fence_ahead_db",
        }
    }

    fn repair_decision(
        state: &VersionFenceState,
        db_version: i64,
        pending_lease: Duration,
    ) -> FenceRepairDecision {
        if let Some(pending) = state.pending_version {
            if pending <= db_version {
                return FenceRepairDecision::FinalizePending;
            }
            if Self::pending_lease_expired(state, pending_lease) {
                return FenceRepairDecision::ExpirePending;
            }
            return FenceRepairDecision::KeepPending;
        }

        if state.committed_version < db_version {
            return FenceRepairDecision::AdvanceCommitted;
        }

        FenceRepairDecision::Noop
    }

    fn pending_lease_expired(state: &VersionFenceState, pending_lease: Duration) -> bool {
        let Some(started_at_ms) = state.pending_started_at_ms else {
            return false;
        };
        let now_ms = crate::SystemClock.now_millis();
        let elapsed_ms = now_ms.saturating_sub(started_at_ms);
        let Ok(pending_lease_ms) = i64::try_from(pending_lease.as_millis()) else {
            return true;
        };
        elapsed_ms >= pending_lease_ms
    }
    pub async fn repair_pending_domains<F, Fut>(&self, mut db_version_for_domain: F)
    where
        F: FnMut(CacheDomain) -> Fut,
        Fut: std::future::Future<Output = Option<i64>>,
    {
        if !self.is_authoritative() {
            return;
        }

        let domains = match self.fence_store.pending_domains().await {
            Ok(domains) => domains,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "Failed to scan pending cache version fences for repair"
                );
                return;
            }
        };

        for domain in domains {
            let Some(db_version) = db_version_for_domain(domain.clone()).await else {
                Self::record_repair(&domain, "unsupported_domain");
                continue;
            };
            self.repair_after_db_read(&domain, db_version).await;
        }
    }

    pub fn spawn_repair_worker(
        self,
        pool: PgPool,
        interval: Duration,
        cancel: CancellationToken,
    ) -> JoinHandle<()> {
        crate::spawn::spawn_monitored("cache_fence_repair_worker", async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        self.repair_pending_domains(|domain| {
                            let pool = pool.clone();
                            async move { db_version_for_repair(&pool, &domain).await }
                        }).await;
                    }
                }
            }
        })
    }

    pub async fn set_version_at_least(&self, domain: &CacheDomain, version: i64) -> Result<i64> {
        if !self.is_authoritative() {
            Self::record_success(domain, "seed_bypass");
            return Ok(version);
        }

        let result = self.fence_store.set_version_at_least(domain, version).await;
        Self::record_result(domain, "seed", &result);
        result
    }

    pub async fn bump_version(&self, domain: &CacheDomain) -> Result<i64> {
        if !self.is_authoritative() {
            Self::record_success(domain, "bump_bypass");
            return Ok(0);
        }

        let result = self.fence_store.bump_version(domain).await;
        Self::record_result(domain, "bump", &result);
        result
    }

    pub fn record_db_fallback(domain: &CacheDomain, reason: &'static str) {
        crate::metrics::cache::CACHE_DB_FALLBACK_TOTAL
            .with_label_values(&[cache_domain_metric_label(domain), reason])
            .inc();
    }

    fn record_result<T>(domain: &CacheDomain, operation: &'static str, result: &Result<T>) {
        match result {
            Ok(_) => Self::record_success(domain, operation),
            Err(error) => Self::record_error(domain, operation, error),
        }
    }

    fn record_success(domain: &CacheDomain, operation: &'static str) {
        crate::metrics::cache::CACHE_FENCE_OPERATIONS_TOTAL
            .with_label_values(&[cache_domain_metric_label(domain), operation, "ok"])
            .inc();
    }

    fn record_error(domain: &CacheDomain, operation: &'static str, error: &Error) {
        let result = match error {
            Error::OptimisticLockConflict => "conflict",
            Error::Timeout(_) => "timeout",
            _ => "error",
        };
        crate::metrics::cache::CACHE_FENCE_OPERATIONS_TOTAL
            .with_label_values(&[cache_domain_metric_label(domain), operation, result])
            .inc();
    }

    fn record_pending_gauge(domain: &CacheDomain, pending: bool) {
        crate::metrics::cache::CACHE_FENCE_PENDING
            .with_label_values(&[cache_domain_metric_label(domain)])
            .set(if pending { 1.0 } else { 0.0 });
    }

    fn record_repair(domain: &CacheDomain, result: &'static str) {
        crate::metrics::cache::CACHE_FENCE_REPAIR_TOTAL
            .with_label_values(&[cache_domain_metric_label(domain), result])
            .inc();
    }

    fn record_db_compare(domain: &CacheDomain, relation: &'static str) {
        crate::metrics::cache::CACHE_FENCE_DB_COMPARE
            .with_label_values(&[cache_domain_metric_label(domain), relation])
            .set(1.0);
    }

    fn record_repair_error(domain: &CacheDomain, error: &Error) {
        let result = match error {
            Error::OptimisticLockConflict => "conflict",
            Error::Timeout(_) => "timeout",
            _ => "error",
        };
        Self::record_repair(domain, result);
    }
}

pub fn version_fence_store_from_shared_state_profile(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn VersionFenceStore>> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            let runtime = profile.require_shared_runtime("cache version fences")?;
            Ok(Arc::new(RedisVersionFenceStore::new(
                runtime,
                profile.key_prefix(),
            )))
        }
        SharedStateMode::SharedBestEffort => {
            if let Some(runtime) = profile.shared_runtime() {
                Ok(Arc::new(RedisVersionFenceStore::new(
                    runtime,
                    profile.key_prefix(),
                )))
            } else {
                Ok(Arc::new(LocalVersionFenceStore::new()))
            }
        }
        SharedStateMode::LocalOnly => Ok(Arc::new(LocalVersionFenceStore::new())),
    }
}

async fn db_version_for_repair(pool: &PgPool, domain: &CacheDomain) -> Option<i64> {
    match domain {
        CacheDomain::RoomSettings { room_id } => sqlx::query_scalar!(
            "SELECT version FROM room_settings WHERE room_id = $1",
            room_id as &RoomId,
        )
        .fetch_optional(pool)
        .await
        .map(|version| version.unwrap_or(0))
        .map_err(|error| {
            tracing::warn!(
                room_id = %room_id,
                error = %error,
                "Failed to read room settings version for cache fence repair"
            );
        })
        .ok(),
        CacheDomain::Playback { room_id } => sqlx::query_scalar!(
            "SELECT version FROM room_playback_state WHERE room_id = $1",
            room_id as &RoomId,
        )
        .fetch_optional(pool)
        .await
        .map(|version| version.unwrap_or(0))
        .map_err(|error| {
            tracing::warn!(
                room_id = %room_id,
                error = %error,
                "Failed to read playback version for cache fence repair"
            );
        })
        .ok(),
        CacheDomain::Permission { room_id, user_id }
        | CacheDomain::RoomMembership { room_id, user_id } => sqlx::query_scalar!(
            r#"SELECT COALESCE(
                 (SELECT version FROM room_members WHERE room_id = $1 AND user_id = $2),
                 (SELECT version FROM room_member_versions
                  WHERE room_id = $1 AND user_id = $2 AND is_member = FALSE),
                 0
             ) as "version!""#,
            room_id as &RoomId,
            user_id as &UserId,
        )
        .fetch_optional(pool)
        .await
        .map(|version| version.unwrap_or(0))
        .map_err(|error| {
            tracing::warn!(
                room_id = %room_id,
                user_id = %user_id,
                error = %error,
                "Failed to read room member version for cache fence repair"
            );
        })
        .ok(),
        CacheDomain::MediaResource { .. }
        | CacheDomain::Playlist { .. }
        | CacheDomain::UserAuthSecurity { .. } => None,
        CacheDomain::RuntimeSetting { key } => {
            sqlx::query_scalar!("SELECT version FROM settings WHERE key = $1", key,)
                .fetch_optional(pool)
                .await
                .map(|version| version.map_or(0, i64::from))
                .map_err(|error| {
                    tracing::warn!(
                        key = %key,
                        error = %error,
                        "Failed to read runtime setting version for cache fence repair"
                    );
                })
                .ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{TestOptionExt, TestResultExt, failing_redis_runtime};

    #[derive(Debug, Default)]
    struct CommitFailureRepairStore {
        state: LocalVersionFenceStore,
    }

    #[async_trait]
    impl VersionFenceStore for CommitFailureRepairStore {
        async fn current_version(&self, domain: &CacheDomain) -> Result<Option<i64>> {
            self.state.current_version(domain).await
        }

        async fn current_state(&self, domain: &CacheDomain) -> Result<Option<VersionFenceState>> {
            self.state.current_state(domain).await
        }

        async fn current_versions(&self, domains: &[CacheDomain]) -> Result<Vec<Option<i64>>> {
            self.state.current_versions(domains).await
        }

        async fn bump_version(&self, domain: &CacheDomain) -> Result<i64> {
            self.state.bump_version(domain).await
        }

        async fn set_version_at_least(&self, domain: &CacheDomain, version: i64) -> Result<i64> {
            self.state.set_version_at_least(domain, version).await
        }

        async fn reserve_next_after_observed_version(
            &self,
            domain: &CacheDomain,
            observed_version: i64,
        ) -> Result<i64> {
            self.state
                .reserve_next_after_observed_version(domain, observed_version)
                .await
        }

        async fn begin_write(
            &self,
            domain: &CacheDomain,
            observed_version: i64,
        ) -> Result<VersionFenceReservation> {
            self.state.begin_write(domain, observed_version).await
        }

        async fn commit_write(
            &self,
            _domain: &CacheDomain,
            _reservation: &VersionFenceReservation,
        ) -> Result<i64> {
            Err(Error::Timeout(
                "injected cache fence commit failure".to_string(),
            ))
        }

        async fn abort_write(
            &self,
            domain: &CacheDomain,
            reservation: &VersionFenceReservation,
        ) -> Result<()> {
            self.state.abort_write(domain, reservation).await
        }

        fn is_authoritative(&self) -> bool {
            true
        }
    }

    fn room_settings_domain(room_id: i64) -> CacheDomain {
        CacheDomain::RoomSettings {
            room_id: RoomId::expect_positive(room_id),
        }
    }

    #[tokio::test]
    async fn local_version_fence_is_authoritative_and_monotonic() {
        let store = LocalVersionFenceStore::new();
        let domain = room_settings_domain(1);

        assert!(store.is_authoritative());
        assert_eq!(
            store
                .current_version(&domain)
                .await
                .checked("operation should succeed"),
            None
        );
        assert_eq!(
            store
                .set_version_at_least(&domain, 5)
                .await
                .checked("operation should succeed"),
            5
        );
        assert_eq!(
            store
                .set_version_at_least(&domain, 3)
                .await
                .checked("operation should succeed"),
            5
        );
        assert_eq!(
            store
                .bump_version(&domain)
                .await
                .checked("operation should succeed"),
            6
        );
        assert_eq!(
            store
                .current_version(&domain)
                .await
                .checked("operation should succeed"),
            Some(6)
        );
    }

    #[tokio::test]
    async fn local_version_fence_reservation_rejects_stale_observed_version_without_advancing() {
        let store = LocalVersionFenceStore::new();
        let domain = room_settings_domain(1);

        assert_eq!(
            store
                .reserve_next_after_observed_version(&domain, 1)
                .await
                .checked("operation should succeed"),
            2
        );
        assert!(
            matches!(
                store.reserve_next_after_observed_version(&domain, 1).await,
                Err(Error::OptimisticLockConflict)
            ),
            "a stale DB snapshot must retry instead of burning an unsatisfiable fence version"
        );
        assert_eq!(
            store
                .current_version(&domain)
                .await
                .checked("operation should succeed"),
            Some(2)
        );
    }

    #[tokio::test]
    async fn coordinator_hides_cache_fence_while_local_write_is_pending() {
        let store = Arc::new(LocalVersionFenceStore::new());
        let coordinator = ConsistencyCoordinator::new(store.clone());
        let domain = room_settings_domain(1);

        let reserved = coordinator
            .reserve_next_after_observed_version(&domain, 0)
            .await
            .checked("operation should succeed");
        assert_eq!(reserved, 1);

        let state = coordinator
            .current_state(&domain)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert_eq!(state.committed_version, 0);
        assert_eq!(state.pending_version, Some(1));
        assert_eq!(
            coordinator
                .current_committed_version(&domain)
                .await
                .checked("operation should succeed"),
            None,
            "strong reads must bypass cache while a write has a pending fence"
        );

        coordinator
            .set_version_at_least(&domain, reserved)
            .await
            .checked("operation should succeed");

        let state = coordinator
            .current_state(&domain)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert_eq!(state.committed_version, 1);
        assert_eq!(state.pending_version, None);
        assert_eq!(
            coordinator
                .current_committed_version(&domain)
                .await
                .checked("operation should succeed"),
            Some(1)
        );
    }

    #[tokio::test]
    async fn coordinator_repair_finalizes_pending_when_db_has_reserved_version() {
        let store = Arc::new(LocalVersionFenceStore::new());
        let coordinator = ConsistencyCoordinator::new(store.clone());
        let domain = room_settings_domain(1);

        let reserved = coordinator
            .begin_observed_write(&domain, 0)
            .await
            .checked("operation should succeed")
            .checked("authoritative local fence should reserve");
        assert_eq!(reserved.version, 1);

        coordinator
            .repair_pending_domains(|_| async { Some(1) })
            .await;

        let state = coordinator
            .current_state(&domain)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert_eq!(state.committed_version, 1);
        assert_eq!(state.pending_version, None);
    }

    #[tokio::test]
    async fn coordinator_repairs_pending_when_commit_finalization_fails_after_db_commit() {
        let store = Arc::new(CommitFailureRepairStore::default());
        let coordinator = ConsistencyCoordinator::new(store.clone());
        let domain = room_settings_domain(1);

        let reservation = coordinator
            .begin_observed_write(&domain, 0)
            .await
            .checked("operation should succeed")
            .checked("authoritative fence should reserve");
        let committed = coordinator
            .commit_reserved_write(&domain, Some(&reservation), 1)
            .await
            .checked("coordinator should recover committed DB version after fence commit failure");

        assert_eq!(committed, 1);
        let state = coordinator
            .current_state(&domain)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert_eq!(state.committed_version, 1);
        assert_eq!(
            state.pending_version, None,
            "post-commit recovery must not leave a permanent pending fence"
        );
    }

    #[tokio::test]
    async fn coordinator_repair_keeps_pending_when_db_has_not_committed_reserved_version() {
        let store = Arc::new(LocalVersionFenceStore::new());
        let coordinator = ConsistencyCoordinator::new(store.clone());
        let domain = room_settings_domain(1);

        let reserved = coordinator
            .begin_observed_write(&domain, 0)
            .await
            .checked("operation should succeed")
            .checked("authoritative local fence should reserve");
        assert_eq!(reserved.version, 1);

        coordinator
            .repair_pending_domains(|_| async { Some(0) })
            .await;

        let state = coordinator
            .current_state(&domain)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert_eq!(state.committed_version, 0);
        assert_eq!(state.pending_version, Some(1));
        assert_eq!(
            coordinator
                .current_committed_version(&domain)
                .await
                .checked("operation should succeed"),
            None
        );
    }

    #[tokio::test]
    async fn coordinator_repair_expires_abandoned_pending_when_db_did_not_commit() {
        let store = Arc::new(LocalVersionFenceStore::new());
        let coordinator = ConsistencyCoordinator::new(store.clone());
        let domain = room_settings_domain(1);

        let reserved = coordinator
            .begin_observed_write(&domain, 0)
            .await
            .checked("operation should succeed")
            .checked("authoritative local fence should reserve");
        assert_eq!(reserved.version, 1);

        coordinator
            .repair_after_db_read_with_pending_lease(&domain, 0, Duration::from_millis(0))
            .await;

        let state = coordinator
            .current_state(&domain)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert_eq!(state.committed_version, 0);
        assert_eq!(
            state.pending_version, None,
            "an expired pending reservation ahead of the DB must not permanently block reads and writes"
        );
        assert_eq!(
            coordinator
                .current_committed_version(&domain)
                .await
                .checked("operation should succeed"),
            Some(0)
        );
    }

    #[tokio::test]
    async fn coordinator_repair_keeps_unexpired_pending_when_db_did_not_commit() {
        let store = Arc::new(LocalVersionFenceStore::new());
        let coordinator = ConsistencyCoordinator::new(store.clone());
        let domain = room_settings_domain(1);

        let reserved = coordinator
            .begin_observed_write(&domain, 0)
            .await
            .checked("operation should succeed")
            .checked("authoritative local fence should reserve");
        assert_eq!(reserved.version, 1);

        coordinator
            .repair_after_db_read_with_pending_lease(&domain, 0, Duration::from_mins(1))
            .await;

        let state = coordinator
            .current_state(&domain)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert_eq!(state.committed_version, 0);
        assert_eq!(
            state.pending_version,
            Some(1),
            "an unexpired pending reservation must keep strong reads on the DB fallback path"
        );
    }

    #[tokio::test]
    async fn coordinator_repair_finalizes_committed_pending_even_when_expired() {
        let store = Arc::new(LocalVersionFenceStore::new());
        let coordinator = ConsistencyCoordinator::new(store.clone());
        let domain = room_settings_domain(1);

        let reserved = coordinator
            .begin_observed_write(&domain, 0)
            .await
            .checked("operation should succeed")
            .checked("authoritative local fence should reserve");
        assert_eq!(reserved.version, 1);

        coordinator
            .repair_after_db_read_with_pending_lease(&domain, 1, Duration::from_millis(0))
            .await;

        let state = coordinator
            .current_state(&domain)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert_eq!(state.committed_version, 1);
        assert_eq!(
            state.pending_version, None,
            "DB version reaching pending version must finalize instead of aborting"
        );
    }

    #[tokio::test]
    async fn local_begin_finalizes_observed_pending_before_installing_next_pending() {
        let store = Arc::new(LocalVersionFenceStore::new());
        let coordinator = ConsistencyCoordinator::new(store.clone());
        let domain = room_settings_domain(1);

        let first = coordinator
            .begin_observed_write(&domain, 0)
            .await
            .checked("operation should succeed")
            .checked("authoritative local fence should reserve");
        assert_eq!(first.version, 1);

        let second = coordinator
            .begin_observed_write(&domain, 1)
            .await
            .checked("operation should succeed")
            .checked("observing the first version in DB should allow the next write");
        assert_eq!(second.version, 2);

        coordinator
            .abort_reserved_write(&domain, Some(&second))
            .await;

        let state = coordinator
            .current_state(&domain)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert_eq!(
            state.committed_version, 1,
            "a later aborted write must not erase an earlier DB-observed commit"
        );
        assert_eq!(state.pending_version, None);
    }

    #[tokio::test]
    async fn local_begin_rejects_second_writer_that_did_not_observe_pending_commit() {
        let store = Arc::new(LocalVersionFenceStore::new());
        let coordinator = ConsistencyCoordinator::new(store.clone());
        let domain = room_settings_domain(1);

        let first = coordinator
            .begin_observed_write(&domain, 5)
            .await
            .checked("operation should succeed")
            .checked("first writer should reserve");
        assert_eq!(first.version, 6);

        assert!(
            matches!(
                coordinator.begin_observed_write(&domain, 5).await,
                Err(Error::OptimisticLockConflict)
            ),
            "a second writer on the same DB snapshot must not replace the first pending token"
        );

        let state = coordinator
            .current_state(&domain)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert_eq!(state.committed_version, 5);
        assert_eq!(state.pending_version, Some(6));
        assert_eq!(state.pending_token.as_deref(), Some(first.token.as_str()));
    }

    #[tokio::test]
    async fn local_begin_clears_pending_observed_in_database_before_next_reservation() {
        let store = Arc::new(LocalVersionFenceStore::new());
        let coordinator = ConsistencyCoordinator::new(store.clone());
        let domain = room_settings_domain(1);

        let first = coordinator
            .begin_observed_write(&domain, 5)
            .await
            .checked("operation should succeed")
            .checked("first writer should reserve");
        assert_eq!(first.version, 6);

        let second = coordinator
            .begin_observed_write(&domain, 6)
            .await
            .checked("operation should succeed")
            .checked("writer that observed the first DB commit should reserve next version");
        assert_eq!(second.version, 7);
        assert_ne!(second.token, first.token);

        let state = coordinator
            .current_state(&domain)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert_eq!(state.committed_version, 6);
        assert_eq!(state.pending_version, Some(7));
        assert_eq!(state.pending_token.as_deref(), Some(second.token.as_str()));

        coordinator
            .commit_reserved_write(&domain, Some(&second), second.version)
            .await
            .checked("operation should succeed");
        let state = coordinator
            .current_state(&domain)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert_eq!(state.committed_version, 7);
        assert_eq!(state.pending_version, None);
    }

    #[tokio::test]
    async fn local_begin_accepts_pending_version_observed_in_database() {
        let store = Arc::new(LocalVersionFenceStore::new());
        let coordinator = ConsistencyCoordinator::new(store.clone());
        let domain = room_settings_domain(1);

        let first = coordinator
            .begin_observed_write(&domain, 0)
            .await
            .checked("operation should succeed")
            .checked("first writer should reserve");
        assert_eq!(first.version, 1);

        let second = coordinator
            .begin_observed_write(&domain, 1)
            .await
            .checked("writer that observed pending version in DB should reserve next version")
            .checked("second writer should reserve");
        assert_eq!(second.version, 2);
        assert_ne!(second.token, first.token);

        let state = coordinator
            .current_state(&domain)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert_eq!(state.committed_version, 1);
        assert_eq!(state.pending_version, Some(2));
        assert_eq!(state.pending_token.as_deref(), Some(second.token.as_str()));
    }

    #[tokio::test]
    async fn local_version_fence_mixed_operations_do_not_deadlock() {
        let store = Arc::new(LocalVersionFenceStore::new());
        let domain = room_settings_domain(1);

        tokio::time::timeout(Duration::from_secs(2), async {
            let mut handles = Vec::new();
            for _ in 0..32 {
                let store = store.clone();
                let domain = domain.clone();
                handles.push(tokio::spawn(async move {
                    for _ in 0..16 {
                        let observed = store
                            .current_version(&domain)
                            .await
                            .checked("operation should succeed")
                            .unwrap_or(0);
                        match store.begin_write(&domain, observed).await {
                            Ok(reservation) => {
                                if reservation.version % 2 == 0 {
                                    store
                                        .abort_write(&domain, &reservation)
                                        .await
                                        .checked("operation should succeed");
                                } else {
                                    store
                                        .commit_write(&domain, &reservation)
                                        .await
                                        .checked("operation should succeed");
                                }
                            }
                            Err(Error::OptimisticLockConflict) => {}
                            Err(error) => {
                                std::panic::panic_any(format!("unexpected fence error: {error:?}"))
                            }
                        }
                        store
                            .set_version_at_least(&domain, observed)
                            .await
                            .checked("operation should succeed");
                    }
                }));
            }

            for handle in handles {
                handle.await.checked("operation should succeed");
            }
        })
        .await
        .checked("local fence mixed operations should not deadlock");
    }

    #[tokio::test]
    async fn local_version_fence_reads_versions_in_batch() {
        let store = LocalVersionFenceStore::new();
        let first = room_settings_domain(1);
        let second = room_settings_domain(2);
        store
            .set_version_at_least(&first, 7)
            .await
            .checked("operation should succeed");

        assert_eq!(
            store
                .current_versions(&[first.clone(), second.clone()])
                .await
                .checked("operation should succeed"),
            vec![Some(7), None]
        );
    }

    #[tokio::test]
    async fn local_version_fence_batch_reads_hide_pending_domains() {
        let store = LocalVersionFenceStore::new();
        let first = room_settings_domain(1);
        let second = room_settings_domain(2);
        store
            .set_version_at_least(&first, 7)
            .await
            .checked("operation should succeed");
        let reservation = store
            .begin_write(&first, 7)
            .await
            .checked("operation should succeed");

        assert_eq!(
            store
                .current_versions(&[first.clone(), second.clone()])
                .await
                .checked("operation should succeed"),
            vec![None, None],
            "batch reads must match strong single-domain reads while a fence is pending"
        );

        store
            .commit_write(&first, &reservation)
            .await
            .checked("operation should succeed");
        assert_eq!(
            store
                .current_versions(&[first, second])
                .await
                .checked("operation should succeed"),
            vec![Some(8), None]
        );
    }

    #[test]
    fn redis_fence_store_parses_valid_pending_key() {
        let store = RedisVersionFenceStore::new(failing_redis_runtime(), "test:");
        let key = format!("{}{}", store.key(&room_settings_domain(42)), ":pending");

        assert_eq!(
            store.domain_from_pending_key(&key),
            Some(room_settings_domain(42))
        );
    }

    #[test]
    fn required_profile_without_redis_runtime_rejects_fence_construction() {
        let profile = SharedStateProfile::new(SharedStateMode::SharedRequired, None, "test:");

        let Err(error) = version_fence_store_from_shared_state_profile(&profile) else {
            std::panic::panic_any("required shared-state fences need Redis");
        };

        assert!(
            error
                .to_string()
                .contains("distributed runtime requires shared cache version fences"),
            "unexpected error: {error}"
        );
    }
}
