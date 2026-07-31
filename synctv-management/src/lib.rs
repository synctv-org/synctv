#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

mod access;
pub mod admin_runtime;
pub mod lifecycle;
mod mapping;
pub mod provider_runtime;
pub mod request_context;
pub mod runtime_error;
pub mod server;
mod service;
mod source_config;

pub use service::ManagementServiceImpl;

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

    #[test]
    fn migrated_runtime_methods_use_management_query_models() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let admin_runtime = std::fs::read_to_string(manifest_dir.join("src/admin_runtime.rs"))
            .expect("admin runtime source should be readable");
        let provider_runtime_dir = manifest_dir.join("src/provider_runtime");
        assert!(
            provider_runtime_dir.is_dir(),
            "provider runtime contracts should be split by provider"
        );
        let provider_runtime = std::fs::read_dir(provider_runtime_dir)
            .expect("provider runtime source directory should be readable")
            .map(|entry| entry.expect("provider runtime directory entry should be readable"))
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "rs")
            })
            .map(|entry| std::fs::read_to_string(entry.path()))
            .collect::<Result<Vec<_>, _>>()
            .expect("provider runtime source files should be readable")
            .join("\n");

        assert!(admin_runtime.contains("query: ListUsersQuery"));
        assert!(admin_runtime.contains("query: GetUserQuery"));
        assert!(admin_runtime.contains("query: GetUserPreferencesQuery"));
        assert!(admin_runtime.contains("command: UpdateUserPreferencesCommand"));
        assert!(admin_runtime.contains("command: AddAdminCommand"));
        assert!(admin_runtime.contains("command: RemoveAdminCommand"));
        assert!(admin_runtime.contains("query: ListAdminsQuery"));
        assert!(admin_runtime.contains("command: CreateUserCommand"));
        assert!(admin_runtime.contains("command: DeleteUserCommand"));
        assert!(admin_runtime.contains("command: BanUserCommand"));
        assert!(admin_runtime.contains("command: UnbanUserCommand"));
        assert!(admin_runtime.contains("query: ListUserRegistrationReviewsQuery"));
        assert!(admin_runtime.contains("command: ApproveUserRegistrationReviewCommand"));
        assert!(admin_runtime.contains("command: RejectUserRegistrationReviewCommand"));
        assert!(admin_runtime.contains("query: ListRoomCreationReviewsQuery"));
        assert!(admin_runtime.contains("command: ApproveRoomCreationReviewCommand"));
        assert!(admin_runtime.contains("command: RejectRoomCreationReviewCommand"));
        assert!(admin_runtime.contains("query: ListRoomJoinReviewsQuery"));
        assert!(admin_runtime.contains("command: ApproveRoomJoinReviewCommand"));
        assert!(admin_runtime.contains("command: RejectRoomJoinReviewCommand"));
        assert!(admin_runtime.contains("query: ListBanRecordsQuery"));
        assert!(admin_runtime.contains("command: UpdateUserRoleCommand"));
        assert!(admin_runtime.contains("command: SetUserPasswordCommand"));
        assert!(admin_runtime.contains("command: UpdateUserUsernameCommand"));
        assert!(admin_runtime.contains("query: GetUserRoomsQuery"));
        assert!(admin_runtime.contains("query: ListRoomsQuery"));
        assert!(admin_runtime.contains("query: ListRoomCategoriesQuery"));
        assert!(admin_runtime.contains("command: UpsertRoomCategoryCommand"));
        assert!(admin_runtime.contains("command: DeleteRoomCategoryCommand"));
        assert!(admin_runtime.contains("query: ListRoomLabelsQuery"));
        assert!(admin_runtime.contains("command: UpsertRoomLabelCommand"));
        assert!(admin_runtime.contains("command: DeleteRoomLabelCommand"));
        assert!(admin_runtime.contains("command: UpdateRoomTaxonomyCommand"));
        assert!(admin_runtime.contains("query: GetRoomQuery"));
        assert!(admin_runtime.contains("query: GetRoomMembersQuery"));
        assert!(admin_runtime.contains("command: AddMemberCommand"));
        assert!(admin_runtime.contains("command: UpdateMemberRemarkNameCommand"));
        assert!(admin_runtime.contains("command: UpdateMemberDisplayTagCommand"));
        assert!(admin_runtime.contains("command: UpdateMemberPermissionsCommand"));
        assert!(admin_runtime.contains("command: KickMemberCommand"));
        assert!(admin_runtime.contains("query: GetRoomSettingsQuery"));
        assert!(admin_runtime.contains("command: UpdateRoomSettingsCommand"));
        assert!(admin_runtime.contains("command: ResetRoomSettingsCommand"));
        assert!(admin_runtime.contains("command: UpdateRoomPasswordCommand"));
        assert!(admin_runtime.contains("command: BanRoomCommand"));
        assert!(admin_runtime.contains("command: UnbanRoomCommand"));
        assert!(admin_runtime.contains("command: DeleteRoomCommand"));
        assert!(admin_runtime.contains("command: BatchBanRoomsCommand"));
        assert!(admin_runtime.contains("command: BatchDeleteRoomsCommand"));
        assert!(admin_runtime.contains("command: StartPlaybackCommand"));
        assert!(admin_runtime.contains("command: UpdatePlaybackStateCommand"));
        assert!(admin_runtime.contains("query: ListRoomStreamsQuery"));
        assert!(admin_runtime.contains("query: ListPlaylistsQuery"));
        assert!(admin_runtime.contains("command: UpdatePlaylistCommand"));
        assert!(admin_runtime.contains("command: MovePlaylistCommand"));
        assert!(admin_runtime.contains("command: DeletePlaylistCommand"));
        assert!(admin_runtime.contains("query: ListMediaQuery"));
        assert!(admin_runtime.contains("command: EditMediaCommand"));
        assert!(admin_runtime.contains("command: DeleteMediaCommand"));
        assert!(admin_runtime.contains("command: MoveMediaCommand"));
        assert!(admin_runtime.contains("command: KickStreamCommand"));
        assert!(admin_runtime.contains("query: GetSettingsQuery"));
        assert!(admin_runtime.contains("command: UpdateSettingsCommand"));
        assert!(admin_runtime.contains("command: SendTestEmailCommand"));
        assert!(admin_runtime.contains("query: GetServiceStateQuery"));
        assert!(admin_runtime.contains("query: ListActiveStreamsQuery"));
        assert!(admin_runtime.contains("command: BatchBanUsersCommand"));
        assert!(admin_runtime.contains("command: BatchDeleteUsersCommand"));
        assert!(!admin_runtime.contains("req: admin_proto::ListUsersRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::GetUserRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::GetUserPreferencesRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::UpdateUserPreferencesRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::AddAdminRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::RemoveAdminRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::ListAdminsRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::CreateUserRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::DeleteUserRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::BanUserRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::UnbanUserRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::ListUserRegistrationReviewsRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::ApproveUserRegistrationReviewRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::RejectUserRegistrationReviewRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::ListRoomCreationReviewsRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::ApproveRoomCreationReviewRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::RejectRoomCreationReviewRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::ListRoomJoinReviewsRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::ApproveRoomJoinReviewRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::RejectRoomJoinReviewRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::ListBanRecordsRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::UpdateUserRoleRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::SetUserPasswordRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::UpdateUserUsernameRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::GetUserRoomsRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::ListRoomsRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::ListRoomCategoriesRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::UpsertRoomCategoryRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::DeleteRoomCategoryRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::ListRoomLabelsRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::UpsertRoomLabelRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::DeleteRoomLabelRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::UpdateRoomTaxonomyRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::GetRoomRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::GetRoomMembersRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::AddMemberRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::UpdateMemberRemarkNameRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::UpdateMemberDisplayTagRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::UpdateMemberPermissionsRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::KickMemberRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::GetRoomSettingsRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::UpdateRoomSettingsRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::ResetRoomSettingsRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::UpdateRoomPasswordRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::BanRoomRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::UnbanRoomRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::DeleteRoomRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::BatchBanRoomsRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::BatchDeleteRoomsRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::KickStreamRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::GetSettingsRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::UpdateSettingsRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::SendTestEmailRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::GetServiceStateRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::ListActiveStreamsRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::BatchBanUsersRequest"));
        assert!(!admin_runtime.contains("req: admin_proto::BatchDeleteUsersRequest"));
        assert!(!admin_runtime.contains("req: client_proto::StartPlaybackRequest"));
        assert!(!admin_runtime.contains("req: client_proto::UpdatePlaybackStateRequest"));
        assert!(!admin_runtime.contains("req: client_proto::ListRoomStreamsRequest"));
        assert!(!admin_runtime.contains("req: client_proto::ListPlaylistsRequest"));
        assert!(!admin_runtime.contains("req: client_proto::UpdatePlaylistRequest"));
        assert!(!admin_runtime.contains("req: client_proto::MovePlaylistRequest"));
        assert!(!admin_runtime.contains("req: client_proto::DeletePlaylistRequest"));
        assert!(!admin_runtime.contains("req: client_proto::ListPlaylistItemsRequest"));
        assert!(!admin_runtime.contains("req: client_proto::EditMediaRequest"));
        assert!(!admin_runtime.contains("req: client_proto::DeleteMediaRequest"));
        assert!(!admin_runtime.contains("req: client_proto::MoveMediaRequest"));

        assert!(provider_runtime.contains("query: ListAvailableProviderInstancesQuery"));
        assert!(provider_runtime.contains("query: ListProviderBackendsQuery"));
        assert!(provider_runtime.contains("query: ProviderInstanceListQuery"));
        assert!(provider_runtime.contains("command: AddProviderInstanceCommand"));
        assert!(provider_runtime.contains("command: UpdateProviderInstanceCommand"));
        assert!(provider_runtime.contains("command: ProviderInstanceNameCommand"));
        assert!(provider_runtime.contains("command: AlistLoginCommand"));
        assert!(provider_runtime.contains("query: AlistListQuery"));
        assert!(provider_runtime.contains("query: AlistSearchQuery"));
        assert!(provider_runtime.contains("query: ProviderCredentialServerQuery"));
        assert!(provider_runtime.contains("command: EmbyLoginCommand"));
        assert!(provider_runtime.contains("query: EmbyListQuery"));
        assert!(provider_runtime.contains("query: BilibiliParseQuery"));
        assert!(provider_runtime.contains("command: BilibiliLoginQrCommand"));
        assert!(provider_runtime.contains("query: BilibiliCheckQrQuery"));
        assert!(provider_runtime.contains("command: BilibiliStartSmsLoginCommand"));
        assert!(provider_runtime.contains("command: BilibiliSendSmsCommand"));
        assert!(provider_runtime.contains("command: BilibiliLoginSmsCommand"));
        assert!(provider_runtime.contains("query: BilibiliUserInfoQuery"));
        assert!(provider_runtime.contains("command: BilibiliLogoutCommand"));
        assert!(!provider_runtime
            .contains("req: provider_common_proto::ListAvailableProviderInstancesRequest"));
        assert!(
            !provider_runtime.contains("req: provider_common_proto::ListProviderBackendsRequest")
        );
        assert!(
            !provider_runtime.contains("req: provider_common_proto::ListProviderInstancesRequest")
        );
        assert!(
            !provider_runtime.contains("req: provider_common_proto::AddProviderInstanceRequest")
        );
        assert!(
            !provider_runtime.contains("req: provider_common_proto::UpdateProviderInstanceRequest")
        );
        assert!(
            !provider_runtime.contains("req: provider_common_proto::DeleteProviderInstanceRequest")
        );
        assert!(!provider_runtime
            .contains("req: provider_common_proto::ReconnectProviderInstanceRequest"));
        assert!(
            !provider_runtime.contains("req: provider_common_proto::EnableProviderInstanceRequest")
        );
        assert!(!provider_runtime
            .contains("req: provider_common_proto::DisableProviderInstanceRequest"));
        assert!(!provider_runtime.contains("req: alist_proto::LoginRequest"));
        assert!(!provider_runtime.contains("req: alist_proto::ListRequest"));
        assert!(!provider_runtime.contains("req: alist_proto::SearchRequest"));
        assert!(!provider_runtime.contains("req: alist_proto::GetMeRequest"));
        assert!(!provider_runtime.contains("req: alist_proto::LogoutRequest"));
        assert!(!provider_runtime.contains("req: emby_proto::LoginRequest"));
        assert!(!provider_runtime.contains("req: emby_proto::ListRequest"));
        assert!(!provider_runtime.contains("req: emby_proto::GetMeRequest"));
        assert!(!provider_runtime.contains("req: emby_proto::LogoutRequest"));
        assert!(!provider_runtime.contains("req: bilibili_proto::ParseRequest"));
        assert!(!provider_runtime.contains("req: bilibili_proto::LoginQrRequest"));
        assert!(!provider_runtime.contains("req: bilibili_proto::CheckQrRequest"));
        assert!(!provider_runtime.contains("req: bilibili_proto::StartSmsLoginRequest"));
        assert!(!provider_runtime.contains("req: bilibili_proto::SendSmsRequest"));
        assert!(!provider_runtime.contains("req: bilibili_proto::LoginSmsRequest"));
        assert!(!provider_runtime.contains("req: bilibili_proto::UserInfoRequest"));
        assert!(!provider_runtime.contains("req: bilibili_proto::LogoutRequest"));
    }
}
