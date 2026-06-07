use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use synctv_core::models::id::{RoomId, UserId};
use tracing::warn;

type ConnectionDeadline = (Instant, String);

#[derive(Debug, Default)]
pub(super) struct TimeoutIndex {
    idle_deadlines: BTreeSet<ConnectionDeadline>,
    idle_by_connection: HashMap<String, Instant>,
    max_deadlines: BTreeSet<ConnectionDeadline>,
    max_by_connection: HashMap<String, Instant>,
    rtc_deadlines: BTreeSet<ConnectionDeadline>,
    rtc_by_connection: HashMap<String, Instant>,
}

impl TimeoutIndex {
    fn update_deadline(
        deadlines: &mut BTreeSet<ConnectionDeadline>,
        deadlines_by_connection: &mut HashMap<String, Instant>,
        connection_id: &str,
        deadline: Instant,
    ) {
        if let Some(previous_deadline) =
            deadlines_by_connection.insert(connection_id.to_string(), deadline)
        {
            deadlines.remove(&(previous_deadline, connection_id.to_string()));
        }
        deadlines.insert((deadline, connection_id.to_string()));
    }

    fn clear_deadline(
        deadlines: &mut BTreeSet<ConnectionDeadline>,
        deadlines_by_connection: &mut HashMap<String, Instant>,
        connection_id: &str,
    ) {
        if let Some(previous_deadline) = deadlines_by_connection.remove(connection_id) {
            deadlines.remove(&(previous_deadline, connection_id.to_string()));
        }
    }

    pub(super) fn schedule_idle(&mut self, connection_id: &str, deadline: Instant) {
        Self::update_deadline(
            &mut self.idle_deadlines,
            &mut self.idle_by_connection,
            connection_id,
            deadline,
        );
    }

    pub(super) fn schedule_max_duration(&mut self, connection_id: &str, deadline: Instant) {
        Self::update_deadline(
            &mut self.max_deadlines,
            &mut self.max_by_connection,
            connection_id,
            deadline,
        );
    }

    pub(super) fn schedule_rtc(&mut self, connection_id: &str, deadline: Instant) {
        Self::update_deadline(
            &mut self.rtc_deadlines,
            &mut self.rtc_by_connection,
            connection_id,
            deadline,
        );
    }

    pub(super) fn clear_rtc(&mut self, connection_id: &str) {
        Self::clear_deadline(
            &mut self.rtc_deadlines,
            &mut self.rtc_by_connection,
            connection_id,
        );
    }

    pub(super) fn remove_connection(&mut self, connection_id: &str) {
        Self::clear_deadline(
            &mut self.idle_deadlines,
            &mut self.idle_by_connection,
            connection_id,
        );
        Self::clear_deadline(
            &mut self.max_deadlines,
            &mut self.max_by_connection,
            connection_id,
        );
        Self::clear_deadline(
            &mut self.rtc_deadlines,
            &mut self.rtc_by_connection,
            connection_id,
        );
    }

    fn collect_due(deadlines: &mut BTreeSet<ConnectionDeadline>, now: Instant) -> Vec<String> {
        let mut due_connection_ids = Vec::new();
        while let Some((deadline, connection_id)) = deadlines.first().cloned() {
            if deadline >= now {
                break;
            }
            deadlines.pop_first();
            due_connection_ids.push(connection_id);
        }
        due_connection_ids
    }

    pub(super) fn take_due_idle(&mut self, now: Instant) -> Vec<String> {
        let due = Self::collect_due(&mut self.idle_deadlines, now);
        for connection_id in &due {
            self.idle_by_connection.remove(connection_id);
        }
        due
    }

    pub(super) fn take_due_max_duration(&mut self, now: Instant) -> Vec<String> {
        let due = Self::collect_due(&mut self.max_deadlines, now);
        for connection_id in &due {
            self.max_by_connection.remove(connection_id);
        }
        due
    }

    pub(super) fn take_due_rtc(&mut self, now: Instant) -> Vec<String> {
        let due = Self::collect_due(&mut self.rtc_deadlines, now);
        for connection_id in &due {
            self.rtc_by_connection.remove(connection_id);
        }
        due
    }
}

/// Connection information.
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub connection_id: String,
    pub registration_token: String,
    pub user_id: UserId,
    pub actor_id: String,
    pub room_id: Option<RoomId>,
    pub connected_at: Instant,
    pub last_activity: Instant,
    pub message_count: u64,
    pub rtc_joined: bool,
    pub rtc_joined_at: Option<Instant>,
}

/// Serializable version of `ConnectionInfo` for Redis persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ConnectionInfoPersistent {
    pub(super) connection_id: String,
    pub(super) registration_token: String,
    pub(super) user_id: UserId,
    #[serde(default)]
    pub(super) actor_id: String,
    pub(super) room_id: Option<RoomId>,
    pub(super) connected_at_unix: u64,
    pub(super) last_activity_unix: u64,
    pub(super) message_count: u64,
    pub(super) rtc_joined: bool,
    pub(super) rtc_joined_at_unix: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RoomTransition {
    pub(super) previous_room_id: Option<RoomId>,
    pub(super) room_id: RoomId,
}

pub(super) fn system_time_to_unix_secs(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .unwrap_or_else(|error| {
            warn!("System clock is before UNIX_EPOCH; using zero timestamp fallback: {error}");
            Duration::ZERO
        })
        .as_secs()
}

pub(super) fn u64_to_usize_saturating(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

pub(super) fn i64_to_usize_saturating(value: i64) -> usize {
    usize::try_from(value.max(0)).unwrap_or(usize::MAX)
}

pub(super) fn i64_to_u64_saturating(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

pub(super) fn usize_to_i64_saturating(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(super) fn usize_to_u64_saturating(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

impl From<&ConnectionInfo> for ConnectionInfoPersistent {
    fn from(info: &ConnectionInfo) -> Self {
        let now = SystemTime::now();
        let now_unix = system_time_to_unix_secs(now);
        let connected_at_unix = now_unix.saturating_sub(info.connected_at.elapsed().as_secs());
        let last_activity_unix = now_unix.saturating_sub(info.last_activity.elapsed().as_secs());
        let rtc_joined_at_unix = info
            .rtc_joined_at
            .map(|joined| now_unix.saturating_sub(joined.elapsed().as_secs()));

        Self {
            connection_id: info.connection_id.clone(),
            registration_token: info.registration_token.clone(),
            user_id: info.user_id,
            actor_id: info.actor_id.clone(),
            room_id: info.room_id,
            connected_at_unix,
            last_activity_unix,
            message_count: info.message_count,
            rtc_joined: info.rtc_joined,
            rtc_joined_at_unix,
        }
    }
}

impl ConnectionInfo {
    #[must_use]
    pub fn new(connection_id: String, user_id: UserId) -> Self {
        Self::new_with_actor_id(connection_id, user_id, user_id.to_string())
    }

    #[must_use]
    pub fn new_with_actor_id(connection_id: String, user_id: UserId, actor_id: String) -> Self {
        let now = Instant::now();
        Self {
            connection_id,
            registration_token: synctv_common::snanoid!(16),
            user_id,
            actor_id,
            room_id: None,
            connected_at: now,
            last_activity: now,
            message_count: 0,
            rtc_joined: false,
            rtc_joined_at: None,
        }
    }

    #[must_use]
    pub fn duration(&self) -> Duration {
        self.connected_at.elapsed()
    }

    #[must_use]
    pub fn idle_duration(&self) -> Duration {
        self.last_activity.elapsed()
    }

    #[must_use]
    pub fn rtc_session_duration(&self) -> Option<Duration> {
        self.rtc_joined_at.map(|joined| joined.elapsed())
    }
}
