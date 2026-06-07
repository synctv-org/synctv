use synctv_core::{
    models::{RoomId, RoomMember, RoomSettings, RoomStatus, UserId},
    service::RoomService,
};

use super::{RealtimeJoinError, RealtimePrincipal};

pub(crate) fn guest_policy_error_to_denial_reason(
    error: synctv_core::Error,
) -> Result<Option<String>, synctv_core::Error> {
    match error {
        synctv_core::Error::Authorization(reason) => Ok(Some(reason)),
        error => Err(error),
    }
}

#[derive(Debug)]
pub(super) enum RealtimeMembershipAccess {
    Allowed(RoomMember),
    Denied(String),
}

pub(super) struct InitialRealtimeJoinState {
    pub(super) member: Option<RoomMember>,
    pub(super) room_settings: Option<RoomSettings>,
}

pub(super) async fn guest_admission_denial_reason(
    room_service: &RoomService,
    room_id: &RoomId,
    user_id: &UserId,
    principal: &RealtimePrincipal,
) -> Result<Option<String>, RealtimeJoinError> {
    let room = room_service.get_room(room_id).await.map_err(|error| {
        tracing::warn!(
            error = %error,
            room_id = %room_id,
            user_id = %user_id,
            "Failed to re-validate guest room access; rejecting connection because final admission must fail closed"
        );
        RealtimeJoinError::ServiceUnavailable(
            "Room re-validation temporarily unavailable".to_string(),
        )
    })?;

    if room.is_banned {
        return Ok(Some("This room has been banned".to_string()));
    }
    if room.status == RoomStatus::Closed {
        return Ok(Some(
            "This room is closed and not accepting new connections".to_string(),
        ));
    }

    let policy_denial = room_service
        .check_guest_allowed(room_id, room_service.settings_registry().map(AsRef::as_ref))
        .await
        .map_or_else(
            |error| match guest_policy_error_to_denial_reason(error) {
                Ok(reason) => Ok(reason),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        user_id = %user_id,
                        "Failed to validate guest policy"
                    );
                    Err(RealtimeJoinError::ServiceUnavailable(
                        "Guest policy validation temporarily unavailable".to_string(),
                    ))
                }
            },
            |()| Ok(None),
        )?;
    if let Some(reason) = policy_denial {
        return Ok(Some(reason));
    }

    if let Some(identity) = principal.guest_identity() {
        match guest_token_blacklist_denial_reason(
            room_service,
            room_id,
            user_id,
            &identity.token_jti,
        )
        .await
        {
            Ok(Some(reason)) => return Ok(Some(reason)),
            Ok(None) => {}
            Err(error) => return Err(error),
        }

        let current_version =
            room_service
                .get_room_guest_version(room_id)
                .await
                .map_err(|error| {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        user_id = %user_id,
                        "Failed to validate guest token version"
                    );
                    RealtimeJoinError::ServiceUnavailable(
                        "Guest access validation temporarily unavailable".to_string(),
                    )
                })?;
        if identity.room_guest_version < current_version {
            return Ok(Some(
                "Guest access for this room has been revoked".to_string(),
            ));
        }
    }

    Ok(None)
}

pub(crate) async fn guest_token_blacklist_denial_reason(
    room_service: &RoomService,
    room_id: &RoomId,
    user_id: &UserId,
    token_jti: &str,
) -> Result<Option<String>, RealtimeJoinError> {
    let user_service = room_service.user_service();
    let key = user_service.key_builder().guest_token_blacklist(token_jti);
    match user_service
        .token_blacklist_store()
        .is_blacklisted_checked(&key)
        .await
    {
        Ok(true) => Ok(Some("Guest token has been revoked".to_string())),
        Ok(false) => Ok(None),
        Err(error) => {
            tracing::warn!(
                error = %error,
                room_id = %room_id,
                user_id = %user_id,
                "Failed to validate guest token blacklist during realtime admission check"
            );
            Err(RealtimeJoinError::ServiceUnavailable(
                "Guest access validation temporarily unavailable".to_string(),
            ))
        }
    }
}

pub(super) async fn probe_realtime_membership_access_with_room(
    room_service: &RoomService,
    room: &synctv_core::models::Room,
    user_id: &UserId,
) -> synctv_core::Result<RealtimeMembershipAccess> {
    match room_service.check_membership_with_room(room, user_id).await {
        Ok(()) => match room_service
            .member_service()
            .get_member(&room.id, user_id)
            .await?
        {
            Some(member) => Ok(RealtimeMembershipAccess::Allowed(member)),
            None => Ok(RealtimeMembershipAccess::Denied(
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            )),
        },
        Err(synctv_core::Error::Authorization(message))
            if message == synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM =>
        {
            if room_service
                .member_service()
                .is_in_kick_cooldown(&room.id, user_id)
                .await?
            {
                Ok(RealtimeMembershipAccess::Denied(
                    synctv_core::repository::room_member::KICK_COOLDOWN_DENIED_MESSAGE.to_string(),
                ))
            } else {
                Ok(RealtimeMembershipAccess::Denied(message))
            }
        }
        Err(synctv_core::Error::Authorization(message)) => {
            Ok(RealtimeMembershipAccess::Denied(message))
        }
        Err(error) => Err(error),
    }
}

pub(super) async fn probe_realtime_membership_access(
    room_service: &RoomService,
    room_id: &RoomId,
    user_id: &UserId,
) -> synctv_core::Result<RealtimeMembershipAccess> {
    let room = room_service.get_room(room_id).await?;
    probe_realtime_membership_access_with_room(room_service, &room, user_id).await
}

pub(super) async fn realtime_membership_denial_reason(
    room_service: &RoomService,
    room_id: &RoomId,
    user_id: &UserId,
) -> synctv_core::Result<Option<String>> {
    match probe_realtime_membership_access(room_service, room_id, user_id).await? {
        RealtimeMembershipAccess::Allowed(_) => Ok(None),
        RealtimeMembershipAccess::Denied(reason) => Ok(Some(reason)),
    }
}
