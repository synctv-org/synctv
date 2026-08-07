//! HTTP extraction helpers.
//!
//! Transport handlers deserialize inputs and hand requests to `impls`; request
//! validation belongs in the impl/core layers shared by HTTP and gRPC.

use axum::{extract::FromRequestParts, http::request::Parts};
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

fn map_query_rejection(message: impl std::fmt::Display) -> super::AppError {
    super::AppError::from(synctv_api_common::impls::ApiError::InvalidInput(format!(
        "Failed to deserialize query string: {message}"
    )))
}

impl<S, T> FromRequestParts<S> for ProtoQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = super::AppError;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        let deserializer = serde_html_form::Deserializer::from_bytes(
            parts.uri.query().unwrap_or_default().as_bytes(),
        );
        std::future::ready(
            serde_path_to_error::deserialize(deserializer)
                .map(Self)
                .map_err(map_query_rejection),
        )
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

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RepeatedQuery {
        #[serde(default)]
        label_ids: Vec<String>,
    }

    type TestResult<T = ()> = anyhow::Result<T>;

    async fn query_handler(ProtoQuery(query): ProtoQuery<PageQuery>) -> &'static str {
        assert_eq!(query.page, 1);
        "ok"
    }

    async fn repeated_query_handler(ProtoQuery(query): ProtoQuery<RepeatedQuery>) -> &'static str {
        assert_eq!(query.label_ids, ["roomlbl_1", "roomlbl_2"]);
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

        assert_eq!(json["code"], tonic::Code::InvalidArgument as i32);
        assert!(matches!(
            json["message"].as_str(),
            Some(message) if message.contains("page")
        ));
        let error_info = json["details"]
            .as_array()
            .and_then(|details| {
                details.iter().find(|detail| {
                    detail["@type"].as_str() == Some("type.googleapis.com/google.rpc.ErrorInfo")
                })
            })
            .ok_or_else(|| anyhow::anyhow!("missing ErrorInfo detail: {json}"))?;
        assert_eq!(
            error_info["metadata"]["errorCode"],
            synctv_api_common::impls::error_codes::INVALID_ARGUMENT.to_string()
        );
        Ok(())
    }

    #[tokio::test]
    async fn proto_query_preserves_repeated_fields() -> TestResult {
        let app = Router::new().route("/test", get(repeated_query_handler));
        let request = Request::builder()
            .uri("/test?labelIds=roomlbl_1&labelIds=roomlbl_2")
            .body(Body::empty())?;

        let response = app.oneshot(request).await?.into_response();

        assert_eq!(response.status(), StatusCode::OK);
        Ok(())
    }

    #[test]
    fn protobuf_json_serde_accepts_proto3_int64_strings() -> TestResult {
        let req = serde_json::from_str::<synctv_proto::client::CreateUserAvatarUploadSessionRequest>(
            r#"{
                    "clientAvatarId":"avatar-1",
                    "mimeType":"image/png",
                    "sizeBytes":"1764839",
                    "width":256,
                    "height":256,
                    "parts":[{
                        "partNumber":1,
                        "offsetBytes":"0",
                        "sizeBytes":"1764839",
                        "checksumSha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    }],
                    "metadata":{}
                }"#,
        )?;

        assert_eq!(req.size_bytes, 1_764_839);
        Ok(())
    }
}
