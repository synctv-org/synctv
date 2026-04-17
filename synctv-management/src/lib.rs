#![cfg_attr(test, allow(clippy::unwrap_used))]

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive — use only one");

#[cfg(all(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
compile_error!(
    "features \"tls-webpki-roots\" and \"tls-native-roots\" are mutually exclusive — use only one"
);

pub mod lifecycle;
pub mod server;
pub mod service;

pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!("descriptor.bin");

pub mod proto {
    include!("synctv.management.rs");
}

#[cfg(test)]
mod tests {
    use prost::Message;
    use prost_types::FileDescriptorSet;

    use crate::FILE_DESCRIPTOR_SET;

    #[test]
    fn management_descriptor_uses_structured_unary_responses() {
        let descriptor = FileDescriptorSet::decode(FILE_DESCRIPTOR_SET)
            .expect("management descriptor set should decode");
        let file = descriptor
            .file
            .iter()
            .find(|file| file.package.as_deref() == Some("synctv.management"))
            .expect("synctv.management descriptor file should exist");
        let service = file
            .service
            .iter()
            .find(|service| service.name.as_deref() == Some("ManagementService"))
            .expect("ManagementService descriptor should exist");

        for method in &service.method {
            if method.name.as_deref() == Some("StopServer") {
                continue;
            }

            assert!(
                !method.server_streaming.unwrap_or(false),
                "unary method {} unexpectedly became server-streaming",
                method.name.as_deref().unwrap_or("<unknown>")
            );
            assert!(
                !method.client_streaming.unwrap_or(false),
                "unary method {} unexpectedly became client-streaming",
                method.name.as_deref().unwrap_or("<unknown>")
            );

            let output_type = method
                .output_type
                .as_deref()
                .expect("management method output type should exist");
            assert_ne!(
                output_type,
                ".synctv.management.JsonResponse",
                "management unary method {} must return a structured protobuf message",
                method.name.as_deref().unwrap_or("<unknown>")
            );
        }
    }
}
