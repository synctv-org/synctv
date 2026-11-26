use std::sync::{Arc, Mutex, MutexGuard};
use tracing::{error, warn};

mod state;
use state::State;
pub use state::{CIRCUIT_BREAKER_THRESHOLD, CIRCUIT_BREAKER_TIMEOUT_SECS};

/// Per-provider circuit breaker. Each admission or completion updates a single state.
#[derive(Default)]
pub struct CircuitBreaker {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    state: State,
    // In-flight calls keep the old allocation alive. Identity cannot be reused
    // until all of those calls have finished.
    generation: Arc<()>,
}

impl Inner {
    fn fail(&mut self, service: &str) {
        let opened = self.state.fail(unix_timestamp_secs());
        if matches!(self.state, State::Open(_)) {
            self.generation = Arc::new(());
        }
        if opened {
            error!(%service, threshold = CIRCUIT_BREAKER_THRESHOLD, "Circuit breaker opened");
        }
    }
}

/// Admission for one RPC. A dropped half-open probe releases its reservation.
pub struct RequestPermit {
    breaker: Arc<CircuitBreaker>,
    generation: Arc<()>,
    probe_since: Option<i64>,
}

impl RequestPermit {
    pub fn success(mut self) {
        let mut inner = self.breaker.inner();
        if Arc::ptr_eq(&self.generation, &inner.generation) {
            inner.state.succeed();
        }
        self.probe_since = None;
    }

    pub fn failure(mut self, service: &str) {
        let mut inner = self.breaker.inner();
        if Arc::ptr_eq(&self.generation, &inner.generation) {
            inner.fail(service);
        }
        self.probe_since = None;
    }
}

impl Drop for RequestPermit {
    fn drop(&mut self) {
        if let Some(since) = self.probe_since {
            let mut inner = self.breaker.inner();
            if Arc::ptr_eq(&self.generation, &inner.generation)
                && matches!(inner.state, State::Probing)
            {
                inner.state = State::Open(since);
            }
        }
    }
}

fn unix_timestamp_secs() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        Err(error) => {
            warn!(%error, "system clock is before Unix epoch");
            0
        }
    }
}

impl CircuitBreaker {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn try_request(self: &Arc<Self>) -> Option<RequestPermit> {
        let mut inner = self.inner();
        let previous = inner.state;
        if !Self::admit(&mut inner.state) {
            return None;
        }
        Some(RequestPermit {
            breaker: Arc::clone(self),
            generation: Arc::clone(&inner.generation),
            probe_since: match previous {
                State::Open(since) => Some(since),
                _ => None,
            },
        })
    }

    pub fn allow_request(&self) -> bool {
        Self::admit(&mut self.inner().state)
    }

    fn admit(state: &mut State) -> bool {
        // The normal closed path needs no clock read.
        let now = if matches!(*state, State::Open(_)) {
            unix_timestamp_secs()
        } else {
            0
        };
        state.admit(now)
    }

    pub fn record_success(&self) {
        self.inner().state.succeed();
    }

    pub fn record_failure(&self, service: &str) {
        self.inner().fail(service);
    }
}

#[cfg(test)]
#[path = "circuit_breaker_tests.rs"]
mod tests;
