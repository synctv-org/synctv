use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{broadcast, watch};

use crate::proto::{ShutdownMode as ProtoShutdownMode, StopServerEvent, StopServerStage};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownMode {
    Graceful,
    Force,
}

impl ShutdownMode {
    #[must_use]
    pub const fn as_proto(self) -> i32 {
        match self {
            Self::Graceful => ProtoShutdownMode::Graceful as i32,
            Self::Force => ProtoShutdownMode::Force as i32,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleStage {
    Ready,
    ShutdownRequested,
    RuntimeDraining,
    ConnectionDraining,
    ComponentsShuttingDown,
    Finalizing,
    Completed,
    Failed,
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
    next_sequence: AtomicU64,
    shutdown_tx: watch::Sender<Option<ShutdownMode>>,
    current_shutdown_mode: RwLock<Option<ShutdownMode>>,
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
                next_sequence: AtomicU64::new(initial_event.sequence),
                shutdown_tx,
                current_shutdown_mode: RwLock::new(None),
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
        *read_lifecycle_lock(&self.inner.current_shutdown_mode, "current_shutdown_mode")
    }

    #[must_use]
    pub fn subscribe(&self) -> LifecycleSubscription {
        LifecycleSubscription {
            snapshot: self.latest_event(),
            receiver: self.inner.event_tx.subscribe(),
        }
    }

    pub fn request_shutdown(&self, requested_mode: ShutdownMode) -> LifecycleEvent {
        let effective_mode = self.merge_shutdown_mode(requested_mode);
        let message = match effective_mode {
            ShutdownMode::Graceful => "shutdown requested".to_string(),
            ShutdownMode::Force => "force shutdown requested".to_string(),
        };
        self.publish(
            LifecycleStage::ShutdownRequested,
            effective_mode,
            message,
            false,
        )
    }

    pub fn publish_runtime_draining(&self) {
        let mode = self
            .current_shutdown_mode()
            .unwrap_or(ShutdownMode::Graceful);
        self.publish(
            LifecycleStage::RuntimeDraining,
            mode,
            "runtime shutdown in progress".to_string(),
            false,
        );
    }

    pub fn publish_connection_draining(&self) {
        let mode = self
            .current_shutdown_mode()
            .unwrap_or(ShutdownMode::Graceful);
        self.publish(
            LifecycleStage::ConnectionDraining,
            mode,
            "connection draining in progress".to_string(),
            false,
        );
    }

    pub fn publish_components_shutting_down(&self) {
        let mode = self
            .current_shutdown_mode()
            .unwrap_or(ShutdownMode::Graceful);
        self.publish(
            LifecycleStage::ComponentsShuttingDown,
            mode,
            "component shutdown in progress".to_string(),
            false,
        );
    }

    pub fn publish_finalizing(&self) {
        let mode = self
            .current_shutdown_mode()
            .unwrap_or(ShutdownMode::Graceful);
        self.publish(
            LifecycleStage::Finalizing,
            mode,
            "final shutdown tasks in progress".to_string(),
            false,
        );
    }

    pub fn publish_completed(&self) {
        let mode = self
            .current_shutdown_mode()
            .unwrap_or(ShutdownMode::Graceful);
        self.publish(
            LifecycleStage::Completed,
            mode,
            "shutdown complete".to_string(),
            true,
        );
    }

    pub fn publish_failure(&self, message: impl Into<String>) {
        let mode = self
            .current_shutdown_mode()
            .unwrap_or(ShutdownMode::Graceful);
        self.publish(LifecycleStage::Failed, mode, message.into(), true);
    }

    #[must_use]
    pub fn latest_event(&self) -> LifecycleEvent {
        read_lifecycle_lock(&self.inner.latest_event, "latest_event").clone()
    }

    fn merge_shutdown_mode(&self, requested_mode: ShutdownMode) -> ShutdownMode {
        let current_mode =
            *read_lifecycle_lock(&self.inner.current_shutdown_mode, "current_shutdown_mode");
        let effective_mode = match (current_mode, requested_mode) {
            (Some(ShutdownMode::Force), _)
            | (Some(ShutdownMode::Graceful), ShutdownMode::Force) => ShutdownMode::Force,
            (Some(existing), _) => existing,
            (None, mode) => mode,
        };
        if current_mode != Some(effective_mode) {
            *write_lifecycle_lock(&self.inner.current_shutdown_mode, "current_shutdown_mode") =
                Some(effective_mode);
            let _ = self.inner.shutdown_tx.send(Some(effective_mode));
        }
        effective_mode
    }

    fn publish(
        &self,
        stage: LifecycleStage,
        mode: ShutdownMode,
        message: String,
        terminal: bool,
    ) -> LifecycleEvent {
        let sequence = self.inner.next_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let event = LifecycleEvent {
            sequence,
            stage,
            mode,
            message,
            terminal,
            unix_millis: unix_millis_now(),
        };
        *write_lifecycle_lock(&self.inner.latest_event, "latest_event") = event.clone();
        let _ = self.inner.event_tx.send(event.clone());
        event
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
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
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
            controller.current_shutdown_mode(),
            Some(ShutdownMode::Force)
        );
    }
}
