use super::*;

pub(super) async fn execute_settings(settings_command: SettingsCommand) -> Result<()> {
    let SettingsCommand { command } = settings_command;
    match command {
        SettingsSubcommand::List(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "get settings",
                get_settings,
                management_proto::GetSettingsRequest {}
            )?;
            args.remote.print_output(&response)
        }
        SettingsSubcommand::Get(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "get settings group",
                get_settings_group,
                management_proto::GetSettingsGroupRequest { group: args.group }
            )?;
            let group = response.group.ok_or_else(|| {
                anyhow!("management settings group response did not include group")
            })?;
            args.remote.print_output(&group)
        }
        SettingsSubcommand::Update(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let settings = parse_setting_entries(&args.entries)?;
            let response = management_unary_call!(
                session,
                "update settings",
                update_settings,
                management_proto::UpdateSettingsRequest {
                    group: args.group,
                    settings,
                }
            )?;
            let group = response.group.ok_or_else(|| {
                anyhow!("management update settings response did not include group")
            })?;
            args.remote.print_output(&group)
        }
        SettingsSubcommand::TestEmail(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "send test email",
                send_test_email,
                management_proto::SendTestEmailRequest { to: args.to }
            )?;
            args.remote.print_output(&response)
        }
    }
}
