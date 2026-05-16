//! Axum middleware for collecting HTTP request metrics.

use axum::{extract::Request, middleware::Next, response::Response};
use std::time::Instant;

use super::metrics;

struct InFlightRequestGuard;

impl InFlightRequestGuard {
    fn new() -> Self {
        metrics::HTTP_REQUESTS_IN_FLIGHT.inc();
        Self
    }
}

impl Drop for InFlightRequestGuard {
    fn drop(&mut self) {
        metrics::HTTP_REQUESTS_IN_FLIGHT.dec();
    }
}

/// Middleware that records HTTP request count, duration, and in-flight gauge.
pub async fn metrics_layer(request: Request, next: Next) -> Response {
    let method = request.method().to_string();
    let path = metrics::normalize_path(request.uri().path());

    let _in_flight = InFlightRequestGuard::new();
    let start = Instant::now();

    let response = next.run(request).await;

    let duration = start.elapsed().as_secs_f64();
    let status = response.status().as_u16().to_string();

    metrics::HTTP_REQUESTS_TOTAL
        .with_label_values(&[&method, &path, &status])
        .inc();
    metrics::HTTP_REQUEST_DURATION_SECONDS
        .with_label_values(&[&method, &path])
        .observe(duration);

    response
}
