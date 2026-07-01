use std::collections::HashMap;
use std::time::Duration;

use axum::http::StatusCode;
use prost::Message as _;
use prost_reflect::ReflectMessage as _;
use synctv_proto::google::rpc;
use tonic::{Code, Status};
use tonic_types::{ErrorDetails, StatusExt};

const ERROR_DOMAIN: &str = "synctv.api";
const REQUEST_ID_METADATA_KEY: &str = "requestId";
const ERROR_CODE_METADATA_KEY: &str = "errorCode";
const OAUTH2_OPERATION_METADATA_KEY: &str = "oauth2Operation";

#[derive(Debug, Clone)]
pub struct GoogleApiError {
    pub grpc_code: Code,
    pub http_status: StatusCode,
    pub message: String,
    pub retry_after_seconds: Option<u64>,
    details: ErrorDetails,
}

impl GoogleApiError {
    #[must_use]
    pub fn from_api_error(err: &crate::impls::ApiError) -> Self {
        let classification = ErrorClassification::from_kind(err.classify());
        let message = err.message().to_string();
        let mut metadata = HashMap::from([
            (ERROR_CODE_METADATA_KEY.to_string(), err.code().to_string()),
            (
                "errorKind".to_string(),
                classification.reason.to_ascii_lowercase(),
            ),
        ]);
        if let Some(operation) = err.oauth2_operation() {
            metadata.insert(
                OAUTH2_OPERATION_METADATA_KEY.to_string(),
                operation.as_str().to_string(),
            );
        }

        let mut details = ErrorDetails::new();
        details.set_error_info(classification.reason, ERROR_DOMAIN, metadata);

        match err {
            crate::impls::ApiError::InvalidInput(message) => {
                details.add_bad_request_violation("request", message);
            }
            crate::impls::ApiError::RangeNotSatisfiable { total_size } => {
                details.set_resource_info(
                    "byte_range",
                    total_size.max(&0).to_string(),
                    "",
                    "Requested byte range is not satisfiable",
                );
            }
            crate::impls::ApiError::RateLimitedWithRetry {
                retry_after_seconds,
                ..
            }
            | crate::impls::ApiError::OAuth2General {
                retry_after_seconds: Some(retry_after_seconds),
                ..
            } => {
                details.set_retry_info(Some(Duration::from_secs(*retry_after_seconds)));
            }
            _ => {}
        }

        Self {
            grpc_code: classification.grpc_code,
            http_status: http_status_for_error(err, classification.http_status),
            message,
            retry_after_seconds: err.retry_after_seconds(),
            details,
        }
    }

    #[must_use]
    pub fn with_request_id(mut self, request_id: Option<&str>) -> Self {
        if let Some(request_id) = request_id.filter(|value| !value.is_empty()) {
            let mut metadata = self
                .details
                .error_info()
                .map(|detail| detail.metadata.clone())
                .unwrap_or_default();
            let reason = self
                .details
                .error_info()
                .map_or_else(|| "UNKNOWN".to_string(), |detail| detail.reason.clone());
            metadata.insert(REQUEST_ID_METADATA_KEY.to_string(), request_id.to_string());
            self.details.set_error_info(reason, ERROR_DOMAIN, metadata);
            self.details.set_request_info(request_id, "");
        }
        self
    }

    #[must_use]
    pub fn to_tonic_status(&self) -> Status {
        Status::with_error_details(self.grpc_code, self.message.clone(), self.details.clone())
    }

    pub fn to_rpc_status(&self) -> Result<rpc::Status, prost::DecodeError> {
        let status = self.to_tonic_status();
        let tonic_status = tonic_types::pb::Status::decode(status.details())?;
        Ok(rpc::Status {
            code: tonic_status.code,
            message: tonic_status.message,
            details: tonic_status
                .details
                .into_iter()
                .map(|detail| pbjson_types::Any {
                    type_url: detail.type_url,
                    value: detail.value.into(),
                })
                .collect(),
        })
    }

    pub fn to_protojson_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        let rpc_status = self
            .to_rpc_status()
            .map_err(|error| serde_json::Error::io(std::io::Error::other(error)))?;
        let dynamic = rpc_status.transcode_to_dynamic();
        let mut output = Vec::new();
        let mut serializer = serde_json::Serializer::new(&mut output);
        dynamic
            .serialize_with_options(&mut serializer, &prost_reflect::SerializeOptions::new())
            .map_err(|error| serde_json::Error::io(std::io::Error::other(error)))?;
        Ok(output)
    }
}

#[derive(Debug, Clone, Copy)]
struct ErrorClassification {
    grpc_code: Code,
    http_status: StatusCode,
    reason: &'static str,
}

impl ErrorClassification {
    const fn from_kind(kind: crate::impls::ErrorKind) -> Self {
        match kind {
            crate::impls::ErrorKind::NotFound => Self {
                grpc_code: Code::NotFound,
                http_status: StatusCode::NOT_FOUND,
                reason: "NOT_FOUND",
            },
            crate::impls::ErrorKind::Unauthenticated => Self {
                grpc_code: Code::Unauthenticated,
                http_status: StatusCode::UNAUTHORIZED,
                reason: "UNAUTHENTICATED",
            },
            crate::impls::ErrorKind::PermissionDenied => Self {
                grpc_code: Code::PermissionDenied,
                http_status: StatusCode::FORBIDDEN,
                reason: "PERMISSION_DENIED",
            },
            crate::impls::ErrorKind::AlreadyExists => Self {
                grpc_code: Code::AlreadyExists,
                http_status: StatusCode::CONFLICT,
                reason: "ALREADY_EXISTS",
            },
            crate::impls::ErrorKind::Conflict => Self {
                grpc_code: Code::Aborted,
                http_status: StatusCode::CONFLICT,
                reason: "CONFLICT",
            },
            crate::impls::ErrorKind::InvalidArgument => Self {
                grpc_code: Code::InvalidArgument,
                http_status: StatusCode::BAD_REQUEST,
                reason: "INVALID_ARGUMENT",
            },
            crate::impls::ErrorKind::RateLimited => Self {
                grpc_code: Code::ResourceExhausted,
                http_status: StatusCode::TOO_MANY_REQUESTS,
                reason: "RATE_LIMITED",
            },
            crate::impls::ErrorKind::ServiceUnavailable => Self {
                grpc_code: Code::Unavailable,
                http_status: StatusCode::SERVICE_UNAVAILABLE,
                reason: "SERVICE_UNAVAILABLE",
            },
            crate::impls::ErrorKind::Timeout => Self {
                grpc_code: Code::DeadlineExceeded,
                http_status: StatusCode::GATEWAY_TIMEOUT,
                reason: "TIMEOUT",
            },
            crate::impls::ErrorKind::Internal => Self {
                grpc_code: Code::Internal,
                http_status: StatusCode::INTERNAL_SERVER_ERROR,
                reason: "INTERNAL",
            },
        }
    }
}

fn http_status_for_error(err: &crate::impls::ApiError, default_status: StatusCode) -> StatusCode {
    match err {
        crate::impls::ApiError::RangeNotSatisfiable { .. } => StatusCode::RANGE_NOT_SATISFIABLE,
        crate::impls::ApiError::BadGateway(_) => StatusCode::BAD_GATEWAY,
        crate::impls::ApiError::RequestTimeout(_) => StatusCode::REQUEST_TIMEOUT,
        _ => default_status,
    }
}

#[must_use]
pub fn sanitized_api_error(err: &crate::impls::ApiError) -> crate::impls::ApiError {
    match err.classify() {
        crate::impls::ErrorKind::Internal => {
            tracing::error!("API internal error: {}", err.message());
            crate::impls::ApiError::Internal("Internal error".to_string())
        }
        _ => err.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail_by_type<'a>(
        json: &'a serde_json::Value,
        type_name: &str,
    ) -> Option<&'a serde_json::Value> {
        let expected = format!("type.googleapis.com/{type_name}");
        json["details"]
            .as_array()?
            .iter()
            .find(|detail| detail["@type"].as_str() == Some(expected.as_str()))
    }

    #[test]
    fn protojson_expands_standard_error_details() -> anyhow::Result<()> {
        let error = GoogleApiError::from_api_error(&crate::impls::ApiError::InvalidInput(
            "email is invalid".to_string(),
        ))
        .with_request_id(Some("req_test_1"));

        let bytes = error.to_protojson_bytes()?;
        let json: serde_json::Value = serde_json::from_slice(&bytes)?;

        assert_eq!(json["code"], tonic::Code::InvalidArgument as i32);
        assert_eq!(json["message"], "email is invalid");

        let error_info = detail_by_type(&json, "google.rpc.ErrorInfo")
            .ok_or_else(|| anyhow::anyhow!("missing ErrorInfo detail: {json}"))?;
        assert_eq!(error_info["reason"], "INVALID_ARGUMENT");
        assert_eq!(error_info["domain"], ERROR_DOMAIN);
        assert_eq!(
            error_info["metadata"][ERROR_CODE_METADATA_KEY],
            crate::impls::error_codes::INVALID_ARGUMENT.to_string()
        );
        assert_eq!(
            error_info["metadata"][REQUEST_ID_METADATA_KEY],
            "req_test_1"
        );

        let request_info = detail_by_type(&json, "google.rpc.RequestInfo")
            .ok_or_else(|| anyhow::anyhow!("missing RequestInfo detail: {json}"))?;
        assert_eq!(request_info["requestId"], "req_test_1");

        let bad_request = detail_by_type(&json, "google.rpc.BadRequest")
            .ok_or_else(|| anyhow::anyhow!("missing BadRequest detail: {json}"))?;
        let violations = bad_request["fieldViolations"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("BadRequest.fieldViolations is missing"))?;
        assert_eq!(violations[0]["field"], "request");
        assert_eq!(violations[0]["description"], "email is invalid");

        Ok(())
    }

    #[test]
    fn tonic_status_contains_richer_error_details() -> anyhow::Result<()> {
        let error = GoogleApiError::from_api_error(&crate::impls::ApiError::RateLimitedWithRetry {
            message: "too many requests".to_string(),
            retry_after_seconds: 9,
        });

        let status = error.to_tonic_status();
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
        assert_eq!(status.message(), "too many requests");

        let rpc_status = tonic_types::pb::Status::decode(status.details())?;
        assert_eq!(rpc_status.code, tonic::Code::ResourceExhausted as i32);
        assert!(rpc_status
            .details
            .iter()
            .any(|detail| detail.type_url == "type.googleapis.com/google.rpc.ErrorInfo"));
        assert!(rpc_status
            .details
            .iter()
            .any(|detail| detail.type_url == "type.googleapis.com/google.rpc.RetryInfo"));

        Ok(())
    }
}
