use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use synctv_management::proto as management_proto;

use crate::admin_client::RemoteAdminSession;
use crate::app::Application;
use crate::path_util::absolute_display_path;

use super::args::*;
use super::commands::*;
use super::completion::execute_completion;
use super::context::{CliConfigContext, RemoteCliContext};
use super::output::{
    config_json_for_display, mask_connection_url, print_humanized_structured_output, print_json,
    print_toml, print_yaml, redact_config_for_display, ConfigOutputFormat, RemoteOutputFormat,
};
use super::output_dto::{
    GetPlaybackCliOutput, KickStreamCliOutput, PlaybackPullUrlCliOutput, PlaybackStartCliOutput,
    PlaybackStopCliOutput, UserMutationCliOutput,
};

const MANAGEMENT_UNARY_RPC_TIMEOUT: Duration = Duration::from_secs(30);
const MANAGEMENT_STOP_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

macro_rules! management_unary_call {
    ($session:expr, $operation:expr, $method:ident, $request:expr) => {{
        let mut client = $session.management_client();
        management_unary_response($operation, client.$method($request)).await
    }};
}

macro_rules! provider_call {
    ($args:expr, $method:ident, $wrapper:ident, $request:expr) => {{
        let access = &$args.access;
        let remote = &access.remote;
        let (session, actor) = connect_provider_actor_access(access).await?;
        let request = $request;
        let response = management_unary_call!(
            session,
            stringify!($method),
            $method,
            management_proto::$wrapper {
                actor: Some(actor),
                request: Some(request),
            }
        )?;
        remote.print_output(&response)
    }};
}

mod ban;
mod config;
mod db;
mod media;
mod media_provider;
mod playlist;
mod playlist_provider;
mod provider;
mod provider_alist;
mod provider_bilibili;
mod provider_douyin;
mod provider_emby;
mod provider_instance;
mod provider_rtmp;
mod provider_services;
mod provider_tiktok;
mod provider_twitch;
mod review;
mod room;
mod room_playback;
mod serve;
mod settings;
mod slice_cache;
mod status;
mod stop;
mod system;
mod user;

use ban::execute_ban;
use config::execute_config;
use db::execute_db;
use media::execute_media;
use media_provider::execute_media_provider;
use playlist::execute_playlist;
use playlist_provider::execute_playlist_provider;
use provider::execute_provider;
use provider_alist::execute_provider_alist;
use provider_bilibili::execute_provider_bilibili;
use provider_douyin::execute_provider_douyin;
use provider_emby::execute_provider_emby;
use provider_instance::execute_provider_instance;
use provider_rtmp::execute_provider_rtmp;
use provider_services::*;
use provider_tiktok::execute_provider_tiktok;
use provider_twitch::execute_provider_twitch;
use review::execute_review;
use room::execute_room;
use room_playback::execute_room_playback_state_update;
use serve::{execute_serve, local_api_probe_address};
use settings::execute_settings;
use slice_cache::execute_slice_cache;
use status::execute_status;
use stop::execute_stop;
use system::execute_system;
use user::execute_user;

#[cfg(test)]
pub(in crate::cli) use db::{database_summary, DatabaseCliOutput};
#[cfg(test)]
pub(in crate::cli) use serve::switch_process_working_dir_to_data_dir;
#[cfg(test)]
pub(in crate::cli) use stop::{
    stop_stream_disconnect_can_be_treated_as_success, stop_stream_end_can_be_treated_as_success,
    synthesize_stop_completion_if_needed, StopServerEventOutput, StopServerOutput,
};

pub async fn execute(cli: Cli) -> Result<()> {
    let cli = apply_root_global_overrides(cli);
    match cli.command {
        Commands::Serve(serve) => Box::pin(execute_serve(serve)).await,
        Commands::Stop(stop) => execute_stop(stop).await,
        Commands::Config(config) => execute_config(config),
        Commands::Db(db) => execute_db(db).await,
        Commands::User(user) => execute_user(user).await,
        Commands::Room(room) => Box::pin(execute_room(room)).await,
        Commands::Review(review) => execute_review(review).await,
        Commands::Ban(ban) => execute_ban(ban).await,
        Commands::Playlist(playlist) => execute_playlist(playlist).await,
        Commands::Media(media) => execute_media(media).await,
        Commands::Provider(provider) => execute_provider(provider).await,
        Commands::ProviderInstance(provider_instance) => {
            execute_provider_instance(provider_instance).await
        }
        Commands::Settings(settings) => execute_settings(settings).await,
        Commands::System(system) => execute_system(system).await,
        Commands::SliceCache(slice_cache) => execute_slice_cache(slice_cache).await,
        Commands::Status(args) => execute_status(args).await,
        Commands::Completion(args) => execute_completion(&args),
        Commands::Version => {
            println!("{}", version_string());
            Ok(())
        }
    }
}

pub(in crate::cli) fn apply_root_global_overrides(mut cli: Cli) -> Cli {
    let root = cli.global.clone();
    match &mut cli.command {
        Commands::Serve(args) => {
            args.global = args.global.merged_with_parent(&root);
        }
        Commands::Stop(args) => merge_remote_access_args(&mut args.remote, &root),
        Commands::Config(args) => {
            args.global = args.global.merged_with_parent(&root);
        }
        Commands::Db(args) => {
            args.global = args.global.merged_with_parent(&root);
        }
        Commands::User(command) => merge_user_command_globals(command, &root),
        Commands::Room(command) => merge_room_command_globals(command, &root),
        Commands::Review(command) => merge_review_command_globals(command, &root),
        Commands::Ban(command) => merge_ban_command_globals(command, &root),
        Commands::Playlist(command) => merge_playlist_command_globals(command, &root),
        Commands::Media(command) => merge_media_command_globals(command, &root),
        Commands::ProviderInstance(command) => {
            merge_provider_instance_command_globals(command, &root);
        }
        Commands::Provider(command) => merge_provider_command_globals(command, &root),
        Commands::Settings(command) => merge_settings_command_globals(command, &root),
        Commands::System(command) => merge_system_command_globals(command, &root),
        Commands::SliceCache(command) => merge_slice_cache_command_globals(command, &root),
        Commands::Status(args) => merge_remote_access_args(&mut args.remote, &root),
        Commands::Completion(_) | Commands::Version => {}
    }
    cli
}

fn merge_remote_access_args(remote: &mut RemoteAccessArgs, root: &GlobalConfigArgs) {
    remote.global = remote.global.merged_with_parent(root);
}

fn merge_review_command_globals(command: &mut ReviewCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        ReviewSubcommand::UserRegistration(command) => match &mut command.command {
            ReviewUserRegistrationSubcommand::List(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            ReviewUserRegistrationSubcommand::Approve(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            ReviewUserRegistrationSubcommand::Reject(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
        },
        ReviewSubcommand::RoomCreation(command) => match &mut command.command {
            ReviewRoomCreationSubcommand::List(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            ReviewRoomCreationSubcommand::Approve(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            ReviewRoomCreationSubcommand::Reject(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
        },
        ReviewSubcommand::RoomJoin(command) => match &mut command.command {
            ReviewRoomJoinSubcommand::List(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            ReviewRoomJoinSubcommand::Approve(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            ReviewRoomJoinSubcommand::Reject(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
        },
    }
}

fn merge_ban_command_globals(command: &mut BanCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        BanSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
    }
}

fn merge_room_scoped_remote_args(room: &mut RoomScopedRemoteArgs, root: &GlobalConfigArgs) {
    merge_remote_access_args(&mut room.remote, root);
}

fn merge_user_command_globals(command: &mut UserCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        UserSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::Get(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::Create(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::Delete(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::Restore(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::Ban(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::Unban(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::SetRole(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::SetPassword(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::SetUsername(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::Rooms(args) => merge_remote_access_args(&mut args.remote, root),
        UserSubcommand::Preferences(command) => match &mut command.command {
            UserPreferencesSubcommand::Get(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            UserPreferencesSubcommand::Set(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
        },
        UserSubcommand::Admin(command) => match &mut command.command {
            UserAdminSubcommand::Grant(args) => merge_remote_access_args(&mut args.remote, root),
            UserAdminSubcommand::Revoke(args) => merge_remote_access_args(&mut args.remote, root),
            UserAdminSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
        },
        UserSubcommand::Batch(command) => match &mut command.command {
            UserBatchSubcommand::Ban(args) => merge_remote_access_args(&mut args.remote, root),
            UserBatchSubcommand::Delete(args) => merge_remote_access_args(&mut args.remote, root),
        },
    }
}

fn merge_room_command_globals(command: &mut RoomCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        RoomSubcommand::Create(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::Get(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::TransferOwner(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::Favorite(command) => match &mut command.command {
            RoomFavoriteSubcommand::Add(args) | RoomFavoriteSubcommand::Remove(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            RoomFavoriteSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
        },
        RoomSubcommand::SetPassword(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::Ban(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::Unban(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::Delete(args) => merge_remote_access_args(&mut args.remote, root),
        RoomSubcommand::Settings(command) => match &mut command.command {
            RoomSettingsSubcommand::Get(args) => merge_remote_access_args(&mut args.remote, root),
            RoomSettingsSubcommand::Update(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            RoomSettingsSubcommand::Reset(args) => merge_remote_access_args(&mut args.remote, root),
        },
        RoomSubcommand::Category(command) => match &mut command.command {
            RoomCategorySubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
            RoomCategorySubcommand::Upsert(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            RoomCategorySubcommand::Delete(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
        },
        RoomSubcommand::Label(command) => match &mut command.command {
            RoomLabelSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
            RoomLabelSubcommand::Upsert(args) => merge_remote_access_args(&mut args.remote, root),
            RoomLabelSubcommand::Delete(args) => merge_remote_access_args(&mut args.remote, root),
        },
        RoomSubcommand::Taxonomy(command) => match &mut command.command {
            RoomTaxonomySubcommand::Set(args) => merge_remote_access_args(&mut args.remote, root),
        },
        RoomSubcommand::Chat(command) => match &mut command.command {
            RoomChatSubcommand::Search(args) => merge_room_scoped_remote_args(&mut args.room, root),
        },
        RoomSubcommand::Member(command) => match &mut command.command {
            RoomMemberSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
            RoomMemberSubcommand::Add(args) => merge_room_scoped_remote_args(&mut args.room, root),
            RoomMemberSubcommand::SetRemarkName(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            RoomMemberSubcommand::SetDisplayTag(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            RoomMemberSubcommand::SetPermissions(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            RoomMemberSubcommand::Kick(args) => merge_room_scoped_remote_args(&mut args.room, root),
        },
        RoomSubcommand::Playback(command) => match &mut command.command {
            RoomPlaybackSubcommand::Get(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            RoomPlaybackSubcommand::Start(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            RoomPlaybackSubcommand::Play(args) | RoomPlaybackSubcommand::Pause(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            RoomPlaybackSubcommand::Seek(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            RoomPlaybackSubcommand::Speed(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            RoomPlaybackSubcommand::Stop(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
        },
        RoomSubcommand::Stream(command) => match &mut command.command {
            RoomStreamSubcommand::List(args) => merge_room_scoped_remote_args(&mut args.room, root),
            RoomStreamSubcommand::Kick(args) => merge_room_scoped_remote_args(&mut args.room, root),
        },
        RoomSubcommand::Batch(command) => match &mut command.command {
            RoomBatchSubcommand::Ban(args) => merge_remote_access_args(&mut args.remote, root),
            RoomBatchSubcommand::Delete(args) => merge_remote_access_args(&mut args.remote, root),
        },
    }
}

fn merge_playlist_command_globals(command: &mut PlaylistCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        PlaylistSubcommand::List(args) => merge_room_scoped_remote_args(&mut args.room, root),
        PlaylistSubcommand::Get(args) => merge_room_scoped_remote_args(&mut args.room, root),
        PlaylistSubcommand::Create(args) => merge_room_scoped_remote_args(&mut args.room, root),
        PlaylistSubcommand::Update(args) => merge_room_scoped_remote_args(&mut args.room, root),
        PlaylistSubcommand::Move(args) => merge_room_scoped_remote_args(&mut args.room, root),
        PlaylistSubcommand::Delete(args) => merge_room_scoped_remote_args(&mut args.room, root),
        PlaylistSubcommand::Provider(command) => match &mut command.command {
            PlaylistProviderSubcommand::Alist(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            PlaylistProviderSubcommand::Emby(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
        },
    }
}

fn merge_media_command_globals(command: &mut MediaCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        MediaSubcommand::List(args) => merge_room_scoped_remote_args(&mut args.room, root),
        MediaSubcommand::Add(args) => merge_room_scoped_remote_args(&mut args.room, root),
        MediaSubcommand::AddUrl(args) => merge_room_scoped_remote_args(&mut args.room, root),
        MediaSubcommand::Update(args) => merge_room_scoped_remote_args(&mut args.room, root),
        MediaSubcommand::Delete(args) => merge_room_scoped_remote_args(&mut args.room, root),
        MediaSubcommand::Move(args) => merge_room_scoped_remote_args(&mut args.room, root),
        MediaSubcommand::Provider(command) => match &mut command.command {
            MediaProviderSubcommand::Alist(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            MediaProviderSubcommand::Emby(args) => {
                merge_room_scoped_remote_args(&mut args.room, root);
            }
            MediaProviderSubcommand::Bilibili(command) => match &mut command.command {
                MediaProviderBilibiliSubcommand::Video(args) => {
                    merge_room_scoped_remote_args(&mut args.room, root);
                }
                MediaProviderBilibiliSubcommand::Pgc(args) => {
                    merge_room_scoped_remote_args(&mut args.room, root);
                }
                MediaProviderBilibiliSubcommand::Live(args) => {
                    merge_room_scoped_remote_args(&mut args.room, root);
                }
            },
        },
    }
}

#[allow(clippy::match_same_arms)]
fn merge_provider_command_globals(command: &mut ProviderCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        ProviderSubcommand::Backends(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Alist(command) => match &mut command.command {
            ProviderAlistSubcommand::Login(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderAlistSubcommand::List(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderAlistSubcommand::Search(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderAlistSubcommand::Me(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderAlistSubcommand::Logout(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderAlistSubcommand::Binds(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Emby(command) => match &mut command.command {
            ProviderEmbySubcommand::Login(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderEmbySubcommand::List(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderEmbySubcommand::Me(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderEmbySubcommand::Logout(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderEmbySubcommand::Binds(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Bilibili(command) => match &mut command.command {
            ProviderBilibiliSubcommand::Parse(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::LoginQr(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::CheckQr(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::StartSmsLogin(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::SendSms(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::LoginSms(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::Me(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::Logout(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
            ProviderBilibiliSubcommand::Binds(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::LiveAreas(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::FavoriteFolders(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::FollowedPgc(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::History(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::PgcTimeline(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::PgcSeasons(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Douyin(command) => match &mut command.command {
            ProviderDouyinSubcommand::Bind(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderDouyinSubcommand::Binds(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderDouyinSubcommand::Unbind(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderDouyinSubcommand::Resolve(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderDouyinSubcommand::Posts(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Tiktok(command) => match &mut command.command {
            ProviderTikTokSubcommand::Bind(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderTikTokSubcommand::Binds(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderTikTokSubcommand::Unbind(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderTikTokSubcommand::Resolve(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderTikTokSubcommand::User(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderTikTokSubcommand::Posts(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Twitch(command) => match &mut command.command {
            ProviderTwitchSubcommand::Bind(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderTwitchSubcommand::Binds(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderTwitchSubcommand::Unbind(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderTwitchSubcommand::Resolve(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderTwitchSubcommand::Items(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderTwitchSubcommand::FollowedLive(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderTwitchSubcommand::CategoryStreams(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderTwitchSubcommand::TopCategories(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderTwitchSubcommand::SearchLive(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderTwitchSubcommand::Schedule(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Rtmp(command) => match &mut command.command {
            ProviderRtmpSubcommand::CreatePublishKey(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderRtmpSubcommand::GetStreamInfo(args) => {
                merge_remote_access_args(&mut args.remote, root);
            }
        },
        ProviderSubcommand::Acfun(command) => match &mut command.command {
            ProviderAcfunSubcommand::Resolve(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Cctv(command) => match &mut command.command {
            ProviderCctvSubcommand::Resolve(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Douyu(command) => match &mut command.command {
            ProviderDouyuSubcommand::Resolve(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Huya(command) => match &mut command.command {
            ProviderHuyaSubcommand::Resolve(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Youtube(command) => match &mut command.command {
            ProviderYoutubeSubcommand::Bind(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderYoutubeSubcommand::Binds(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderYoutubeSubcommand::Unbind(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderYoutubeSubcommand::Resolve(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Cloudreve(command) => match &mut command.command {
            ProviderCloudreveSubcommand::Login(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderCloudreveSubcommand::List(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderCloudreveSubcommand::Search(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderCloudreveSubcommand::Me(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderCloudreveSubcommand::Logout(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderCloudreveSubcommand::Binds(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Fnos(command) => match &mut command.command {
            ProviderFnosSubcommand::Login(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderFnosSubcommand::List(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderFnosSubcommand::Libraries(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderFnosSubcommand::Items(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderFnosSubcommand::SetFavorite(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderFnosSubcommand::SetWatched(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderFnosSubcommand::ServerInfo(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderFnosSubcommand::Logout(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderFnosSubcommand::Binds(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Nextcloud(command) => match &mut command.command {
            ProviderNextcloudSubcommand::Login(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderNextcloudSubcommand::StartLoginFlow(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderNextcloudSubcommand::PollLoginFlow(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderNextcloudSubcommand::List(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderNextcloudSubcommand::Favorites(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderNextcloudSubcommand::Logout(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderNextcloudSubcommand::Binds(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Qnap(command) => match &mut command.command {
            ProviderQnapSubcommand::Login(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderQnapSubcommand::List(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderQnapSubcommand::Capabilities(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderQnapSubcommand::Logout(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderQnapSubcommand::Binds(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Seafile(command) => match &mut command.command {
            ProviderSeafileSubcommand::Login(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderSeafileSubcommand::UnlockLibrary(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderSeafileSubcommand::Repositories(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderSeafileSubcommand::List(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderSeafileSubcommand::Starred(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderSeafileSubcommand::Logout(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderSeafileSubcommand::Binds(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Synology(command) => match &mut command.command {
            ProviderSynologySubcommand::Login(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderSynologySubcommand::Files(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderSynologySubcommand::Libraries(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderSynologySubcommand::Movies(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderSynologySubcommand::TvShows(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderSynologySubcommand::Episodes(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderSynologySubcommand::HomeVideos(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderSynologySubcommand::Recordings(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderSynologySubcommand::Logout(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderSynologySubcommand::Binds(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
        ProviderSubcommand::Truenas(command) => match &mut command.command {
            ProviderTruenasSubcommand::Login(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderTruenasSubcommand::List(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderTruenasSubcommand::Logout(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderTruenasSubcommand::Binds(args) => {
                merge_remote_access_args(&mut args.access.remote, root);
            }
        },
    }
}

fn merge_provider_instance_command_globals(
    command: &mut ProviderInstanceCommand,
    root: &GlobalConfigArgs,
) {
    match &mut command.command {
        ProviderInstanceSubcommand::Available(args) => {
            merge_remote_access_args(&mut args.remote, root);
        }
        ProviderInstanceSubcommand::List(args) => {
            merge_remote_access_args(&mut args.remote, root);
        }
        ProviderInstanceSubcommand::Create(args) => {
            merge_remote_access_args(&mut args.remote, root);
        }
        ProviderInstanceSubcommand::Update(args) => {
            merge_remote_access_args(&mut args.remote, root);
        }
        ProviderInstanceSubcommand::Delete(args)
        | ProviderInstanceSubcommand::Reconnect(args)
        | ProviderInstanceSubcommand::Enable(args)
        | ProviderInstanceSubcommand::Disable(args) => {
            merge_remote_access_args(&mut args.remote, root);
        }
    }
}

fn merge_settings_command_globals(command: &mut SettingsCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        SettingsSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
        SettingsSubcommand::Get(args) => merge_remote_access_args(&mut args.remote, root),
        SettingsSubcommand::Update(args) => merge_remote_access_args(&mut args.remote, root),
        SettingsSubcommand::Export(args) => merge_remote_access_args(&mut args.remote, root),
        SettingsSubcommand::Import(args) => merge_remote_access_args(&mut args.remote, root),
        SettingsSubcommand::TestEmail(args) => merge_remote_access_args(&mut args.remote, root),
    }
}

fn merge_system_command_globals(command: &mut SystemCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        SystemSubcommand::Stats(args) => merge_remote_access_args(&mut args.remote, root),
        SystemSubcommand::Stream(command) => match &mut command.command {
            SystemStreamSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
            SystemStreamSubcommand::Kick(args) => merge_remote_access_args(&mut args.remote, root),
        },
    }
}

fn merge_slice_cache_command_globals(command: &mut SliceCacheCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        SliceCacheSubcommand::Stats(args) => merge_remote_access_args(&mut args.remote, root),
        SliceCacheSubcommand::Purge(args) => merge_remote_access_args(&mut args.remote, root),
        SliceCacheSubcommand::EvictExpired(args) => {
            merge_remote_access_args(&mut args.remote, root);
        }
    }
}

async fn management_unary_response<T>(
    operation: &'static str,
    future: impl std::future::Future<Output = std::result::Result<tonic::Response<T>, tonic::Status>>,
) -> Result<T> {
    management_unary_response_with_timeout(operation, MANAGEMENT_UNARY_RPC_TIMEOUT, future).await
}

pub(in crate::cli) async fn management_unary_response_with_timeout<T>(
    operation: &'static str,
    timeout: Duration,
    future: impl std::future::Future<Output = std::result::Result<tonic::Response<T>, tonic::Status>>,
) -> Result<T> {
    let response = tokio::time::timeout(timeout, future)
        .await
        .with_context(|| {
            format!(
                "management operation '{operation}' timed out after {}s",
                timeout.as_secs()
            )
        })?
        .map_err(|status| format_management_status_error(operation, &status))?;
    Ok(response.into_inner())
}

pub(in crate::cli) async fn management_stream_item<T>(
    operation: &'static str,
    timeout: Duration,
    future: impl std::future::Future<Output = std::result::Result<Option<T>, tonic::Status>>,
) -> Result<Option<T>> {
    tokio::time::timeout(timeout, future)
        .await
        .with_context(|| {
            format!(
                "management stream '{operation}' timed out after {}s",
                timeout.as_secs()
            )
        })?
        .map_err(|status| format_management_status_error(operation, &status))
}

pub(in crate::cli) fn format_management_status_error(
    operation: &'static str,
    status: &tonic::Status,
) -> anyhow::Error {
    let message = status.message().trim();
    let detail = if message.is_empty() {
        operation.to_string()
    } else {
        message.to_string()
    };

    let rendered = match status.code() {
        tonic::Code::InvalidArgument => {
            format!("management {operation} failed: invalid request: {detail}")
        }
        tonic::Code::NotFound | tonic::Code::AlreadyExists | tonic::Code::Unknown => {
            format!("management {operation} failed: {detail}")
        }
        tonic::Code::PermissionDenied => {
            format!("management {operation} failed: permission denied: {detail}")
        }
        tonic::Code::Unauthenticated => {
            format!("management {operation} failed: authentication failed: {detail}")
        }
        tonic::Code::Unavailable => {
            format!("management {operation} failed: service unavailable: {detail}")
        }
        tonic::Code::DeadlineExceeded => {
            format!("management {operation} failed: deadline exceeded: {detail}")
        }
        tonic::Code::Aborted => {
            format!("management {operation} failed: operation aborted: {detail}")
        }
        tonic::Code::ResourceExhausted => {
            format!("management {operation} failed: resource exhausted: {detail}")
        }
        tonic::Code::Internal => format!("management {operation} failed: internal error"),
        _ => format!("management {operation} failed: {}: {detail}", status.code()),
    };

    anyhow!(rendered)
}

pub(super) fn resolve_remote_endpoint(global: &GlobalConfigArgs) -> Option<String> {
    global
        .endpoint
        .as_deref()
        .map(str::to_string)
        .or_else(|| std::env::var("SYNCTV_MANAGEMENT_ENDPOINT").ok())
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

async fn connect_remote_access(args: &RemoteAccessArgs) -> Result<RemoteAdminSession> {
    let context = RemoteCliContext::new(args);
    context.initialize_output_state()?;
    let options = context.connection_options(args)?;
    let session = RemoteAdminSession::connect(options).await?;
    if args.global.verbose > 0 {
        eprintln!(
            "Connected to remote management endpoint {}",
            session.endpoint()
        );
    }
    Ok(session)
}

async fn connect_provider_actor_access(
    access: &ProviderServiceRemoteActorArgs,
) -> Result<(RemoteAdminSession, management_proto::UserRef)> {
    let session = connect_remote_access(&access.remote).await?;
    Ok((session, access.actor.to_management_proto()?))
}

pub(in crate::cli) fn batch_user_refs_to_proto(
    usernames: Vec<String>,
    user_ids: Vec<String>,
) -> Vec<management_proto::UserRef> {
    usernames
        .into_iter()
        .map(|username| management_proto::UserRef {
            value: Some(management_proto::user_ref::Value::Username(username)),
        })
        .chain(
            user_ids
                .into_iter()
                .map(|user_id| management_proto::UserRef {
                    value: Some(management_proto::user_ref::Value::UserId(user_id)),
                }),
        )
        .collect()
}

fn infer_cli_api_base_url(global: &GlobalConfigArgs) -> Option<String> {
    if global.endpoint.is_some() {
        return None;
    }
    let config = CliConfigContext::new(global.clone()).config().ok()?;
    Some(format!("http://{}", local_api_probe_address(&config)))
}

fn absolutize_cli_url(raw: &str, api_base_url: Option<&str>) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(parsed) = url::Url::parse(trimmed) {
        return Some(parsed.to_string());
    }

    let base = api_base_url?;
    let base = if base.ends_with('/') {
        base.to_string()
    } else {
        format!("{base}/")
    };
    let parsed_base = url::Url::parse(&base).ok()?;
    parsed_base.join(trimmed).ok().map(|url| url.to_string())
}

pub(in crate::cli) fn build_get_playback_cli_output(
    response: synctv_proto::client::GetPlaybackResponse,
    global: &GlobalConfigArgs,
) -> GetPlaybackCliOutput {
    let synctv_proto::client::GetPlaybackResponse {
        playback_state,
        playback,
    } = response;
    let api_base_url = infer_cli_api_base_url(global);
    let mut pull_urls = Vec::new();
    let mut default_pull_url = None;
    let mut default_absolute_pull_url = None;
    let mut hls_pull_url = None;
    let mut hls_absolute_pull_url = None;
    let mut flv_pull_url = None;
    let mut flv_absolute_pull_url = None;

    if let Some(playback) = playback.as_ref() {
        let mut modes = playback.playback_infos.iter().collect::<Vec<_>>();
        modes.sort_by_key(|(mode, _)| *mode);

        for (mode, info) in modes {
            for (index, playback_media) in info.medias.iter().enumerate() {
                let is_default = mode == &playback.default_mode
                    && i32::try_from(index)
                        .is_ok_and(|index| info.default_media_index.unwrap_or(0) == index);
                let absolute_url = absolutize_cli_url(&playback_media.url, api_base_url.as_deref());
                let output = PlaybackPullUrlCliOutput {
                    mode: mode.clone(),
                    format: playback_media.format.clone(),
                    name: playback_media.name.clone(),
                    url: playback_media.url.clone(),
                    absolute_url: absolute_url.clone(),
                    default: is_default,
                    headers: playback_media.headers.clone(),
                    expire_at: playback_media.expire_at,
                };

                if is_default {
                    default_pull_url = Some(output.url.clone());
                    default_absolute_pull_url.clone_from(&output.absolute_url);
                }

                match playback_media.format.as_str() {
                    "m3u8" if is_default || hls_pull_url.is_none() => {
                        hls_pull_url = Some(output.url.clone());
                        hls_absolute_pull_url.clone_from(&output.absolute_url);
                    }
                    "flv" if is_default || flv_pull_url.is_none() => {
                        flv_pull_url = Some(output.url.clone());
                        flv_absolute_pull_url.clone_from(&output.absolute_url);
                    }
                    _ => {}
                }

                pull_urls.push(output);
            }
        }
    }

    let default_mode = playback
        .as_ref()
        .map(|playback| playback.default_mode.clone())
        .filter(|mode| !mode.is_empty());

    GetPlaybackCliOutput {
        playback_state,
        playback,
        default_mode,
        pull_urls,
        default_pull_url,
        default_absolute_pull_url,
        hls_pull_url,
        hls_absolute_pull_url,
        flv_pull_url,
        flv_absolute_pull_url,
    }
}

fn normalized_optional_cli_value(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn provider_service_instance_name(args: &ProviderServiceInstanceArgs) -> String {
    provider_instance_name_string(args.instance_name.as_deref())
}

fn provider_instance_name_string(raw: Option<&str>) -> String {
    normalized_optional_cli_value(raw).unwrap_or_default()
}

fn parse_optional_room_settings_json(
    raw: Option<&str>,
) -> Result<Option<synctv_proto::client::RoomSettings>> {
    normalized_optional_cli_value(raw)
        .map(|raw| {
            let patch: synctv_proto::client::RoomSettingsPatch =
                serde_json::from_str(&raw).context("invalid room settings patch JSON")?;
            Ok(room_settings_patch_to_full_settings(patch))
        })
        .transpose()
}

pub(in crate::cli) fn room_settings_patch_to_full_settings(
    patch: synctv_proto::client::RoomSettingsPatch,
) -> synctv_proto::client::RoomSettings {
    let defaults = synctv_core::models::RoomSettings::default();
    let default_auto_play = defaults.auto_play.value;
    let auto_play_patch = patch.auto_play.unwrap_or_default();
    let auto_play_mode = auto_play_patch
        .mode
        .unwrap_or(match default_auto_play.mode {
            synctv_core::models::PlayMode::Sequential => synctv_proto::client::PlayMode::Sequential,
            synctv_core::models::PlayMode::RepeatOne => synctv_proto::client::PlayMode::RepeatOne,
            synctv_core::models::PlayMode::RepeatAll => synctv_proto::client::PlayMode::RepeatAll,
            synctv_core::models::PlayMode::Shuffle => synctv_proto::client::PlayMode::Shuffle,
        } as i32);

    synctv_proto::client::RoomSettings {
        allow_guest_join: patch
            .allow_guest_join
            .unwrap_or(defaults.allow_guest_join.0),
        max_members: patch.max_members.unwrap_or(defaults.max_members.0),
        require_approval: patch
            .require_approval
            .unwrap_or(defaults.require_approval.0),
        allow_auto_join: patch.allow_auto_join.unwrap_or(defaults.allow_auto_join.0),
        chat_enabled: patch.chat_enabled.unwrap_or(defaults.chat_enabled.0),
        voice_chat_enabled: patch
            .voice_chat_enabled
            .unwrap_or(defaults.voice_chat_enabled.0),
        p2p_media_enabled: patch
            .p2p_media_enabled
            .unwrap_or(defaults.p2p_media_enabled.0),
        auto_play: Some(synctv_proto::client::AutoPlaySettings {
            enabled: auto_play_patch.enabled.unwrap_or(default_auto_play.enabled),
            mode: auto_play_mode,
            delay: auto_play_patch.delay.unwrap_or(default_auto_play.delay),
        }),
        admin_added_permissions: patch
            .admin_added_permissions
            .unwrap_or(defaults.admin_added_permissions.0),
        admin_removed_permissions: patch
            .admin_removed_permissions
            .unwrap_or(defaults.admin_removed_permissions.0),
        member_added_permissions: patch
            .member_added_permissions
            .unwrap_or(defaults.member_added_permissions.0),
        member_removed_permissions: patch
            .member_removed_permissions
            .unwrap_or(defaults.member_removed_permissions.0),
        guest_added_permissions: patch
            .guest_added_permissions
            .unwrap_or(defaults.guest_added_permissions.0),
        guest_removed_permissions: patch
            .guest_removed_permissions
            .unwrap_or(defaults.guest_removed_permissions.0),
    }
}

fn parse_optional_provider_target_json(
    raw: Option<&str>,
) -> Result<Option<synctv_proto::client::ProviderTarget>> {
    normalized_optional_cli_value(raw)
        .map(|raw| serde_json::from_str(&raw).context("invalid provider target JSON"))
        .transpose()
}

fn parse_media_source_config_json(
    provider: CliSourceProvider,
    raw: &str,
) -> Result<synctv_proto::source_config::MediaSourceConfig> {
    media_source_config_json_to_proto(provider, raw)
}

fn parse_optional_playlist_source_config_json(
    provider: Option<CliSourceProvider>,
    raw: Option<&str>,
) -> Result<Option<synctv_proto::source_config::PlaylistSourceConfig>> {
    match (provider, raw) {
        (Some(provider), Some(raw)) => {
            playlist_source_config_json_to_proto(provider, raw).map(Some)
        }
        (None, Some(_)) => bail!("--source-provider is required with --source-config-json"),
        (_, None) => Ok(None),
    }
}

fn media_source_config_json_to_proto(
    provider: CliSourceProvider,
    raw: &str,
) -> Result<synctv_proto::source_config::MediaSourceConfig> {
    use synctv_proto::source_config::{
        media_source_config, AcFunMediaSourceConfig, AlistMediaSourceConfig,
        BilibiliMediaSourceConfig, CctvMediaSourceConfig, CloudreveMediaSourceConfig,
        DirectUrlMediaSourceConfig, DouyinMediaSourceConfig, DouyuMediaSourceConfig,
        EmbyMediaSourceConfig, FnosMediaSourceConfig, HuyaMediaSourceConfig,
        LiveProxyMediaSourceConfig, NextcloudMediaSourceConfig, QnapMediaSourceConfig,
        RtmpMediaSourceConfig, SeafileMediaSourceConfig, SynologyMediaSourceConfig,
        TikTokMediaSourceConfig, TrueNasMediaSourceConfig, TwitchMediaSourceConfig,
        YoutubeMediaSourceConfig,
    };

    let provider =
        match provider {
            CliSourceProvider::DirectUrl => {
                media_source_config::Provider::DirectUrl(parse_cli_json::<
                    DirectUrlMediaSourceConfig,
                >(
                    "directUrl media sourceConfig", raw
                )?)
            }
            CliSourceProvider::Bilibili => media_source_config::Provider::Bilibili(
                parse_cli_json::<BilibiliMediaSourceConfig>("bilibili media sourceConfig", raw)?,
            ),
            CliSourceProvider::Alist => {
                media_source_config::Provider::Alist(parse_cli_json::<AlistMediaSourceConfig>(
                    "alist media sourceConfig",
                    raw,
                )?)
            }
            CliSourceProvider::Emby => {
                media_source_config::Provider::Emby(parse_cli_json::<EmbyMediaSourceConfig>(
                    "emby media sourceConfig",
                    raw,
                )?)
            }
            CliSourceProvider::Rtmp => {
                media_source_config::Provider::Rtmp(parse_cli_json::<RtmpMediaSourceConfig>(
                    "rtmp media sourceConfig",
                    raw,
                )?)
            }
            CliSourceProvider::LiveProxy => {
                media_source_config::Provider::LiveProxy(parse_cli_json::<
                    LiveProxyMediaSourceConfig,
                >(
                    "liveProxy media sourceConfig", raw
                )?)
            }
            CliSourceProvider::Cloudreve => {
                media_source_config::Provider::Cloudreve(parse_cli_json::<
                    CloudreveMediaSourceConfig,
                >(
                    "cloudreve media sourceConfig", raw
                )?)
            }
            CliSourceProvider::Twitch => {
                media_source_config::Provider::Twitch(parse_cli_json::<TwitchMediaSourceConfig>(
                    "twitch media sourceConfig",
                    raw,
                )?)
            }
            CliSourceProvider::Youtube => {
                media_source_config::Provider::Youtube(parse_cli_json::<YoutubeMediaSourceConfig>(
                    "YouTube media sourceConfig",
                    raw,
                )?)
            }
            CliSourceProvider::Huya => {
                media_source_config::Provider::Huya(parse_cli_json::<HuyaMediaSourceConfig>(
                    "huya media sourceConfig",
                    raw,
                )?)
            }
            CliSourceProvider::Douyu => {
                media_source_config::Provider::Douyu(parse_cli_json::<DouyuMediaSourceConfig>(
                    "douyu media sourceConfig",
                    raw,
                )?)
            }
            CliSourceProvider::Douyin => {
                media_source_config::Provider::Douyin(parse_cli_json::<DouyinMediaSourceConfig>(
                    "Douyin media sourceConfig",
                    raw,
                )?)
            }
            CliSourceProvider::Tiktok => {
                media_source_config::Provider::Tiktok(parse_cli_json::<TikTokMediaSourceConfig>(
                    "TikTok media sourceConfig",
                    raw,
                )?)
            }
            CliSourceProvider::Acfun => {
                media_source_config::Provider::AcFun(parse_cli_json::<AcFunMediaSourceConfig>(
                    "AcFun media sourceConfig",
                    raw,
                )?)
            }
            CliSourceProvider::Cctv => {
                media_source_config::Provider::Cctv(parse_cli_json::<CctvMediaSourceConfig>(
                    "CCTV media sourceConfig",
                    raw,
                )?)
            }
            CliSourceProvider::Fnos => {
                media_source_config::Provider::Fnos(parse_cli_json::<FnosMediaSourceConfig>(
                    "FNOS media sourceConfig",
                    raw,
                )?)
            }
            CliSourceProvider::Qnap => {
                media_source_config::Provider::Qnap(parse_cli_json::<QnapMediaSourceConfig>(
                    "QNAP media sourceConfig",
                    raw,
                )?)
            }
            CliSourceProvider::Synology => media_source_config::Provider::Synology(
                parse_cli_json::<SynologyMediaSourceConfig>("Synology media sourceConfig", raw)?,
            ),
            CliSourceProvider::Nextcloud => {
                media_source_config::Provider::Nextcloud(parse_cli_json::<
                    NextcloudMediaSourceConfig,
                >(
                    "Nextcloud media sourceConfig", raw
                )?)
            }
            CliSourceProvider::Seafile => {
                media_source_config::Provider::Seafile(parse_cli_json::<SeafileMediaSourceConfig>(
                    "Seafile media sourceConfig",
                    raw,
                )?)
            }
            CliSourceProvider::Truenas => {
                media_source_config::Provider::Truenas(parse_cli_json::<TrueNasMediaSourceConfig>(
                    "TrueNAS media sourceConfig",
                    raw,
                )?)
            }
        };
    Ok(synctv_proto::source_config::MediaSourceConfig {
        provider: Some(provider),
    })
}

fn playlist_source_config_json_to_proto(
    provider: CliSourceProvider,
    raw: &str,
) -> Result<synctv_proto::source_config::PlaylistSourceConfig> {
    use synctv_proto::source_config::{
        playlist_source_config, AlistPlaylistSourceConfig, BilibiliPlaylistSourceConfig,
        CloudrevePlaylistSourceConfig, DouyinPlaylistSourceConfig, EmbyPlaylistSourceConfig,
        FnosPlaylistSourceConfig, NextcloudPlaylistSourceConfig, QnapPlaylistSourceConfig,
        SeafilePlaylistSourceConfig, SynologyPlaylistSourceConfig, TikTokPlaylistSourceConfig,
        TrueNasPlaylistSourceConfig, TwitchPlaylistSourceConfig, YoutubePlaylistSourceConfig,
    };

    let provider = match provider {
        CliSourceProvider::Bilibili => {
            playlist_source_config::Provider::Bilibili(parse_cli_json::<
                BilibiliPlaylistSourceConfig,
            >(
                "Bilibili playlist sourceConfig", raw
            )?)
        }
        CliSourceProvider::Alist => {
            playlist_source_config::Provider::Alist(parse_cli_json::<AlistPlaylistSourceConfig>(
                "alist playlist sourceConfig",
                raw,
            )?)
        }
        CliSourceProvider::Emby => {
            playlist_source_config::Provider::Emby(parse_cli_json::<EmbyPlaylistSourceConfig>(
                "emby playlist sourceConfig",
                raw,
            )?)
        }
        CliSourceProvider::Cloudreve => {
            playlist_source_config::Provider::Cloudreve(parse_cli_json::<
                CloudrevePlaylistSourceConfig,
            >(
                "cloudreve playlist sourceConfig", raw
            )?)
        }
        CliSourceProvider::Twitch => {
            playlist_source_config::Provider::Twitch(parse_cli_json::<TwitchPlaylistSourceConfig>(
                "twitch playlist sourceConfig",
                raw,
            )?)
        }
        CliSourceProvider::Youtube => playlist_source_config::Provider::Youtube(parse_cli_json::<
            YoutubePlaylistSourceConfig,
        >(
            "YouTube playlist sourceConfig",
            raw,
        )?),
        CliSourceProvider::Douyin => {
            playlist_source_config::Provider::Douyin(parse_cli_json::<DouyinPlaylistSourceConfig>(
                "Douyin playlist sourceConfig",
                raw,
            )?)
        }
        CliSourceProvider::Tiktok => {
            playlist_source_config::Provider::Tiktok(parse_cli_json::<TikTokPlaylistSourceConfig>(
                "TikTok playlist sourceConfig",
                raw,
            )?)
        }
        CliSourceProvider::Fnos => {
            playlist_source_config::Provider::Fnos(parse_cli_json::<FnosPlaylistSourceConfig>(
                "FNOS playlist sourceConfig",
                raw,
            )?)
        }
        CliSourceProvider::Qnap => {
            playlist_source_config::Provider::Qnap(parse_cli_json::<QnapPlaylistSourceConfig>(
                "QNAP playlist sourceConfig",
                raw,
            )?)
        }
        CliSourceProvider::Synology => {
            playlist_source_config::Provider::Synology(parse_cli_json::<
                SynologyPlaylistSourceConfig,
            >(
                "Synology playlist sourceConfig", raw
            )?)
        }
        CliSourceProvider::Nextcloud => {
            playlist_source_config::Provider::Nextcloud(parse_cli_json::<
                NextcloudPlaylistSourceConfig,
            >(
                "Nextcloud playlist sourceConfig", raw
            )?)
        }
        CliSourceProvider::Seafile => playlist_source_config::Provider::Seafile(parse_cli_json::<
            SeafilePlaylistSourceConfig,
        >(
            "Seafile playlist sourceConfig",
            raw,
        )?),
        CliSourceProvider::Truenas => playlist_source_config::Provider::Truenas(parse_cli_json::<
            TrueNasPlaylistSourceConfig,
        >(
            "TrueNAS playlist sourceConfig",
            raw,
        )?),
        other => bail!("{other:?} does not support playlist source_config"),
    };
    Ok(synctv_proto::source_config::PlaylistSourceConfig {
        provider: Some(provider),
    })
}

pub(in crate::cli) fn parse_cli_json<T>(label: &str, raw: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_str(raw).with_context(|| format!("Invalid {label} JSON"))
}

pub(in crate::cli) fn parse_masked_settings_request<T>(
    label: &str,
    request_json: Option<&str>,
    set: &[String],
    unset: &[String],
) -> Result<T>
where
    T: DeserializeOwned,
{
    if let Some(raw) = request_json {
        return parse_cli_json(label, raw);
    }
    if set.is_empty() && unset.is_empty() {
        bail!("provide at least one --set or --unset, or use --request-json");
    }

    let mut settings = serde_json::Map::new();
    let mut paths = Vec::with_capacity(set.len() + unset.len());
    let mut seen = BTreeSet::new();

    for assignment in set {
        let (path, raw_value) = assignment
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid --set '{assignment}'; expected PATH=VALUE"))?;
        register_mask_path(path, &mut paths, &mut seen)?;
        let mut value = serde_json::from_str(raw_value)
            .unwrap_or_else(|_| serde_json::Value::String(raw_value.to_string()));
        if let serde_json::Value::String(name) = &value {
            if let Some(enum_value) = cli_settings_enum_value(path, name) {
                value = serde_json::Value::Number(enum_value.into());
            }
        }
        if value.is_null() {
            bail!("--set '{path}' cannot use null; use --unset {path}");
        }
        insert_json_path(&mut settings, path, value)?;
    }
    for path in unset {
        register_mask_path(path, &mut paths, &mut seen)?;
    }

    serde_json::from_value(serde_json::json!({
        "settings": settings,
        "updateMask": paths.join(","),
    }))
    .with_context(|| format!("Invalid {label} values"))
}

fn cli_settings_enum_value(path: &str, raw: &str) -> Option<i32> {
    let value = raw.trim().to_ascii_uppercase().replace('-', "_");
    match path {
        "autoPlay.mode" => match value.as_str() {
            "UNSPECIFIED" | "PLAY_MODE_UNSPECIFIED" => Some(0),
            "SEQUENTIAL" | "PLAY_MODE_SEQUENTIAL" => Some(1),
            "REPEAT_ONE" | "PLAY_MODE_REPEAT_ONE" => Some(2),
            "REPEAT_ALL" | "PLAY_MODE_REPEAT_ALL" => Some(3),
            "SHUFFLE" | "PLAY_MODE_SHUFFLE" => Some(4),
            _ => None,
        },
        "roomCreation.passwordPolicy" => match value.as_str() {
            "UNSPECIFIED" | "ROOM_PASSWORD_POLICY_UNSPECIFIED" => Some(0),
            "OPTIONAL" | "ROOM_PASSWORD_POLICY_OPTIONAL" => Some(1),
            "REQUIRED" | "ROOM_PASSWORD_POLICY_REQUIRED" => Some(2),
            "FORBIDDEN" | "ROOM_PASSWORD_POLICY_FORBIDDEN" => Some(3),
            _ => None,
        },
        _ => None,
    }
}

fn register_mask_path<'a>(
    path: &'a str,
    paths: &mut Vec<&'a str>,
    seen: &mut BTreeSet<&'a str>,
) -> Result<()> {
    if path.is_empty() || path.split('.').any(str::is_empty) {
        bail!("settings paths must contain non-empty dot-separated field names");
    }
    if !seen.insert(path) {
        bail!("duplicate settings path '{path}'");
    }
    if let Some(conflict) = seen.iter().copied().find(|existing| {
        let existing = *existing;
        existing != path
            && (existing
                .strip_prefix(path)
                .is_some_and(|suffix| suffix.starts_with('.'))
                || path
                    .strip_prefix(existing)
                    .is_some_and(|suffix| suffix.starts_with('.')))
    }) {
        bail!("conflicting settings paths '{path}' and '{conflict}'");
    }
    paths.push(path);
    Ok(())
}

fn insert_json_path(
    object: &mut serde_json::Map<String, serde_json::Value>,
    path: &str,
    value: serde_json::Value,
) -> Result<()> {
    let mut segments = path.split('.');
    let first = segments
        .next()
        .ok_or_else(|| anyhow!("settings path is required"))?;
    insert_json_segments(object, first, &mut segments, value, path)
}

fn insert_json_segments<'a>(
    object: &mut serde_json::Map<String, serde_json::Value>,
    segment: &str,
    remaining: &mut impl Iterator<Item = &'a str>,
    value: serde_json::Value,
    path: &str,
) -> Result<()> {
    let Some(next) = remaining.next() else {
        object.insert(segment.to_string(), value);
        return Ok(());
    };
    let nested = object
        .entry(segment.to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow!("conflicting settings path '{path}'"))?;
    insert_json_segments(nested, next, remaining, value, path)
}

fn parse_cli_optional_json<T>(label: &str, raw: Option<&str>) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    raw.map(|value| parse_cli_json(label, value)).transpose()
}

fn optional_source_provider_to_proto_i32(provider: Option<CliSourceProvider>) -> i32 {
    provider.map_or(
        synctv_proto::source_config::SourceProvider::Unspecified as i32,
        CliSourceProvider::to_proto_i32,
    )
}

pub(in crate::cli) fn normalized_provider_types(providers: &[CliSourceProvider]) -> Vec<i32> {
    providers
        .iter()
        .copied()
        .map(CliSourceProvider::to_proto_i32)
        .collect()
}

pub fn version_string() -> String {
    format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod source_config_tests {
    use super::*;

    type TestResult<T = ()> = anyhow::Result<T>;

    #[test]
    fn parses_cloudreve_playlist_source_config() -> TestResult {
        let config = playlist_source_config_json_to_proto(
            CliSourceProvider::Cloudreve,
            r#"{"serverId":"cloudreve-server","path":"cloudreve://my/Shows"}"#,
        )?;

        let Some(synctv_proto::source_config::playlist_source_config::Provider::Cloudreve(config)) =
            config.provider
        else {
            return Err(anyhow::anyhow!(
                "Cloudreve playlist config did not produce its provider oneof"
            ));
        };
        assert_eq!(config.server_id, "cloudreve-server");
        assert_eq!(config.path, "cloudreve://my/Shows");
        Ok(())
    }

    #[test]
    fn parses_youtube_media_source_config() -> TestResult {
        let config = media_source_config_json_to_proto(
            CliSourceProvider::Youtube,
            r#"{"videoId":"dQw4w9WgXcQ","shared":true}"#,
        )?;

        let Some(synctv_proto::source_config::media_source_config::Provider::Youtube(config)) =
            config.provider
        else {
            return Err(anyhow::anyhow!(
                "YouTube media config did not produce its provider oneof"
            ));
        };
        assert_eq!(config.video_id, "dQw4w9WgXcQ");
        assert!(config.shared);
        Ok(())
    }

    #[test]
    fn parses_douyin_media_and_playlist_source_configs() -> TestResult {
        use synctv_proto::source_config::douyin_media_source_config::Source;

        let media = media_source_config_json_to_proto(
            CliSourceProvider::Douyin,
            r#"{"live":{"webRid":"123456","shared":true}}"#,
        )?;
        let Some(synctv_proto::source_config::media_source_config::Provider::Douyin(media)) =
            media.provider
        else {
            return Err(anyhow::anyhow!(
                "Douyin media config did not produce its provider oneof"
            ));
        };
        assert!(matches!(media.source, Some(Source::Live(_))));

        let playlist = playlist_source_config_json_to_proto(
            CliSourceProvider::Douyin,
            r#"{"secUid":"MS4wLjABAAAAexample","shared":true}"#,
        )?;
        let Some(synctv_proto::source_config::playlist_source_config::Provider::Douyin(config)) =
            playlist.provider
        else {
            return Err(anyhow::anyhow!(
                "Douyin playlist config did not produce its provider oneof"
            ));
        };
        assert_eq!(config.sec_uid, "MS4wLjABAAAAexample");
        assert!(config.shared);
        Ok(())
    }

    #[test]
    fn parses_tiktok_media_and_playlist_source_configs() -> TestResult {
        use synctv_proto::source_config::tik_tok_media_source_config::Source;

        let media = media_source_config_json_to_proto(
            CliSourceProvider::Tiktok,
            r#"{"live":{"uniqueId":"creator_name","shared":true}}"#,
        )?;
        let Some(synctv_proto::source_config::media_source_config::Provider::Tiktok(media)) =
            media.provider
        else {
            return Err(anyhow::anyhow!(
                "TikTok media config did not produce its provider oneof"
            ));
        };
        assert!(matches!(media.source, Some(Source::Live(_))));

        let playlist = playlist_source_config_json_to_proto(
            CliSourceProvider::Tiktok,
            r#"{"secUid":"MS4wLjABAAAAexample","shared":true}"#,
        )?;
        let Some(synctv_proto::source_config::playlist_source_config::Provider::Tiktok(config)) =
            playlist.provider
        else {
            return Err(anyhow::anyhow!(
                "TikTok playlist config did not produce its provider oneof"
            ));
        };
        assert_eq!(config.sec_uid, "MS4wLjABAAAAexample");
        assert!(config.shared);
        Ok(())
    }

    #[test]
    fn parses_twitch_media_and_playlist_source_configs() -> TestResult {
        use synctv_proto::source_config::twitch_media_source_config::Source;

        let media = media_source_config_json_to_proto(
            CliSourceProvider::Twitch,
            r#"{"video":{"videoId":"1234","shared":true}}"#,
        )?;
        let Some(synctv_proto::source_config::media_source_config::Provider::Twitch(media)) =
            media.provider
        else {
            return Err(anyhow::anyhow!(
                "Twitch media config did not produce its provider oneof"
            ));
        };
        assert!(matches!(media.source, Some(Source::Video(_))));

        let playlist = playlist_source_config_json_to_proto(
            CliSourceProvider::Twitch,
            r#"{"channel":{"channel":"streamer","content":4},"shared":true}"#,
        )?;
        let Some(synctv_proto::source_config::playlist_source_config::Provider::Twitch(config)) =
            playlist.provider
        else {
            return Err(anyhow::anyhow!(
                "Twitch playlist config did not produce its provider oneof"
            ));
        };
        let Some(synctv_proto::source_config::twitch_playlist_source_config::Source::Channel(
            channel,
        )) = config.source
        else {
            return Err(anyhow::anyhow!("expected Twitch channel playlist source"));
        };
        assert_eq!(channel.channel, "streamer");
        assert_eq!(
            channel.content,
            synctv_proto::source_config::TwitchPlaylistContent::Clips as i32
        );
        assert!(config.shared);
        Ok(())
    }

    #[test]
    fn parses_huya_live_and_video_source_configs() -> TestResult {
        use synctv_proto::source_config::huya_media_source_config::Source;

        for (raw, expected) in [
            (r#"{"live":{"roomId":"660000"}}"#, "live"),
            (r#"{"video":{"videoId":"1002412640"}}"#, "video"),
        ] {
            let media = media_source_config_json_to_proto(CliSourceProvider::Huya, raw)?;
            let Some(synctv_proto::source_config::media_source_config::Provider::Huya(config)) =
                media.provider
            else {
                return Err(anyhow::anyhow!(
                    "Huya media config did not produce its provider oneof"
                ));
            };
            let actual = match config.source {
                Some(Source::Live(_)) => "live",
                Some(Source::Video(_)) => "video",
                None => "missing",
            };
            assert_eq!(actual, expected);
        }
        Ok(())
    }

    #[test]
    fn parses_all_youtube_playlist_source_variants() -> TestResult {
        use synctv_proto::source_config::youtube_playlist_source_config::Source;

        for (raw, expected) in [
            (
                r#"{"shared":true,"playlist":{"playlistId":"PL-example"}}"#,
                "playlist",
            ),
            (
                r#"{"shared":false,"channel":{"channelId":"UC-example"}}"#,
                "channel",
            ),
            (
                r#"{"shared":false,"search":{"query":"rust media server"}}"#,
                "search",
            ),
            (r#"{"shared":true,"subscriptions":{}}"#, "subscriptions"),
            (r#"{"shared":true,"likedVideos":{}}"#, "likedVideos"),
            (r#"{"shared":true,"watchLater":{}}"#, "watchLater"),
        ] {
            let config = playlist_source_config_json_to_proto(CliSourceProvider::Youtube, raw)?;
            let Some(synctv_proto::source_config::playlist_source_config::Provider::Youtube(
                config,
            )) = config.provider
            else {
                return Err(anyhow::anyhow!(
                    "YouTube playlist config did not produce its provider oneof"
                ));
            };
            let actual = match config.source {
                Some(Source::Playlist(_)) => "playlist",
                Some(Source::Channel(_)) => "channel",
                Some(Source::Search(_)) => "search",
                Some(Source::Subscriptions(_)) => "subscriptions",
                Some(Source::LikedVideos(_)) => "likedVideos",
                Some(Source::WatchLater(_)) => "watchLater",
                None => "missing",
            };
            assert_eq!(actual, expected);
        }
        Ok(())
    }

    #[test]
    fn parses_truenas_playlist_source_config() -> TestResult {
        let config = playlist_source_config_json_to_proto(
            CliSourceProvider::Truenas,
            r#"{"serverId":"nas-home","search":{"path":"/mnt/tank","query":"movie"}}"#,
        )?;
        let Some(synctv_proto::source_config::playlist_source_config::Provider::Truenas(config)) =
            config.provider
        else {
            return Err(anyhow::anyhow!(
                "TrueNAS playlist config did not produce its provider oneof"
            ));
        };
        assert_eq!(config.server_id, "nas-home");
        assert!(matches!(
            config.source,
            Some(synctv_proto::source_config::true_nas_playlist_source_config::Source::Search(_))
        ));
        Ok(())
    }
}
