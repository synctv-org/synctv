use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{broadcast, watch};

use crate::proto::{ShutdownMode as ProtoShutdownMode, StopServerEvent, StopServerStage};

mod transition;
pub use transition::{LifecycleStage, ShutdownMode};

impl ShutdownMode {
    #[must_use]
    pub const fn as_proto(self) -> i32 {
        match self {
            Self::Graceful => ProtoShutdownMode::Graceful as i32,
            Self::Force => ProtoShutdownMode::Force as i32,
        }
    }
}

impl LifecycleStage {
    #[must_use]
    pub const fn as_proto(self) -> i32 {
        match self {
            Self::Ready => StopServerStage::Ready as i32,
            Self::ShutdownRequested => StopServerStage::ShutdownRequested as i32,
            Self::RuntimeDraining => StopServerStage::RuntimeDraining as i32,
            Self::ConnectionDraining => StopServerStage::ConnectionDraining as i32,
            Self::ComponentsShuttingDown => StopServerStage::ComponentsShuttingDown as i32,
            Self::Finalizing => StopServerStage::Finalizing as i32,
            Self::Completed => StopServerStage::Completed as i32,
            Self::Failed => StopServerStage::Failed as i32,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LifecycleEvent {
    pub sequence: u64,
    pub stage: LifecycleStage,
    pub mode: ShutdownMode,
    pub message: String,
    pub terminal: bool,
    pub unix_millis: i64,
}

impl LifecycleEvent {
    #[must_use]
    pub fn to_proto(&self) -> StopServerEvent {
        StopServerEvent {
            sequence: self.sequence,
            stage: self.stage.as_proto(),
            mode: self.mode.as_proto(),
            message: self.message.clone(),
            terminal: self.terminal,
            unix_millis: self.unix_millis,
        }
    }
}

pub struct LifecycleSubscription {
    pub snapshot: LifecycleEvent,
    pub receiver: broadcast::Receiver<LifecycleEvent>,
}

struct Inner {
    shutdown_tx: watch::Sender<Option<ShutdownMode>>,
    event_tx: broadcast::Sender<LifecycleEvent>,
    latest_event: RwLock<LifecycleEvent>,
}

#[derive(Clone)]
pub struct ManagementLifecycleController {
    inner: Arc<Inner>,
}

impl Default for ManagementLifecycleController {
    fn default() -> Self {
        Self::new()
    }
}

impl ManagementLifecycleController {
    #[must_use]
    pub fn new() -> Self {
        let initial_event = LifecycleEvent {
            sequence: 1,
            stage: LifecycleStage::Ready,
            mode: ShutdownMode::Graceful,
            message: "server ready".to_string(),
            terminal: false,
            unix_millis: unix_millis_now(),
        };
        let (shutdown_tx, _) = watch::channel(None);
        let (event_tx, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(Inner {
                shutdown_tx,
                event_tx,
                latest_event: RwLock::new(initial_event),
            }),
        }
    }

    #[must_use]
    pub fn shutdown_receiver(&self) -> watch::Receiver<Option<ShutdownMode>> {
        self.inner.shutdown_tx.subscribe()
    }

    #[must_use]
    pub fn current_shutdown_mode(&self) -> Option<ShutdownMode> {
        *self.inner.shutdown_tx.borrow()
    }

    #[must_use]
    pub fn subscribe(&self) -> LifecycleSubscription {
        let latest = read_lifecycle_lock(&self.inner.latest_event, "latest_event");
        LifecycleSubscription {
            snapshot: latest.clone(),
            receiver: self.inner.event_tx.subscribe(),
        }
    }

    pub fn request_shutdown(&self, requested_mode: ShutdownMode) -> LifecycleEvent {
        let mut latest = write_lifecycle_lock(&self.inner.latest_event, "latest_event");
        let current = self.current_shutdown_mode();
        let effective_mode = transition::merge(current, requested_mode);
        if current != Some(effective_mode) {
            self.inner.shutdown_tx.send_replace(Some(effective_mode));
        }
        let message = match effective_mode {
            ShutdownMode::Graceful => "shutdown requested".to_string(),
            ShutdownMode::Force => "force shutdown requested".to_string(),
        };
        self.publish(
            &mut latest,
            LifecycleStage::ShutdownRequested,
            effective_mode,
            message,
        )
    }

    pub fn publish_runtime_draining(&self) {
        self.publish_current(
            LifecycleStage::RuntimeDraining,
            "runtime shutdown in progress".to_string(),
        );
    }

    pub fn publish_connection_draining(&self) {
        self.publish_current(
            LifecycleStage::ConnectionDraining,
            "connection draining in progress".to_string(),
        );
    }

    pub fn publish_components_shutting_down(&self) {
        self.publish_current(
            LifecycleStage::ComponentsShuttingDown,
            "component shutdown in progress".to_string(),
        );
    }

    pub fn publish_finalizing(&self) {
        self.publish_current(
            LifecycleStage::Finalizing,
            "final shutdown tasks in progress".to_string(),
        );
    }

    pub fn publish_completed(&self) {
        self.publish_current(LifecycleStage::Completed, "shutdown complete".to_string());
    }

    pub fn publish_failure(&self, message: impl Into<String>) {
        self.publish_current(LifecycleStage::Failed, message.into());
    }

    fn publish(
        &self,
        latest: &mut LifecycleEvent,
        stage: LifecycleStage,
        mode: ShutdownMode,
        message: String,
    ) -> LifecycleEvent {
        let sequence = latest.sequence.saturating_add(1);
        let event = LifecycleEvent {
            sequence,
            stage,
            mode,
            message,
            terminal: stage.terminal(),
            unix_millis: unix_millis_now(),
        };
        *latest = event.clone();
        if self.inner.event_tx.send(event.clone()).is_err() {
            tracing::debug!(
                sequence = event.sequence,
                stage = ?event.stage,
                "management lifecycle event published with no active subscribers"
            );
        }
        event
    }

    fn publish_current(&self, stage: LifecycleStage, message: String) -> LifecycleEvent {
        let mut latest = write_lifecycle_lock(&self.inner.latest_event, "latest_event");
        let mode = self
            .current_shutdown_mode()
            .unwrap_or(ShutdownMode::Graceful);
        self.publish(&mut latest, stage, mode, message)
    }
}

fn read_lifecycle_lock<'a, T>(lock: &'a RwLock<T>, name: &'static str) -> RwLockReadGuard<'a, T> {
    match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::error!(
                lock = name,
                "management lifecycle lock was poisoned; recovering stored state"
            );
            poisoned.into_inner()
        }
    }
}

fn write_lifecycle_lock<'a, T>(lock: &'a RwLock<T>, name: &'static str) -> RwLockWriteGuard<'a, T> {
    match lock.write() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::error!(
                lock = name,
                "management lifecycle lock was poisoned; recovering stored state"
            );
            poisoned.into_inner()
        }
    }
}

fn unix_millis_now() -> i64 {
    let millis = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis(),
        Err(error) => {
            tracing::warn!(%error, "system clock is before Unix epoch");
            0
        }
    };
    i64::try_from(millis).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{LifecycleStage, ManagementLifecycleController, ShutdownMode};

    #[test]
    fn force_shutdown_request_escalates_existing_graceful_request() {
        let controller = ManagementLifecycleController::new();

        let graceful = controller.request_shutdown(ShutdownMode::Graceful);
        assert_eq!(graceful.stage, LifecycleStage::ShutdownRequested);
        assert_eq!(graceful.mode, ShutdownMode::Graceful);

        let force = controller.request_shutdown(ShutdownMode::Force);
        assert_eq!(force.stage, LifecycleStage::ShutdownRequested);
        assert_eq!(force.mode, ShutdownMode::Force);
        assert_eq!(
            controller.request_shutdown(ShutdownMode::Graceful).mode,
            ShutdownMode::Force
        );
        assert_eq!(
            *controller.shutdown_receiver().borrow(),
            Some(ShutdownMode::Force)
        );
        assert_eq!(controller.subscribe().snapshot.mode, ShutdownMode::Force);
        assert_eq!(
            controller.current_shutdown_mode(),
            Some(ShutdownMode::Force)
        );
    }
}
