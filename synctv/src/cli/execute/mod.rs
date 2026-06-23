use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use synctv_core::config::absolute_display_path;
use synctv_management::proto as management_proto;

use crate::admin_client::RemoteAdminSession;
use crate::app::Application;

use super::args::*;
use super::commands::*;
use super::completion::execute_completion;
use super::context::{CliConfigContext, RemoteCliContext};
use super::output::{
    mask_connection_url, print_humanized_structured_output, print_json, print_toml, print_yaml,
    redact_config_for_display, ConfigOutputFormat, RemoteOutputFormat,
};
use super::output_dto::{
    GetPlaybackCliOutput, KickStreamCliOutput, PlaybackPullUrlCliOutput, PlaybackStartCliOutput,
    PlaybackStopCliOutput, UserMutationCliOutput,
};

const MANAGEMENT_UNARY_RPC_TIMEOUT: Duration = Duration::from_secs(30);
const MANAGEMENT_STOP_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

macro_rules! management_unary_call {
    ($session:expr, $operation:literal, $method:ident, $request:expr) => {{
        let mut client = $session.management_client();
        management_unary_response($operation, client.$method($request)).await
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
mod provider_emby;
mod provider_rtmp;
mod review;
mod room;
mod room_playback;
mod serve;
mod settings;
mod slice_cache;
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
use provider_emby::execute_provider_emby;
use provider_rtmp::execute_provider_rtmp;
use review::execute_review;
use room::execute_room;
use room_playback::execute_room_playback_state_update;
use serve::{execute_serve, local_api_probe_address};
use settings::execute_settings;
use slice_cache::execute_slice_cache;
use stop::execute_stop;
use system::execute_system;
use user::execute_user;

#[cfg(test)]
pub(in crate::cli) use db::{database_summary, DatabaseCliOutput};
#[cfg(test)]
pub(in crate::cli) use serve::switch_process_working_dir_to_data_dir;
#[cfg(test)]
pub(in crate::cli) use stop::{
    stop_server_stage_name, stop_stream_disconnect_can_be_treated_as_success,
    stop_stream_end_can_be_treated_as_success, synthesize_stop_completion_if_needed,
    StopServerEventOutput, StopServerOutput,
};

pub async fn execute(cli: Cli) -> Result<()> {
    let cli = apply_root_global_overrides(cli);
    match cli.command {
        Commands::Serve(serve) => Box::pin(execute_serve(serve)).await,
        Commands::Stop(stop) => execute_stop(stop).await,
        Commands::Config(config) => execute_config(config),
        Commands::Db(db) => execute_db(db).await,
        Commands::User(user) => execute_user(user).await,
        Commands::Room(room) => execute_room(room).await,
        Commands::Review(review) => execute_review(review).await,
        Commands::Ban(ban) => execute_ban(ban).await,
        Commands::Playlist(playlist) => execute_playlist(playlist).await,
        Commands::Media(media) => execute_media(media).await,
        Commands::Provider(provider) => execute_provider(provider).await,
        Commands::Settings(settings) => execute_settings(settings).await,
        Commands::System(system) => execute_system(system).await,
        Commands::SliceCache(slice_cache) => execute_slice_cache(slice_cache).await,
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
        Commands::Provider(command) => merge_provider_command_globals(command, &root),
        Commands::Settings(command) => merge_settings_command_globals(command, &root),
        Commands::System(command) => merge_system_command_globals(command, &root),
        Commands::SliceCache(command) => merge_slice_cache_command_globals(command, &root),
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
        RoomSubcommand::Member(command) => match &mut command.command {
            RoomMemberSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
            RoomMemberSubcommand::Add(args) => merge_room_scoped_remote_args(&mut args.room, root),
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

fn merge_provider_command_globals(command: &mut ProviderCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        ProviderSubcommand::Available(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Backends(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Create(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Update(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Delete(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Reconnect(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Enable(args) => merge_remote_access_args(&mut args.remote, root),
        ProviderSubcommand::Disable(args) => merge_remote_access_args(&mut args.remote, root),
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
                merge_remote_access_args(&mut args.access.remote, root);
            }
            ProviderBilibiliSubcommand::Binds(args) => {
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
    }
}

fn merge_settings_command_globals(command: &mut SettingsCommand, root: &GlobalConfigArgs) {
    match &mut command.command {
        SettingsSubcommand::List(args) => merge_remote_access_args(&mut args.remote, root),
        SettingsSubcommand::Get(args) => merge_remote_access_args(&mut args.remote, root),
        SettingsSubcommand::Update(args) => merge_remote_access_args(&mut args.remote, root),
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
                        .is_ok_and(|index| info.default_media_index == Some(index));
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
                    "m3u8" if hls_pull_url.is_none() => {
                        hls_pull_url = Some(output.url.clone());
                        hls_absolute_pull_url.clone_from(&output.absolute_url);
                    }
                    "flv" if flv_pull_url.is_none() => {
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

fn raw_optional_bytes(raw: Option<&str>) -> Vec<u8> {
    raw.map(str::as_bytes)
        .map_or_else(Vec::new, ToOwned::to_owned)
}

fn parse_media_source_config_json(
    provider: CliSourceProvider,
    raw: &str,
) -> Result<synctv_proto::source_config::MediaSourceConfig> {
    let json: Value = parse_cli_json("media source_config", raw)?;
    media_source_config_json_to_proto(provider, &json)
}

fn parse_optional_playlist_source_config_json(
    provider: Option<CliSourceProvider>,
    raw: Option<&str>,
) -> Result<Option<synctv_proto::source_config::PlaylistSourceConfig>> {
    match (provider, raw) {
        (Some(provider), Some(raw)) => {
            let json: Value = parse_cli_json("playlist source_config", raw)?;
            playlist_source_config_json_to_proto(provider, &json).map(Some)
        }
        (None, Some(_)) => bail!("--source-provider is required with --source-config-json"),
        (_, None) => Ok(None),
    }
}

fn media_source_config_json_to_proto(
    provider: CliSourceProvider,
    json: &Value,
) -> Result<synctv_proto::source_config::MediaSourceConfig> {
    let config =
        synctv_core::models::MediaSourceConfig::from_provider_json(provider.to_core(), json)?;
    Ok(config.into())
}

fn playlist_source_config_json_to_proto(
    provider: CliSourceProvider,
    json: &Value,
) -> Result<synctv_proto::source_config::PlaylistSourceConfig> {
    let config =
        synctv_core::models::PlaylistSourceConfig::from_provider_json(provider.to_core(), json)?;
    Ok(config.into())
}

fn parse_cli_json<T>(label: &str, raw: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_str(raw).with_context(|| format!("Invalid {label} JSON"))
}

fn parse_cli_optional_json<T>(label: &str, raw: Option<&str>) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    raw.map(|value| parse_cli_json(label, value)).transpose()
}

pub(in crate::cli) fn parse_setting_entries(
    entries: &[String],
) -> Result<std::collections::HashMap<String, String>> {
    let mut settings = std::collections::HashMap::with_capacity(entries.len());
    for entry in entries {
        let Some((key, value)) = entry.split_once('=') else {
            bail!("invalid --set entry '{entry}': expected key=value");
        };
        let key = key.trim();
        if key.is_empty() {
            bail!("invalid --set entry '{entry}': key must not be empty");
        }
        if settings
            .insert(key.to_string(), value.to_string())
            .is_some()
        {
            bail!("duplicate --set entry for key '{key}'");
        }
    }
    Ok(settings)
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
