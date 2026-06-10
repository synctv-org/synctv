//! HTTP extraction helpers.
//!
//! Transport handlers deserialize inputs and hand requests to `impls`; request
//! validation belongs in the impl/core layers shared by HTTP and gRPC.

use axum::{
    extract::{rejection::QueryRejection, FromRequestParts, Query},
    http::request::Parts,
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

fn map_query_rejection(rejection: &QueryRejection) -> super::AppError {
    let mut error = super::AppError::new(rejection.status(), rejection.body_text());
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
        response::IntoResponse,
        routing::get,
        Router,
    };
    use tower::ServiceExt;

    #[derive(Debug, serde::Deserialize)]
    struct PageQuery {
        page: i32,
    }

    type TestResult<T = ()> = anyhow::Result<T>;

    async fn query_handler(ProtoQuery(query): ProtoQuery<PageQuery>) -> &'static str {
        assert_eq!(query.page, 1);
        "ok"
    }

    #[tokio::test]
    async fn proto_query_rejection_uses_app_error_code() -> TestResult {
        let app = Router::new().route("/test", get(query_handler));
        let request = Request::builder()
            .uri("/test?page=abc")
            .body(Body::empty())?;

        let response = app.oneshot(request).await?.into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let json: serde_json::Value = serde_json::from_slice(&body)?;

        assert_eq!(json["status"], 400);
        assert_eq!(json["code"], crate::impls::error_codes::INVALID_ARGUMENT);
        assert!(matches!(
            json["error"].as_str(),
            Some(message) if message.contains("page")
        ));
        Ok(())
    }

    #[test]
    fn protobuf_json_serde_accepts_proto3_int64_strings() -> TestResult {
        let req = serde_json::from_str::<synctv_proto::client::CreateUserAvatarUploadSessionRequest>(
            r#"{
                    "client_avatar_id":"avatar-1",
                    "mime_type":"image/png",
                    "size_bytes":"1764839",
                    "width":256,
                    "height":256,
                    "checksum_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "metadata":{}
                }"#,
        )?;

        assert_eq!(req.size_bytes, 1_764_839);
        Ok(())
    }
}
