#[cfg(verus_only)]
use vstd::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(verus_only, verus_verify)]
pub enum ShutdownMode {
    Graceful,
    Force,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(verus_only, verus_verify)]
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

#[cfg_attr(verus_only, verus_spec(out => ensures (out is Force) == (current == Some(ShutdownMode::Force) || requested is Force)))]
pub(super) const fn merge(current: Option<ShutdownMode>, requested: ShutdownMode) -> ShutdownMode {
    if matches!(current, Some(ShutdownMode::Force)) {
        ShutdownMode::Force
    } else {
        requested
    }
}
impl LifecycleStage {
    #[cfg_attr(verus_only, verus_spec(out => ensures out == (self is Completed || self is Failed)))]
    pub(super) const fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}
