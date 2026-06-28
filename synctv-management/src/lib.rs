#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

mod access;
pub mod lifecycle;
mod mapping;
pub mod server;
mod service;
mod source_config;

pub use service::{ManagementServiceImpl, ManagementSliceCacheRuntime};

pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/descriptor.bin"));

#[allow(clippy::pedantic)]
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/synctv.management.rs"));
    include!(concat!(env!("OUT_DIR"), "/synctv.management.serde.rs"));
}

#[cfg(test)]
mod tests {
    use std::io;

    use prost::Message;
    use prost_types::FileDescriptorSet;

    use crate::FILE_DESCRIPTOR_SET;

    #[test]
    fn management_descriptor_uses_structured_unary_responses(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let descriptor = FileDescriptorSet::decode(FILE_DESCRIPTOR_SET)?;
        let file = descriptor
            .file
            .iter()
            .find(|file| file.package.as_deref() == Some("synctv.management"))
            .ok_or_else(|| io::Error::other("synctv.management descriptor file should exist"))?;
        let service = file
            .service
            .iter()
            .find(|service| service.name.as_deref() == Some("ManagementService"))
            .ok_or_else(|| io::Error::other("ManagementService descriptor should exist"))?;

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
                .ok_or_else(|| io::Error::other("management method output type should exist"))?;
            assert_ne!(
                output_type,
                ".synctv.management.JsonResponse",
                "management unary method {} must return a structured protobuf message",
                method.name.as_deref().unwrap_or("<unknown>")
            );
        }
        Ok(())
    }

    #[test]
    fn management_proto_json_uses_lower_camel_and_integer_enums(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let users: crate::proto::ListUsersRequest =
            serde_json::from_str(r#"{"pageSize":20,"sortDirection":2,"status":1,"role":3}"#)?;
        assert_eq!(users.page_size, 20);
        assert_eq!(
            users.sort_direction,
            crate::proto::SortDirection::Desc as i32
        );
        assert_eq!(
            users.status,
            synctv_proto::common::UserStatus::Active as i32
        );
        assert_eq!(users.role, synctv_proto::common::UserRole::User as i32);

        let playlists: crate::proto::ListPlaylistsRequest = serde_json::from_str(
            r#"{"roomId":"room-1","pageSize":50,"providerInstanceName":"alist-main","sortDirection":1}"#,
        )?;
        assert_eq!(playlists.room_id, "room-1");
        assert_eq!(playlists.page_size, 50);
        assert_eq!(playlists.provider_instance_name, "alist-main");
        assert_eq!(
            playlists.sort_direction,
            crate::proto::SortDirection::Asc as i32
        );
        Ok(())
    }

    #[test]
    fn management_proto_json_rejects_snake_case_and_enum_strings() {
        assert!(serde_json::from_str::<crate::proto::ListUsersRequest>(
            r#"{"page_size":20,"sort_direction":2}"#
        )
        .is_err());
        assert!(serde_json::from_str::<crate::proto::ListUsersRequest>(
            r#"{"pageSize":20,"sortDirection":"SORT_DIRECTION_DESC"}"#
        )
        .is_err());
        assert!(serde_json::from_str::<crate::proto::ListPlaylistsRequest>(
            r#"{"room_id":"room-1","provider_instance_name":"alist-main"}"#
        )
        .is_err());
    }

    #[test]
    fn management_descriptor_does_not_embed_provider_service_contracts(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let descriptor = FileDescriptorSet::decode(FILE_DESCRIPTOR_SET)?;

        let forbidden_services = [
            ("synctv.provider.alist", "AlistProviderService"),
            ("synctv.provider.bilibili", "BilibiliProviderService"),
            ("synctv.provider.emby", "EmbyProviderService"),
            ("synctv.provider.rtmp", "RtmpProviderService"),
        ];

        let embedded_provider_service = descriptor.file.iter().find_map(|file| {
            forbidden_services
                .iter()
                .find_map(|(package, service_name)| {
                    file.service
                        .iter()
                        .find(|service| {
                            service.name.as_deref() == Some(*service_name)
                                && file.package.as_deref() == Some(*package)
                        })
                        .map(|_| format!("{package}.{service_name}"))
                })
        });

        assert!(
            embedded_provider_service.is_none(),
            "management descriptor must reference provider messages without embedding provider service contracts: {embedded_provider_service:?}"
        );
        Ok(())
    }

    #[test]
    fn management_src_does_not_keep_extern_generated_contracts() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let forbidden_generated_files = [
            "src/synctv.admin.rs",
            "src/synctv.client.rs",
            "src/synctv.provider.alist.rs",
            "src/synctv.provider.bilibili.rs",
            "src/synctv.provider.emby.rs",
            "src/synctv.provider.rtmp.rs",
            "src/buf.validate.rs",
            "src/synctv.management.rs",
            "src/descriptor.bin",
        ];

        let stale_files = forbidden_generated_files
            .iter()
            .filter(|relative_path| manifest_dir.join(relative_path).exists())
            .copied()
            .collect::<Vec<_>>();

        assert!(
            stale_files.is_empty(),
            "management must use synctv-proto extern_path for shared contracts; remove stale generated file(s): {stale_files:?}"
        );
    }
}
