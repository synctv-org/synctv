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
                        shared: false,
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
            let (session, actor_user_id) = connect_provider_actor_access(&args).await?;
            let response = management_unary_call!(
                session,
                "bilibili logout",
                bilibili_logout,
                management_proto::BilibiliLogoutRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::bilibili::LogoutRequest {}),
                }
            )?;
            args.remote.print_output(&response)
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
        ProviderBilibiliSubcommand::LiveAreas(args) => execute_bilibili_live_areas(args).await,
        ProviderBilibiliSubcommand::FavoriteFolders(args) => {
            execute_bilibili_favorite_folders(args).await
        }
        ProviderBilibiliSubcommand::FollowedPgc(args) => execute_bilibili_followed_pgc(args).await,
        ProviderBilibiliSubcommand::History(args) => execute_bilibili_history(args).await,
        ProviderBilibiliSubcommand::PgcTimeline(args) => execute_bilibili_pgc_timeline(args).await,
        ProviderBilibiliSubcommand::PgcSeasons(args) => execute_bilibili_pgc_seasons(args).await,
    }
}

pub(crate) async fn execute_bilibili_live_areas(
    args: ProviderBilibiliListLiveAreasArgs,
) -> Result<()> {
    provider_call!(
        args,
        bilibili_list_live_areas,
        BilibiliListLiveAreasRequest,
        synctv_proto::providers::bilibili::ListLiveAreasRequest {
            instance_name: provider_service_instance_name(&args.instance),
            shared: false
        }
    )
}

pub(crate) async fn execute_bilibili_favorite_folders(
    args: ProviderBilibiliFavoriteFoldersArgs,
) -> Result<()> {
    provider_call!(
        args,
        bilibili_list_favorite_folders,
        BilibiliListFavoriteFoldersRequest,
        synctv_proto::providers::bilibili::ListFavoriteFoldersRequest {
            instance_name: provider_service_instance_name(&args.instance),
            shared: false
        }
    )
}

pub(crate) async fn execute_bilibili_followed_pgc(
    args: ProviderBilibiliFollowedPgcArgs,
) -> Result<()> {
    provider_call!(
        args,
        bilibili_list_followed_pgc,
        BilibiliListFollowedPgcRequest,
        synctv_proto::providers::bilibili::ListFollowedPgcRequest {
            instance_name: provider_service_instance_name(&args.instance),
            r#type: args.r#type.to_proto(),
            page: args.page,
            page_size: args.page_size,
            shared: false
        }
    )
}

pub(crate) async fn execute_bilibili_history(args: ProviderBilibiliHistoryArgs) -> Result<()> {
    provider_call!(
        args,
        bilibili_list_history,
        BilibiliListHistoryRequest,
        synctv_proto::providers::bilibili::ListHistoryRequest {
            r#type: args.r#type.to_proto(),
            cursor: args.cursor,
            page_size: args.page_size,
            instance_name: provider_service_instance_name(&args.instance),
            shared: false
        }
    )
}

pub(crate) async fn execute_bilibili_pgc_timeline(
    args: ProviderBilibiliPgcTimelineArgs,
) -> Result<()> {
    provider_call!(
        args,
        bilibili_list_pgc_timeline,
        BilibiliListPgcTimelineRequest,
        synctv_proto::providers::bilibili::ListPgcTimelineRequest {
            r#type: args.r#type.to_proto(),
            before_days: args.before_days,
            after_days: args.after_days,
            instance_name: provider_service_instance_name(&args.instance),
            shared: false
        }
    )
}

pub(crate) async fn execute_bilibili_pgc_seasons(
    args: ProviderBilibiliPgcSeasonsArgs,
) -> Result<()> {
    provider_call!(
        args,
        bilibili_list_pgc_seasons,
        BilibiliListPgcSeasonsRequest,
        synctv_proto::providers::bilibili::ListPgcSeasonsRequest {
            r#type: args.r#type.to_proto(),
            page: args.page,
            page_size: args.page_size,
            order: args.order.to_proto(),
            ascending: args.ascending,
            finished: args.finished,
            area: args.area,
            year: args.year,
            style_id: args.style_id,
            instance_name: provider_service_instance_name(&args.instance),
            shared: false
        }
    )
}
