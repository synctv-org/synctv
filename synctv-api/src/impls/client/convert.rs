//! Proto conversion helper functions

pub(super) const fn user_role_to_proto(role: synctv_core::models::UserRole) -> i32 {
    match role {
        synctv_core::models::UserRole::Root => synctv_proto::common::UserRole::Root as i32,
        synctv_core::models::UserRole::Admin => synctv_proto::common::UserRole::Admin as i32,
        synctv_core::models::UserRole::User => synctv_proto::common::UserRole::User as i32,
    }
}

pub(super) const fn user_status_to_proto(status: synctv_core::models::UserStatus) -> i32 {
    match status {
        synctv_core::models::UserStatus::Active => synctv_proto::common::UserStatus::Active as i32,
        synctv_core::models::UserStatus::Pending => synctv_proto::common::UserStatus::Pending as i32,
        synctv_core::models::UserStatus::Banned => synctv_proto::common::UserStatus::Banned as i32,
    }
}

pub fn proto_role_to_room_role(role_i32: i32) -> Result<synctv_core::models::RoomRole, crate::impls::ApiError> {
    match synctv_proto::common::RoomMemberRole::try_from(role_i32) {
        Ok(synctv_proto::common::RoomMemberRole::Creator) => Ok(synctv_core::models::RoomRole::Creator),
        Ok(synctv_proto::common::RoomMemberRole::Admin) => Ok(synctv_core::models::RoomRole::Admin),
        Ok(synctv_proto::common::RoomMemberRole::Member) => Ok(synctv_core::models::RoomRole::Member),
        Ok(synctv_proto::common::RoomMemberRole::Guest) => Ok(synctv_core::models::RoomRole::Guest),
        _ => Err(crate::impls::ApiError::InvalidInput(format!("Unknown room member role: {role_i32}"))),
    }
}

pub fn proto_role_to_user_role(role_i32: i32) -> Result<synctv_core::models::UserRole, crate::impls::ApiError> {
    match synctv_proto::common::UserRole::try_from(role_i32) {
        Ok(synctv_proto::common::UserRole::Root) => Ok(synctv_core::models::UserRole::Root),
        Ok(synctv_proto::common::UserRole::Admin) => Ok(synctv_core::models::UserRole::Admin),
        Ok(synctv_proto::common::UserRole::User) => Ok(synctv_core::models::UserRole::User),
        _ => Err(crate::impls::ApiError::InvalidInput(format!("Unknown user role: {role_i32}"))),
    }
}

#[must_use]
pub const fn room_role_to_proto(role: synctv_core::models::RoomRole) -> i32 {
    match role {
        synctv_core::models::RoomRole::Creator => synctv_proto::common::RoomMemberRole::Creator as i32,
        synctv_core::models::RoomRole::Admin => synctv_proto::common::RoomMemberRole::Admin as i32,
        synctv_core::models::RoomRole::Member => synctv_proto::common::RoomMemberRole::Member as i32,
        synctv_core::models::RoomRole::Guest => synctv_proto::common::RoomMemberRole::Guest as i32,
    }
}

pub(super) fn user_to_proto(user: &synctv_core::models::User) -> crate::proto::client::User {
    crate::proto::client::User {
        id: user.id.as_str().to_string(),
        username: user.username.clone(),
        email: user.email.clone().unwrap_or_default(),
        role: user_role_to_proto(user.role),
        status: user_status_to_proto(user.status),
        created_at: user.created_at.timestamp(),
        email_verified: user.email_verified,
    }
}

pub(super) fn room_to_proto_basic(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
) -> crate::proto::client::Room {
    let room_settings = settings.cloned().unwrap_or_default();
    crate::proto::client::Room {
        id: room.id.as_str().to_string(),
        name: room.name.clone(),
        description: room.description.clone(),
        created_by: room.created_by.as_str().to_string(),
        status: synctv_proto::common::RoomStatus::from(room.status) as i32,
        settings: serde_json::to_vec(&room_settings).unwrap_or_default(),
        created_at: room.created_at.timestamp(),
        member_count: member_count.unwrap_or(0),
        updated_at: room.updated_at.timestamp(),
        is_banned: room.is_banned,
    }
}

#[must_use]
pub fn media_to_proto(media: &synctv_core::models::Media) -> crate::proto::client::Media {
    // Get metadata from PlaybackResult if available (for direct URLs)
    let metadata_bytes = if media.is_direct() {
        media
            .get_playback_result()
            .map(|pb| serde_json::to_vec(&pb.metadata).unwrap_or_default())
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    // Strip credentials from source_config before sending to clients
    let sanitized_config = synctv_core::provider::strip_source_config_credentials(&media.source_config);

    crate::proto::client::Media {
        id: media.id.as_str().to_string(),
        room_id: media.room_id.as_str().to_string(),
        provider: media.source_provider.clone(),
        title: media.name.clone(),
        metadata: metadata_bytes,
        position: media.position,
        added_at: media.added_at.timestamp(),
        added_by: media.creator_id.as_ref().map_or(String::new(), |id| id.as_str().to_string()),
        provider_instance_name: media.provider_instance_name.clone().unwrap_or_default(),
        source_config: serde_json::to_vec(&sanitized_config).unwrap_or_default(),
    }
}

pub(super) fn playlist_to_proto(playlist: &synctv_core::models::Playlist, item_count: i32) -> crate::proto::client::Playlist {
    crate::proto::client::Playlist {
        id: playlist.id.as_str().to_string(),
        room_id: playlist.room_id.as_str().to_string(),
        name: playlist.name.clone(),
        parent_id: playlist.parent_id.as_ref().map(|id| id.as_str().to_string()).unwrap_or_default(),
        position: playlist.position,
        is_folder: playlist.parent_id.is_none() || playlist.source_provider.is_some(),
        is_dynamic: playlist.is_dynamic(),
        item_count,
        created_at: playlist.created_at.timestamp(),
        updated_at: playlist.updated_at.timestamp(),
    }
}

pub(super) fn playback_state_to_proto(state: &synctv_core::models::RoomPlaybackState) -> crate::proto::client::PlaybackState {
    crate::proto::client::PlaybackState {
        room_id: state.room_id.as_str().to_string(),
        playing_media_id: state.playing_media_id.as_ref().map(|id| id.as_str().to_string()).unwrap_or_default(),
        current_time: state.current_time,
        speed: state.speed,
        is_playing: state.is_playing,
        updated_at: state.updated_at.timestamp(),
        version: state.version as i32,
        playing_playlist_id: state.playing_playlist_id.as_ref().map(|id| id.as_str().to_string()).unwrap_or_default(),
        relative_path: state.relative_path.clone(),
    }
}

pub(super) fn room_member_to_proto(
    member: synctv_core::models::RoomMemberWithUser,
    role_default: synctv_core::models::PermissionBits,
) -> synctv_proto::common::RoomMember {
    synctv_proto::common::RoomMember {
        room_id: member.room_id.as_str().to_string(),
        user_id: member.user_id.as_str().to_string(),
        username: member.username.clone(),
        role: room_role_to_proto(member.role),
        permissions: member.effective_permissions(role_default).0,
        added_permissions: member.added_permissions,
        removed_permissions: member.removed_permissions,
        admin_added_permissions: member.admin_added_permissions,
        admin_removed_permissions: member.admin_removed_permissions,
        joined_at: member.joined_at.timestamp(),
        is_online: member.is_online,
    }
}

/// Convert SFU `NetworkStats` to proto `PeerNetworkQuality`
#[must_use]
pub fn network_stats_to_proto(
    peer_id: String,
    ns: synctv_sfu::NetworkStats,
) -> crate::proto::client::PeerNetworkQuality {
    let quality_action = match ns.quality_action {
        synctv_sfu::QualityAction::None => crate::proto::client::QualityAction::None.into(),
        synctv_sfu::QualityAction::ReduceQuality => crate::proto::client::QualityAction::ReduceQuality.into(),
        synctv_sfu::QualityAction::ReduceFramerate => crate::proto::client::QualityAction::ReduceFramerate.into(),
        synctv_sfu::QualityAction::AudioOnly => crate::proto::client::QualityAction::AudioOnly.into(),
    };
    crate::proto::client::PeerNetworkQuality {
        peer_id,
        rtt_ms: ns.rtt_ms,
        packet_loss_rate: ns.packet_loss_rate,
        jitter_ms: ns.jitter_ms,
        available_bandwidth_kbps: ns.available_bandwidth_kbps,
        quality_score: u32::from(ns.quality_score),
        quality_action,
    }
}
