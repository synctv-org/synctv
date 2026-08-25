use std::{
    collections::BTreeMap,
    sync::{LazyLock, Mutex},
};

use prometheus::{
    core::Collector, Encoder, GaugeVec, HistogramOpts, HistogramVec, IntCounter, IntCounterVec,
    IntGauge, Opts, Registry, TextEncoder,
};

static REGISTRY: LazyLock<MetricsRegistry> = LazyLock::new(MetricsRegistry::new);

#[derive(Debug)]
struct MetricsRegistry {
    inner: Registry,
    descriptors: Mutex<BTreeMap<String, MetricDescriptor>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MetricKind {
    Counter,
    Gauge,
    Histogram,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MetricDescriptor {
    pub kind: MetricKind,
    pub labels: Vec<String>,
}

impl MetricsRegistry {
    fn new() -> Self {
        let registry = Self {
            inner: Registry::new(),
            descriptors: Mutex::new(BTreeMap::new()),
        };
        #[cfg(target_os = "linux")]
        registry.register_collector(
            prometheus::process_collector::ProcessCollector::for_self(),
            "process",
        );
        registry
    }

    #[cfg(test)]
    fn empty() -> Self {
        Self {
            inner: Registry::new(),
            descriptors: Mutex::new(BTreeMap::new()),
        }
    }

    fn register<T>(&self, metric: T, name: &str, kind: MetricKind) -> T
    where
        T: Collector + Clone + 'static,
    {
        let descriptors = metric.desc();
        self.register_collector(metric.clone(), name);
        let mut registered = self
            .descriptors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for descriptor in descriptors {
            registered.insert(
                descriptor.fq_name.clone(),
                MetricDescriptor {
                    kind,
                    labels: descriptor.variable_labels.clone(),
                },
            );
        }
        metric
    }

    fn register_collector<T>(&self, collector: T, name: &str)
    where
        T: Collector + 'static,
    {
        self.inner
            .register(Box::new(collector))
            .unwrap_or_else(|error| panic!("registering Prometheus metric `{name}`: {error}"));
    }

    fn gather(&self) -> Result<String, MetricsError> {
        let mut buffer = Vec::new();
        TextEncoder::new().encode(&self.inner.gather(), &mut buffer)?;
        Ok(String::from_utf8(buffer)?)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    #[error("failed to encode Prometheus metrics: {0}")]
    Encode(#[from] prometheus::Error),
    #[error("Prometheus text encoder produced invalid UTF-8: {0}")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
}

fn register<T>(metric: T, name: &str, kind: MetricKind) -> T
where
    T: Collector + Clone + 'static,
{
    REGISTRY.register(metric, name, kind)
}

pub(super) fn int_counter(name: &str, help: &str) -> IntCounter {
    let metric = IntCounter::new(name, help)
        .unwrap_or_else(|error| panic!("defining Prometheus metric `{name}`: {error}"));
    register(metric, name, MetricKind::Counter)
}

pub(super) fn int_gauge(name: &str, help: &str) -> IntGauge {
    let metric = IntGauge::new(name, help)
        .unwrap_or_else(|error| panic!("defining Prometheus metric `{name}`: {error}"));
    register(metric, name, MetricKind::Gauge)
}

pub(super) fn int_counter_vec(name: &str, help: &str, labels: &[&str]) -> IntCounterVec {
    let metric = IntCounterVec::new(Opts::new(name, help), labels)
        .unwrap_or_else(|error| panic!("defining Prometheus metric `{name}`: {error}"));
    register(metric, name, MetricKind::Counter)
}

pub(super) fn gauge_vec(name: &str, help: &str, labels: &[&str]) -> GaugeVec {
    let metric = GaugeVec::new(Opts::new(name, help), labels)
        .unwrap_or_else(|error| panic!("defining Prometheus metric `{name}`: {error}"));
    register(metric, name, MetricKind::Gauge)
}

pub(super) fn histogram_vec(opts: HistogramOpts, labels: &[&str]) -> HistogramVec {
    let name = opts.common_opts.fq_name();
    let metric = HistogramVec::new(opts, labels)
        .unwrap_or_else(|error| panic!("defining Prometheus metric `{name}`: {error}"));
    register(metric, &name, MetricKind::Histogram)
}

pub(super) fn gather() -> Result<String, MetricsError> {
    REGISTRY.gather()
}

#[cfg(test)]
pub(super) fn descriptors() -> BTreeMap<String, MetricDescriptor> {
    REGISTRY
        .descriptors
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "registering Prometheus metric `duplicate_metric`")]
    fn registry_fails_fast_on_duplicate_metric_names() {
        let registry = MetricsRegistry::empty();
        let first = IntGauge::new("duplicate_metric", "first definition").expect("valid metric");
        let duplicate =
            IntGauge::new("duplicate_metric", "second definition").expect("valid metric");

        registry.register(first, "duplicate_metric", MetricKind::Gauge);
        registry.register(duplicate, "duplicate_metric", MetricKind::Gauge);
    }
}
