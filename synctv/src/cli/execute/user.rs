use super::*;

pub(super) async fn execute_user(user_command: UserCommand) -> Result<()> {
    let UserCommand { command } = user_command;
    match command {
        UserSubcommand::List(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "list users",
                list_users,
                management_proto::ListUsersRequest {
                    page: args.page,
                    page_size: args.page_size,
                    status: args.status.map_or(
                        synctv_proto::common::UserStatus::Unspecified as i32,
                        CliUserStatus::to_proto,
                    ),
                    role: args.role.map_or(
                        synctv_proto::common::UserRole::Unspecified as i32,
                        CliUserRole::to_proto,
                    ),
                    search: args.search.unwrap_or_default(),
                    sort_by: args.sort_by.map_or(
                        management_proto::UserListSortBy::CreatedAt as i32,
                        CliUserSortField::to_proto
                    ),
                    sort_direction: args.sort_dir.to_proto(),
                    is_banned: None,
                    include_deleted: args.include_deleted,
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Get(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let (user_id, username) = args.user.to_management_selector()?;
            let response = management_unary_call!(
                session,
                "get user",
                get_user,
                management_proto::GetUserRequest { user_id, username }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Preferences(preferences_command) => match preferences_command.command {
            UserPreferencesSubcommand::Get(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let (user_id, username) = args.user.to_management_selector()?;
                let response = management_unary_call!(
                    session,
                    "get user preferences",
                    get_user_preferences,
                    management_proto::GetUserPreferencesRequest { user_id, username }
                )?;
                print_humanized_structured_output(args.remote.output, &response)
            }
            UserPreferencesSubcommand::Set(args) => {
                let args = *args;
                let notifications = parse_cli_optional_json(
                    "notification preferences",
                    args.notifications_json.as_deref(),
                )?;
                if args.two_factor_enabled.is_none() && notifications.is_none() {
                    bail!("No user preference fields provided");
                }

                let session = connect_remote_access(&args.remote).await?;
                let (user_id, username) = args.user.to_management_selector()?;
                let response = management_unary_call!(
                    session,
                    "update user preferences",
                    update_user_preferences,
                    management_proto::UpdateUserPreferencesRequest {
                        user_id,
                        username,
                        two_factor_enabled: args.two_factor_enabled,
                        notifications,
                    }
                )?;
                print_humanized_structured_output(args.remote.output, &response)
            }
        },
        UserSubcommand::Create(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "create user",
                create_user,
                management_proto::CreateUserRequest {
                    username: args.username,
                    email: args.email.unwrap_or_default(),
                    role: args.role.to_proto(),
                    status: args.status.to_proto(),
                    password: args.password.unwrap_or_default(),
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Delete(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let (user_id, username) = args.user.to_management_selector()?;
            let response = management_unary_call!(
                session,
                "delete user",
                delete_user,
                management_proto::DeleteUserRequest { user_id, username }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Restore(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let (user_id, username) = args.user.to_management_selector()?;
            let response = management_unary_call!(
                session,
                "restore user",
                restore_user,
                management_proto::RestoreUserRequest {
                    user_id,
                    username,
                    ignore_identity_conflicts: args.ignore_identity_conflicts,
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Ban(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let (user_id, username) = args.user.to_management_selector()?;
            let response = management_unary_call!(
                session,
                "ban user",
                ban_user,
                management_proto::BanUserRequest {
                    user_id,
                    username,
                    reason: args.reason.unwrap_or_default(),
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Unban(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let (user_id, username) = args.user.to_management_selector()?;
            let response = management_unary_call!(
                session,
                "unban user",
                unban_user,
                management_proto::UnbanUserRequest { user_id, username }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Bans(command) => match command.command {
            UserBansSubcommand::List(args) => {
                super::ban::execute_ban_records_list(
                    &args.remote,
                    synctv_proto::admin::BanTargetType::User as i32,
                    args.active,
                    args.user_id.unwrap_or_default(),
                    String::new(),
                    args.page,
                    args.page_size,
                )
                .await
            }
        },
        UserSubcommand::SetRole(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let role = args.resolved_role();
            let (user_id, username) = args.user.to_management_selector()?;
            let response = management_unary_call!(
                session,
                "update user role",
                update_user_role,
                management_proto::UpdateUserRoleRequest {
                    user_id,
                    username,
                    role: role.to_proto(),
                }
            )?;
            args.remote.print_output(&UserMutationCliOutput {
                success: true,
                user: Some(response),
            })
        }
        UserSubcommand::SetPassword(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let (user_id, username) = args.user.to_management_selector()?;
            let response = management_unary_call!(
                session,
                "set user password",
                set_user_password,
                management_proto::SetUserPasswordRequest {
                    user_id,
                    username,
                    password: args.password,
                    reason: args.reason.unwrap_or_default(),
                }
            )?;
            args.remote.print_output(&UserMutationCliOutput {
                success: response.success,
                user: response.user,
            })
        }
        UserSubcommand::SetUsername(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let (user_id, username) = args.user.to_management_selector()?;
            let response = management_unary_call!(
                session,
                "update user username",
                update_user_username,
                management_proto::UpdateUserUsernameRequest {
                    user_id,
                    username,
                    new_username: args.new_username,
                }
            )?;
            args.remote.print_output(&UserMutationCliOutput {
                success: true,
                user: Some(response),
            })
        }
        UserSubcommand::Rooms(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let (user_id, username) = args.user.to_management_selector()?;
            let response = management_unary_call!(
                session,
                "get user rooms",
                get_user_rooms,
                management_proto::GetUserRoomsRequest {
                    user_id,
                    username,
                    page: args.page,
                    page_size: args.page_size,
                    status: args.status.map_or(
                        synctv_proto::common::RoomStatus::Unspecified as i32,
                        CliRoomStatus::to_proto,
                    ),
                    search: args.search.unwrap_or_default(),
                    is_banned: args.is_banned,
                    sort_by: args.sort_by.map_or(
                        management_proto::RoomListSortBy::CreatedAt as i32,
                        CliRoomSortField::to_proto,
                    ),
                    sort_direction: args.sort_dir.to_proto(),
                }
            )?;
            args.remote.print_output(&response)
        }
        UserSubcommand::Admin(admin_command) => match admin_command.command {
            UserAdminSubcommand::Grant(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let (user_id, username) = args.user.to_management_selector()?;
                let response = management_unary_call!(
                    session,
                    "add admin",
                    add_admin,
                    management_proto::AddAdminRequest { user_id, username }
                )?;
                args.remote.print_output(&response)
            }
            UserAdminSubcommand::Revoke(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let (user_id, username) = args.user.to_management_selector()?;
                let response = management_unary_call!(
                    session,
                    "remove admin",
                    remove_admin,
                    management_proto::RemoveAdminRequest { user_id, username }
                )?;
                args.remote.print_output(&response)
            }
            UserAdminSubcommand::List(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "list admins",
                    list_admins,
                    management_proto::ListAdminsRequest {
                        page: args.page,
                        page_size: args.page_size,
                        search: args.search.unwrap_or_default(),
                        sort_by: args.sort_by.map_or(
                            management_proto::UserListSortBy::CreatedAt as i32,
                            CliUserSortField::to_proto,
                        ),
                        sort_direction: args.sort_dir.to_proto(),
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
        UserSubcommand::Batch(batch_command) => match batch_command.command {
            UserBatchSubcommand::Ban(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "batch ban users",
                    batch_ban_users,
                    management_proto::BatchBanUsersRequest {
                        users: batch_user_refs_to_proto(args.usernames, args.user_ids),
                        reason: args.reason.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            UserBatchSubcommand::Delete(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "batch delete users",
                    batch_delete_users,
                    management_proto::BatchDeleteUsersRequest {
                        users: batch_user_refs_to_proto(args.usernames, args.user_ids),
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
    }
}
