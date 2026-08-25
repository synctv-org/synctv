use super::*;

#[derive(Debug, Clone, Copy)]
pub enum RelayProtocol {
    Hls,
    Rtmp,
}

impl RelayProtocol {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Hls => "hls",
            Self::Rtmp => "rtmp",
        }
    }
}

pub static STREAM_RELAY_DURATION: std::sync::LazyLock<HistogramVec> =
    std::sync::LazyLock::new(|| {
        histogram_vec(
            HistogramOpts::new(
                "stream_relay_duration_seconds",
                "Stream relay operation duration in seconds",
            ),
            &["stream_type"],
        )
    });

pub static ACTIVE_RELAY_STREAMS: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
    int_gauge(
        "active_relay_streams",
        "Current number of active relay streams",
    )
});

pub static STREAM_ERRORS: std::sync::LazyLock<IntCounterVec> = std::sync::LazyLock::new(|| {
    int_counter_vec(
        "stream_errors_total",
        "Total number of stream errors",
        &["stream_type", "error_type"],
    )
});

pub fn track_relay(protocol: RelayProtocol) -> InFlightTimer {
    InFlightTimer::start(
        &ACTIVE_RELAY_STREAMS,
        &STREAM_RELAY_DURATION.with_label_values(&[protocol.as_str()]),
    )
}

pub fn record_error(protocol: RelayProtocol, error: &str) {
    let error_type = if error.contains("timeout") {
        "timeout"
    } else if error.contains("connection") {
        "connection"
    } else {
        "other"
    };
    STREAM_ERRORS
        .with_label_values(&[protocol.as_str(), error_type])
        .inc();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_error_classification_has_a_bounded_value_set() {
        let timeout = STREAM_ERRORS.with_label_values(&["rtmp", "timeout"]);
        let connection = STREAM_ERRORS.with_label_values(&["rtmp", "connection"]);
        let other = STREAM_ERRORS.with_label_values(&["rtmp", "other"]);
        let before = (timeout.get(), connection.get(), other.get());

        record_error(RelayProtocol::Rtmp, "request timeout");
        record_error(RelayProtocol::Rtmp, "connection reset");
        record_error(RelayProtocol::Rtmp, "codec failure with id 123");

        assert_eq!(timeout.get(), before.0 + 1);
        assert_eq!(connection.get(), before.1 + 1);
        assert_eq!(other.get(), before.2 + 1);
    }
}
