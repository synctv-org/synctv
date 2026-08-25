use prometheus::{Histogram, HistogramTimer, IntGauge};

/// Keeps an integer gauge balanced across early returns, cancellation, and panics.
#[derive(Debug)]
#[must_use = "the guard must be held for as long as the measured operation is active"]
pub struct GaugeGuard {
    gauge: IntGauge,
}

impl GaugeGuard {
    pub fn increment(gauge: &IntGauge) -> Self {
        gauge.inc();
        Self {
            gauge: gauge.clone(),
        }
    }
}

impl Drop for GaugeGuard {
    fn drop(&mut self) {
        self.gauge.dec();
    }
}

/// Measures an operation's active count and duration through one lexical lifetime.
#[derive(Debug)]
#[must_use = "the guard must be held until the measured operation completes"]
pub struct InFlightTimer {
    _active: GaugeGuard,
    _duration: HistogramTimer,
}

impl InFlightTimer {
    pub fn start(active: &IntGauge, duration: &Histogram) -> Self {
        Self {
            _active: GaugeGuard::increment(active),
            _duration: duration.start_timer(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gauge_guard_balances_the_gauge_when_dropped() {
        let gauge = IntGauge::new("guard_test_gauge", "test gauge").expect("valid gauge");

        {
            let _guard = GaugeGuard::increment(&gauge);
            assert_eq!(gauge.get(), 1);
        }

        assert_eq!(gauge.get(), 0);
    }

    #[test]
    fn in_flight_timer_records_duration_and_balances_the_gauge() {
        let gauge = IntGauge::new("timer_test_gauge", "test gauge").expect("valid gauge");
        let histogram =
            Histogram::with_opts(prometheus::HistogramOpts::new("timer_test", "test timer"))
                .expect("valid histogram");

        {
            let _guard = InFlightTimer::start(&gauge, &histogram);
            assert_eq!(gauge.get(), 1);
        }

        assert_eq!(gauge.get(), 0);
        assert_eq!(histogram.get_sample_count(), 1);
    }
}
