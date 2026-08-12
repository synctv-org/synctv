use rand::RngExt;
use std::time::Duration;
use synctv_core::models::{RealtimeActor, UserId};

/// Default TTL for membership cache entries (30 seconds).
///
/// This TTL is chosen to balance between:
/// - Reducing database load (longer TTL = fewer queries)
/// - Responsiveness to membership changes (shorter TTL = faster detection of bans/removals)
///
/// With a 30-second TTL and 25-35 second heartbeat interval, we ensure:
/// - At most 1 DB query per connection per 30 seconds (vs. every heartbeat without cache)
/// - Banned/removed users are disconnected within ~30-65 seconds worst case
/// - The disconnect signal channel (Redis `PubSub`) provides immediate notification in most cases
pub const MEMBERSHIP_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeartbeatSchedule {
    membership_cache_ttl: Duration,
    base_interval: Duration,
    max_jitter_secs: u64,
}

impl HeartbeatSchedule {
    #[must_use]
    pub const fn production() -> Self {
        Self {
            membership_cache_ttl: MEMBERSHIP_CACHE_TTL,
            base_interval: Duration::from_secs(25),
            max_jitter_secs: 10,
        }
    }

    #[must_use]
    pub const fn fixed(membership_cache_ttl: Duration, base_interval: Duration) -> Self {
        Self {
            membership_cache_ttl,
            base_interval,
            max_jitter_secs: 0,
        }
    }

    #[must_use]
    pub const fn membership_cache_ttl(self) -> Duration {
        self.membership_cache_ttl
    }

    #[must_use]
    pub const fn max_jitter_secs(self) -> u64 {
        self.max_jitter_secs
    }

    #[must_use]
    pub fn period_with_random_jitter(self) -> Duration {
        self.base_interval
            + Duration::from_secs(rand::rng().random_range(0u64..=self.max_jitter_secs))
    }

    #[must_use]
    pub fn period_for_user(self, user_id: &UserId) -> Duration {
        let jitter_secs = if self.max_jitter_secs == 0 {
            0
        } else {
            user_id.as_i64().unsigned_abs() % (self.max_jitter_secs + 1)
        };
        self.base_interval + Duration::from_secs(jitter_secs)
    }

    #[must_use]
    pub fn period_for_actor(self, actor: &RealtimeActor) -> Duration {
        use std::hash::{Hash, Hasher};

        let jitter_secs = if self.max_jitter_secs == 0 {
            0
        } else {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            actor.connection_key().hash(&mut hasher);
            hasher.finish() % (self.max_jitter_secs + 1)
        };
        self.base_interval + Duration::from_secs(jitter_secs)
    }
}
