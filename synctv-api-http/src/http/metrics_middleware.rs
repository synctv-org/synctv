//! Axum middleware for collecting HTTP request metrics.

use axum::{
    extract::{MatchedPath, Request},
    middleware::Next,
    response::Response,
};
use std::time::Instant;

use synctv_api_common::observability::metrics;

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
    let path = request.extensions().get::<MatchedPath>().map_or_else(
        || "<unmatched>".to_string(),
        |path| path.as_str().to_string(),
    );

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

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::get,
        Router,
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn metrics_use_matched_route_without_resource_ids() {
        let app = Router::new()
            .route("/items/{item_id}", get(|| async { StatusCode::OK }))
            .layer(axum::middleware::from_fn(super::metrics_layer));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/items/private-resource-id")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);

        let output = synctv_api_common::observability::metrics::gather_metrics();
        assert!(output.contains(
            "http_requests_total{method=\"GET\",path=\"/items/{item_id}\",status=\"200\"}"
        ));
        assert!(!output.contains("private-resource-id"));
    }
}
