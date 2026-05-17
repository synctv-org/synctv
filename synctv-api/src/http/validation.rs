//! HTTP extraction helpers.
//!
//! Transport handlers deserialize inputs and hand requests to `impls`; request
//! validation belongs in the impl/core layers shared by HTTP and gRPC.

use axum::{body::Body, extract::FromRequest};
use axum::{
    extract::rejection::JsonRejection,
    extract::{rejection::QueryRejection, FromRequestParts, Query},
    http::request::Parts,
    Json,
};
use serde::de::DeserializeOwned;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtoQuery<T>(pub T);

impl<T> std::ops::Deref for ProtoQuery<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> std::ops::DerefMut for ProtoQuery<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtoJson<T>(pub T);

impl<T> std::ops::Deref for ProtoJson<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> std::ops::DerefMut for ProtoJson<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

fn map_query_rejection(rejection: &QueryRejection) -> super::AppError {
    let mut error = super::AppError::new(rejection.status(), rejection.body_text());
    error.error_code = Some(crate::impls::error_codes::INVALID_ARGUMENT);
    error
}

fn map_json_rejection(rejection: &JsonRejection) -> super::AppError {
    let mut error = super::AppError::bad_request(rejection.body_text());
    error.error_code = Some(crate::impls::error_codes::INVALID_ARGUMENT);
    error
}

impl<S, T> FromRequestParts<S> for ProtoQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = super::AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Query(value) = Query::<T>::from_request_parts(parts, state)
            .await
            .map_err(|rejection| map_query_rejection(&rejection))?;
        Ok(Self(value))
    }
}

impl<S, T> FromRequest<S, Body> for ProtoJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = super::AppError;

    async fn from_request(
        request: axum::extract::Request<Body>,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(request, state)
            .await
            .map_err(|rejection| map_json_rejection(&rejection))?;
        Ok(Self(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::{Method, Request, StatusCode},
        response::IntoResponse,
        routing::{get, post},
        Router,
    };
    use serde::Deserialize;
    use tower::ServiceExt;

    #[derive(Debug, Deserialize)]
    struct PageQuery {
        page: i32,
    }

    async fn query_handler(ProtoQuery(query): ProtoQuery<PageQuery>) -> &'static str {
        assert_eq!(query.page, 1);
        "ok"
    }

    async fn json_handler(ProtoJson(query): ProtoJson<PageQuery>) -> &'static str {
        assert_eq!(query.page, 1);
        "ok"
    }

    #[tokio::test]
    async fn proto_query_rejection_uses_app_error_code() {
        let app = Router::new().route("/test", get(query_handler));
        let request = Request::builder()
            .uri("/test?page=abc")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap().into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["status"], 400);
        assert_eq!(json["code"], crate::impls::error_codes::INVALID_ARGUMENT);
        assert!(json["error"].as_str().unwrap_or_default().contains("page"));
    }

    #[tokio::test]
    async fn proto_json_rejection_uses_app_error_code() {
        let app = Router::new().route("/test", post(json_handler));
        let request = Request::builder()
            .method(Method::POST)
            .uri("/test")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"page":"abc"}"#))
            .unwrap();

        let response = app.oneshot(request).await.unwrap().into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["status"], 400);
        assert_eq!(json["code"], crate::impls::error_codes::INVALID_ARGUMENT);
        assert!(json["error"].as_str().unwrap_or_default().contains("page"));
    }
}
