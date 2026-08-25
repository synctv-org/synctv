//! Axum middleware for collecting HTTP request metrics.

use axum::{
    extract::{MatchedPath, Request},
    middleware::Next,
    response::Response,
};
use std::time::Instant;

use synctv_api_common::observability::metrics;

/// Middleware that records HTTP request count, duration, and in-flight gauge.
pub async fn metrics_layer(request: Request, next: Next) -> Response {
    let method = request.method().to_string();
    let path = request.extensions().get::<MatchedPath>().map_or_else(
        || "<unmatched>".to_string(),
        |path| path.as_str().to_string(),
    );

    let _in_flight = metrics::start_request();
    let start = Instant::now();

    let response = next.run(request).await;

    metrics::record_request(&method, &path, response.status().as_u16(), start.elapsed());

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

        let output = synctv_api_common::observability::metrics::gather_metrics()
            .expect("metrics should encode");
        assert!(output.contains(
            "http_requests_total{method=\"GET\",path=\"/items/{item_id}\",status=\"200\"}"
        ));
        assert!(!output.contains("private-resource-id"));
    }
}
