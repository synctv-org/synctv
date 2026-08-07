use super::*;

pub(super) async fn execute_ban(ban_command: BanCommand) -> Result<()> {
    match ban_command.command {
        BanSubcommand::List(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "list ban records",
                list_ban_records,
                management_proto::ListBanRecordsRequest {
                    page: args.page,
                    page_size: args.page_size,
                    target_type: args.target.map_or(
                        synctv_proto::admin::BanTargetType::Unspecified as i32,
                        CliBanTarget::to_proto,
                    ),
                    active: args.active,
                    user_id: args.user_id.unwrap_or_default(),
                    room_id: args.room_id.unwrap_or_default(),
                }
            )?;
            args.remote.print_output(&response)
        }
    }
}
