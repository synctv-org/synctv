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
        &crate::proto::providers::common::ProviderInstanceQuery {
            instance_name: instance_name.to_string(),
        },
    )
    .map(|name| name.map(str::to_owned))
    .map_err(crate::grpc::map_api_error)
}

#[cfg(test)]
mod tests {
    #[test]
    fn provider_instance_name_rejects_invalid_grpc_body_field() {
        let status = super::provider_instance_name("bad/name")
            .expect_err("gRPC body instance_name must be validated like HTTP query");

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn provider_instance_name_trims_valid_value() {
        let instance_name = super::provider_instance_name("  alist-main  ")
            .expect("valid instance_name should pass");

        assert_eq!(instance_name.as_deref(), Some("alist-main"));
    }
}
