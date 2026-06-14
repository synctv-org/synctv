use super::{middleware::RequestMetadata, AppResult, AppState};

mod chat;
mod execute;
mod lifecycle;
mod media;
mod members;
mod objects;
mod playback;
mod playlists;
mod query;
mod report;
mod settings;
mod streams;
mod types;
pub(crate) mod watch;
#[cfg(feature = "openapi")]
pub(crate) use chat::{
    __path_clear_chat_reaction, __path_delete_chat_message, __path_edit_chat_message,
    __path_get_chat_history, __path_get_chat_message, __path_get_chat_message_context,
    __path_get_chat_message_read_receipts, __path_get_chat_read_state,
    __path_list_chat_reaction_users, __path_mark_chat_read, __path_send_chat_message,
    __path_set_chat_reaction,
};
pub use chat::{
    clear_chat_reaction, delete_chat_message, edit_chat_message, get_chat_history,
    get_chat_message, get_chat_message_context, get_chat_message_read_receipts,
    get_chat_playback_messages, get_chat_read_state, list_chat_reaction_users, mark_chat_read,
    send_chat_message, set_chat_reaction,
};
pub(crate) use execute::execute_room_actor_endpoint;
use execute::request_metadata;
#[cfg(feature = "openapi")]
pub(crate) use lifecycle::{
    __path_check_room, __path_create_room, __path_get_hot_rooms, __path_get_room, __path_join_room,
    __path_leave_room, __path_list_or_get_rooms,
};
pub use lifecycle::{
    check_room, create_room, delete_room, get_hot_rooms, get_room, join_room, leave_room,
    list_or_get_rooms,
};
#[cfg(feature = "openapi")]
pub(crate) use media::{
    __path_add_media, __path_clear_playlist, __path_delete_entries, __path_delete_media,
    __path_edit_media, __path_get_media, __path_get_playlist, __path_list_playlist_items,
    __path_move_media, __path_push_media_batch,
};
pub use media::{
    add_media, clear_playlist, delete_entries, delete_media, edit_media, get_media, get_playlist,
    list_playlist_items, move_media, push_media_batch,
};
#[cfg(feature = "openapi")]
pub(crate) use members::__path_get_room_members;
pub use members::get_room_members;
#[cfg(feature = "openapi")]
pub(crate) use objects::__path_create_chat_image_upload_session;
pub use objects::{
    clear_playlist_cover, clear_room_cover, clear_video_cover, create_chat_image_upload_session,
    create_playlist_cover_upload_session, create_room_cover_upload_session,
    create_video_cover_upload_session, get_chat_image_object, get_playlist_cover_object,
    get_room_cover_object, get_video_cover_object, update_playlist_cover, update_room_cover,
    update_video_cover, upload_chat_image_object, upload_playlist_cover_object,
    upload_room_cover_object, upload_video_cover_object,
};
#[cfg(feature = "openapi")]
pub(crate) use playback::{
    __path_get_playback, __path_start_playback, __path_stop_playback, __path_update_playback_state,
};
pub use playback::{
    get_playback, start_playback, stop_playback, update_playback_state, watch_playback,
    watch_playback_state,
};
#[cfg(feature = "openapi")]
pub(crate) use playlists::{
    __path_create_playlist, __path_delete_playlist, __path_list_playlists, __path_move_playlist,
    __path_update_playlist,
};
pub use playlists::{
    create_playlist, delete_playlist, list_playlists, move_playlist, update_playlist,
};
#[cfg(test)]
pub(crate) use query::build_get_playback_request;
#[cfg(test)]
pub(crate) use query::watch_after_event_sequence;
#[cfg(test)]
use query::{parse_optional_query_bool, parse_optional_query_i32};
pub use query::{GetPlaybackQuery, WatchPlaybackQuery, WatchQuery};
#[cfg(feature = "openapi")]
pub(crate) use report::{
    __path_get_room_content_report, __path_list_room_content_reports, __path_report_content,
    __path_update_room_content_report_status,
};
pub use report::{
    get_room_content_report, list_room_content_reports, report_content,
    update_room_content_report_status,
};
#[cfg(feature = "openapi")]
pub(crate) use settings::{
    __path_clear_room_password, __path_finish_room_password_login,
    __path_finish_room_password_registration, __path_get_room_settings, __path_reset_room_settings,
    __path_start_room_password_login, __path_start_room_password_registration,
    __path_transfer_room_ownership, __path_update_room_settings,
};
pub use settings::{
    clear_room_password, finish_room_password_login, finish_room_password_registration,
    get_room_settings, reset_room_settings, start_room_password_login,
    start_room_password_registration, transfer_room_ownership, update_room_settings,
};
#[cfg(feature = "openapi")]
pub(crate) use streams::{
    __path_get_room_stream_info, __path_kick_room_stream, __path_list_room_streams,
};
pub use streams::{get_room_stream_info, kick_room_stream, list_room_streams};
pub use types::{
    ChatImageObjectQuery, KickRoomStreamBody, PlaylistCoverObjectQuery, RoomCoverObjectQuery,
    VideoCoverObjectQuery,
};
#[cfg(feature = "openapi")]
pub(crate) use watch::__path_watch_chat_events;
#[cfg(test)]
use watch::{sse_event_from_server_message, sse_event_id_from_resource_event, CancelOnDropStream};
pub use watch::{watch_chat_events, watch_playlist_items, watch_room_members, watch_room_settings};

#[cfg(test)]
mod tests;
