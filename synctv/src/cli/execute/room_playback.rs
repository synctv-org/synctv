use super::*;

pub(super) async fn execute_room_playback_state_update(
    room: RoomScopedRemoteArgs,
    update_type: CliPlaybackStateUpdateType,
    playing: Option<bool>,
    position: Option<f64>,
    speed: Option<f64>,
    version: Option<i64>,
) -> Result<()> {
    let session = connect_remote_access(&room.remote).await?;
    let response = management_unary_call!(
        session,
        "update room playback state",
        update_playback_state,
        management_proto::UpdatePlaybackStateRequest {
            room_id: room.room_id,
            update: Some(synctv_proto::client::UpdatePlaybackStateRequest {
                r#type: update_type.to_proto(),
                playing,
                position,
                speed,
                version,
                expected_media_id: None,
                expected_playlist_id: None,
                expected_target_hash: None,
            }),
        }
    )?;
    room.remote.print_output(&response)
}
