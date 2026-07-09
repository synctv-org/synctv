use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use rsntp::{AsyncSntpClient, Config as SntpConfig, SynchronizationError};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const NANOS_PER_SECOND_I64: i64 = 1_000_000_000;
const NANOS_PER_MILLI_I64: i64 = 1_000_000;

#[derive(Debug, Clone, Default)]
pub struct TimeOptions {
    pub timezone: String,
    pub clock_sync: ClockSyncOptions,
}

#[derive(Debug, Clone, Default)]
pub struct ClockSyncOptions {
    pub enabled: bool,
    pub provider: ClockSyncProvider,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockSyncProvider {
    Sntp(ClockSyncSntpProviderOptions),
}

impl Default for ClockSyncProvider {
    fn default() -> Self {
        Self::Sntp(ClockSyncSntpProviderOptions::default())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockSyncSntpProviderOptions {
    pub servers: Vec<String>,
    pub interval_seconds: u64,
    pub timeout_millis: u64,
}

impl Default for ClockSyncSntpProviderOptions {
    fn default() -> Self {
        Self {
            servers: vec![
                "time.cloudflare.com:123".to_string(),
                "pool.ntp.org:123".to_string(),
            ],
            interval_seconds: 300,
            timeout_millis: 1_000,
        }
    }
}

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;

    fn now_millis(&self) -> i64 {
        self.now().timestamp_millis()
    }

    fn now_nanos(&self) -> i64 {
        self.now()
            .timestamp_nanos_opt()
            .unwrap_or_else(|| self.now_millis().saturating_mul(1_000_000))
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl SystemClock {
    #[must_use]
    pub fn now(self) -> DateTime<Utc> {
        Utc::now()
    }

    #[must_use]
    pub fn now_millis(self) -> i64 {
        self.now().timestamp_millis()
    }

    #[must_use]
    pub fn now_nanos(self) -> i64 {
        self.now()
            .timestamp_nanos_opt()
            .unwrap_or_else(|| self.now_millis().saturating_mul(1_000_000))
    }
}

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        (*self).now()
    }

    fn now_millis(&self) -> i64 {
        (*self).now_millis()
    }

    fn now_nanos(&self) -> i64 {
        (*self).now_nanos()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncedClockStatus {
    pub enabled: bool,
    pub synchronized: bool,
    pub offset_millis: i64,
    pub last_sync_at_millis: Option<i64>,
}

pub struct SyncedClock {
    enabled: bool,
    servers: Vec<String>,
    interval: Duration,
    offset_nanos: AtomicI64,
    last_sync_at_millis: AtomicI64,
    synchronized: AtomicBool,
    anchor: RwLock<Option<SyncedClockAnchor>>,
    client: AsyncSntpClient,
}

#[derive(Debug, Clone)]
struct SyncedClockAnchor {
    synced_unix_nanos: i64,
    monotonic_at_sync: Instant,
}

impl SyncedClock {
    #[must_use]
    pub fn from_options(options: &TimeOptions) -> Self {
        let clock_sync = &options.clock_sync;
        let ClockSyncProvider::Sntp(provider) = &clock_sync.provider;
        let timeout = Duration::from_millis(provider.timeout_millis.max(1));
        let client = AsyncSntpClient::with_config(SntpConfig::default().timeout(timeout));
        Self {
            enabled: clock_sync.enabled,
            servers: provider.servers.clone(),
            interval: Duration::from_secs(provider.interval_seconds.max(1)),
            offset_nanos: AtomicI64::new(0),
            last_sync_at_millis: AtomicI64::new(0),
            synchronized: AtomicBool::new(false),
            anchor: RwLock::new(None),
            client,
        }
    }

    #[must_use]
    pub fn system() -> Self {
        Self::from_options(&TimeOptions::default())
    }

    pub fn start(self: &Arc<Self>, cancel: CancellationToken) -> Option<JoinHandle<()>> {
        if !self.enabled || self.servers.is_empty() {
            return None;
        }
        let clock = self.clone();
        Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(clock.interval);
            interval.tick().await;
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    _ = interval.tick() => clock.sync_once_logging_errors().await,
                }
            }
        }))
    }

    fn synced_now(&self) -> DateTime<Utc> {
        if let Some(now_nanos) = self.synced_now_nanos() {
            return utc_from_nanos(now_nanos).unwrap_or_else(Utc::now);
        }

        let offset = self.offset_nanos.load(Ordering::Relaxed);
        let system_now = Utc::now();
        system_now
            .checked_add_signed(chrono::Duration::nanoseconds(offset))
            .unwrap_or(system_now)
    }

    #[must_use]
    pub fn now(&self) -> DateTime<Utc> {
        self.synced_now()
    }

    #[must_use]
    pub fn now_millis(&self) -> i64 {
        self.now().timestamp_millis()
    }

    #[must_use]
    pub fn now_nanos(&self) -> i64 {
        if let Some(now_nanos) = self.synced_now_nanos() {
            return now_nanos;
        }

        self.now()
            .timestamp_nanos_opt()
            .unwrap_or_else(|| self.now_millis().saturating_mul(1_000_000))
    }

    #[must_use]
    pub fn status(&self) -> SyncedClockStatus {
        let last_sync_at_millis = self.last_sync_at_millis.load(Ordering::Relaxed);
        SyncedClockStatus {
            enabled: self.enabled,
            synchronized: self.synchronized.load(Ordering::Relaxed),
            offset_millis: offset_nanos_to_millis(self.offset_nanos.load(Ordering::Relaxed)),
            last_sync_at_millis: (last_sync_at_millis > 0).then_some(last_sync_at_millis),
        }
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    pub async fn sync_once(&self) -> Result<SyncedClockStatus, SyncedClockError> {
        if !self.enabled {
            return Ok(self.status());
        }
        if self.servers.is_empty() {
            return Err(SyncedClockError::NoServersConfigured);
        }

        let mut last_error = None;
        for server in &self.servers {
            match self.client.synchronize(server.as_str()).await {
                Ok(result) => {
                    let offset_nanos =
                        offset_seconds_to_nanos(result.clock_offset().as_secs_f64())?;
                    let sync_instant = Instant::now();
                    let synced_unix_nanos = corrected_unix_nanos(Utc::now(), offset_nanos)
                        .ok_or(SyncedClockError::CorrectedTimeOutOfRange)?;
                    self.offset_nanos.store(offset_nanos, Ordering::Relaxed);
                    self.store_anchor(SyncedClockAnchor {
                        synced_unix_nanos,
                        monotonic_at_sync: sync_instant,
                    });
                    self.last_sync_at_millis.store(
                        synced_unix_nanos.div_euclid(NANOS_PER_MILLI_I64),
                        Ordering::Relaxed,
                    );
                    self.synchronized.store(true, Ordering::Relaxed);
                    return Ok(self.status());
                }
                Err(error) => {
                    last_error = Some(SyncedClockError::Sync {
                        server: server.clone(),
                        source: error,
                    });
                }
            }
        }

        self.synchronized.store(false, Ordering::Relaxed);
        Err(last_error.unwrap_or(SyncedClockError::NoServersConfigured))
    }

    fn store_anchor(&self, anchor: SyncedClockAnchor) {
        match self.anchor.write() {
            Ok(mut guard) => {
                *guard = Some(anchor);
            }
            Err(poisoned) => {
                *poisoned.into_inner() = Some(anchor);
            }
        }
    }

    fn synced_now_nanos(&self) -> Option<i64> {
        let anchor = match self.anchor.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }?;
        let elapsed_nanos =
            i64::try_from(anchor.monotonic_at_sync.elapsed().as_nanos()).unwrap_or(i64::MAX);
        Some(anchor.synced_unix_nanos.saturating_add(elapsed_nanos))
    }

    async fn sync_once_logging_errors(&self) {
        match self.sync_once().await {
            Ok(status) => {
                tracing::debug!(
                    offset_millis = status.offset_millis,
                    last_sync_at_millis = status.last_sync_at_millis,
                    "application clock synchronized"
                );
            }
            Err(error) => {
                tracing::warn!(error = %error, "application clock synchronization failed");
            }
        }
    }
}

impl Clock for SyncedClock {
    fn now(&self) -> DateTime<Utc> {
        self.synced_now()
    }

    fn now_nanos(&self) -> i64 {
        if let Some(now_nanos) = self.synced_now_nanos() {
            return now_nanos;
        }

        self.now()
            .timestamp_nanos_opt()
            .unwrap_or_else(|| self.now_millis().saturating_mul(1_000_000))
    }
}

fn offset_seconds_to_nanos(seconds: f64) -> Result<i64, SyncedClockError> {
    if seconds == 0.0 {
        return Ok(0);
    }
    let duration =
        Duration::try_from_secs_f64(seconds.abs()).map_err(|_| SyncedClockError::OffsetOverflow)?;
    let nanos = i64::try_from(duration.as_nanos()).map_err(|_| SyncedClockError::OffsetOverflow)?;
    Ok(if seconds.is_sign_negative() {
        nanos.saturating_neg()
    } else {
        nanos
    })
}

fn offset_nanos_to_millis(nanos: i64) -> i64 {
    if nanos >= 0 {
        nanos.saturating_add(NANOS_PER_MILLI_I64 / 2) / NANOS_PER_MILLI_I64
    } else {
        nanos.saturating_sub(NANOS_PER_MILLI_I64 / 2) / NANOS_PER_MILLI_I64
    }
}

fn corrected_unix_nanos(system_now: DateTime<Utc>, offset_nanos: i64) -> Option<i64> {
    system_now.timestamp_nanos_opt()?.checked_add(offset_nanos)
}

#[derive(Debug, thiserror::Error)]
pub enum SyncedClockError {
    #[error("no time synchronization servers configured")]
    NoServersConfigured,
    #[error("time synchronization failed for {server}: {source}")]
    Sync {
        server: String,
        source: SynchronizationError,
    },
    #[error("time synchronization offset overflowed i64 nanoseconds")]
    OffsetOverflow,
    #[error("synchronized application time is outside supported timestamp range")]
    CorrectedTimeOutOfRange,
}

#[must_use]
pub fn utc_from_millis(millis: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt(millis).single()
}

#[must_use]
pub fn utc_from_nanos(nanos: i64) -> Option<DateTime<Utc>> {
    let seconds = nanos.div_euclid(NANOS_PER_SECOND_I64);
    let nanoseconds = u32::try_from(nanos.rem_euclid(NANOS_PER_SECOND_I64)).ok()?;
    Utc.timestamp_opt(seconds, nanoseconds).single()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_reports_unsynchronized_status() {
        let clock = SyncedClock::system();
        let status = clock.status();

        assert!(!status.enabled);
        assert!(!status.synchronized);
        assert_eq!(status.offset_millis, 0);
        assert_eq!(status.last_sync_at_millis, None);
        assert!(clock.now_millis() > 0);
    }

    #[tokio::test]
    async fn disabled_clock_sync_once_keeps_system_mode() {
        let clock = SyncedClock::system();
        let status = clock
            .sync_once()
            .await
            .expect("disabled sync should succeed");

        assert!(!status.enabled);
        assert!(!status.synchronized);
        assert_eq!(status.offset_millis, 0);
    }

    #[tokio::test]
    async fn enabled_clock_without_servers_fails_initial_sync() {
        let clock = SyncedClock::from_options(&TimeOptions {
            clock_sync: ClockSyncOptions {
                enabled: true,
                provider: ClockSyncProvider::Sntp(ClockSyncSntpProviderOptions {
                    servers: Vec::new(),
                    timeout_millis: 0,
                    interval_seconds: 0,
                }),
            },
            ..TimeOptions::default()
        });

        let error = clock
            .sync_once()
            .await
            .expect_err("enabled clock without servers should fail");

        assert!(matches!(error, SyncedClockError::NoServersConfigured));
    }

    #[test]
    fn utc_from_nanos_handles_negative_subsecond_values() {
        let datetime = utc_from_nanos(-1).expect("timestamp should be representable");

        assert_eq!(datetime.timestamp(), -1);
        assert_eq!(datetime.timestamp_subsec_nanos(), 999_999_999);
    }

    #[test]
    fn synchronized_clock_uses_monotonic_anchor() {
        let clock = SyncedClock::system();
        let base = Utc
            .with_ymd_and_hms(2026, 7, 6, 0, 0, 0)
            .single()
            .expect("valid test time")
            .timestamp_nanos_opt()
            .expect("test time fits in nanoseconds");
        clock.store_anchor(SyncedClockAnchor {
            synced_unix_nanos: base,
            monotonic_at_sync: Instant::now()
                .checked_sub(Duration::from_millis(25))
                .expect("test duration should be smaller than process uptime"),
        });

        let now = clock.now_nanos();

        assert!(now >= base + 25_000_000);
    }

    #[test]
    fn system_clock_reports_epoch_based_values() {
        let seconds = u64::try_from(SystemClock.now().timestamp()).unwrap_or(0);
        let millis = u128::try_from(SystemClock.now_millis()).unwrap_or(0);

        assert!(seconds > 0);
        assert!(millis >= u128::from(seconds) * 1_000);
    }
}
