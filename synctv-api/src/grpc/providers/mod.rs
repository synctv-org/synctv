//! Provider gRPC Services
//!
//! Provider-specific gRPC services for parse, browse, proxy, etc.

pub mod alist;
pub mod bilibili;
pub mod common;
pub mod emby;
pub mod rtmp;

pub(crate) fn provider_instance_name(instance_name: &str) -> Result<Option<String>, tonic::Status> {
    crate::impls::providers::common::provider_instance_name_from_query(
        &synctv_proto::providers::common::ProviderInstanceQuery {
            instance_name: instance_name.to_string(),
        },
    )
    .map(|name| name.map(str::to_owned))
    .map_err(crate::grpc::map_api_error)
}

#[cfg(test)]
mod tests {
    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    #[test]
    fn provider_instance_name_rejects_invalid_grpc_body_field() -> TestResult {
        let Err(status) = super::provider_instance_name("bad/name") else {
            return Err(test_error(
                "gRPC body instance_name validation accepted invalid input",
            ));
        };

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        Ok(())
    }

    #[test]
    fn provider_instance_name_trims_valid_value() -> TestResult {
        let instance_name = super::provider_instance_name("  alist-main  ")?;

        assert_eq!(instance_name.as_deref(), Some("alist-main"));
        Ok(())
    }
}
