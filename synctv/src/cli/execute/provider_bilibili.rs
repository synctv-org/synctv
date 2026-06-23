use super::*;

pub(super) async fn execute_provider_bilibili(command: ProviderBilibiliCommand) -> Result<()> {
    match command.command {
        ProviderBilibiliSubcommand::Parse(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili parse",
                bilibili_parse,
                management_proto::BilibiliParseRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::ParseRequest {
                        url: args.url,
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderBilibiliSubcommand::LoginQr(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili login qr",
                bilibili_login_qr,
                management_proto::BilibiliLoginQrRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::LoginQrRequest {
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderBilibiliSubcommand::CheckQr(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili check qr",
                bilibili_check_qr,
                management_proto::BilibiliCheckQrRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::CheckQrRequest {
                        key: args.key,
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderBilibiliSubcommand::StartSmsLogin(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili start sms login",
                bilibili_start_sms_login,
                management_proto::BilibiliStartSmsLoginRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::StartSmsLoginRequest {
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderBilibiliSubcommand::SendSms(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili send sms",
                bilibili_send_sms,
                management_proto::BilibiliSendSmsRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::SendSmsRequest {
                        session_token: args.session_token,
                        phone: args.phone,
                        validate: args.validate,
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderBilibiliSubcommand::LoginSms(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili login sms",
                bilibili_login_sms,
                management_proto::BilibiliLoginSmsRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::LoginSmsRequest {
                        session_token: args.session_token,
                        code: args.code,
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderBilibiliSubcommand::Me(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili me",
                bilibili_get_user_info,
                management_proto::BilibiliGetUserInfoRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::UserInfoRequest {
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderBilibiliSubcommand::Logout(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili logout",
                bilibili_logout,
                management_proto::BilibiliLogoutRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::LogoutRequest {
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderBilibiliSubcommand::Binds(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "bilibili get binds",
                bilibili_get_binds,
                management_proto::BilibiliGetBindsRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::GetBindsRequest {
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
    }
}
