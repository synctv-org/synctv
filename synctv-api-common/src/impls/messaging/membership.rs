use synctv_core::{
    models::{RoomId, RoomMember, RoomSettings, UserId},
    service::RoomService,
};

pub(super) use synctv_core::service::RealtimeMembershipAccess;

use super::{RealtimeJoinError, RealtimePrincipal};

pub fn guest_policy_error_to_denial_reason(
    error: synctv_core::Error,
) -> Result<Option<String>, synctv_core::Error> {
    match error {
        synctv_core::Error::Authorization(reason) => Ok(Some(reason)),
        error => Err(error),
    }
}

pub(super) struct InitialRealtimeJoinState {
    pub(super) member: Option<RoomMember>,
    pub(super) room_settings: Option<RoomSettings>,
}

pub(super) struct RealtimeMembershipProbe<'a> {
    room_service: &'a RoomService,
}

impl<'a> RealtimeMembershipProbe<'a> {
    pub(super) fn new(room_service: &'a RoomService) -> Self {
        Self { room_service }
    }

    pub(super) async fn guest_admission_denial_reason(
        &self,
        room_id: &RoomId,
        principal: &RealtimePrincipal,
    ) -> Result<Option<String>, RealtimeJoinError> {
        let room = self.room_service.get_room(room_id).await.map_err(|error| {
            tracing::warn!(
                error = %error,
                room_id = %room_id,
                guest_id = principal.guest_identity().map(|identity| identity.guest_id.as_str()),
                "Failed to re-validate guest room access; rejecting connection because final admission must fail closed"
            );
            RealtimeJoinError::ServiceUnavailable(
                "Room re-validation temporarily unavailable".to_string(),
            )
        })?;

        if let Err(error) = self.room_service.ensure_guest_room_available(&room).await {
            return match guest_policy_error_to_denial_reason(error) {
                Ok(reason) => Ok(reason),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        guest_id = principal.guest_identity().map(|identity| identity.guest_id.as_str()),
                        "Failed to validate guest room availability"
                    );
                    Err(RealtimeJoinError::ServiceUnavailable(
                        "Room availability validation temporarily unavailable".to_string(),
                    ))
                }
            };
        }

        let policy_denial = self
            .room_service
            .check_guest_allowed(
                room_id,
                self.room_service
                    .runtime_settings_store()
                    .map(AsRef::as_ref),
            )
            .await
            .map_or_else(
                |error| match guest_policy_error_to_denial_reason(error) {
                    Ok(reason) => Ok(reason),
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            room_id = %room_id,
                            guest_id = principal.guest_identity().map(|identity| identity.guest_id.as_str()),
                            "Failed to validate guest policy"
                        );
                        Err(RealtimeJoinError::ServiceUnavailable(
                            "Guest policy validation temporarily unavailable".to_string(),
                        ))
                    }
                },
                |_| Ok(None),
            )?;
        if let Some(reason) = policy_denial {
            return Ok(Some(reason));
        }

        if let Some(identity) = principal.guest_identity() {
            match self
                .guest_token_blacklist_denial_reason(room_id, identity, &identity.token_jti)
                .await
            {
                Ok(Some(reason)) => return Ok(Some(reason)),
                Ok(None) => {}
                Err(error) => return Err(error),
            }

            let current_version = self
                .room_service
                .get_room_guest_version(room_id)
                .await
                .map_err(|error| {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        guest_id = %identity.guest_id,
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

    pub async fn guest_token_blacklist_denial_reason(
        &self,
        room_id: &RoomId,
        identity: &super::GuestRealtimeIdentity,
        token_jti: &str,
    ) -> Result<Option<String>, RealtimeJoinError> {
        let user_service = self.room_service.user_service();
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
                    guest_id = %identity.guest_id,
                    "Failed to validate guest token blacklist during realtime admission check"
                );
                Err(RealtimeJoinError::ServiceUnavailable(
                    "Guest access validation temporarily unavailable".to_string(),
                ))
            }
        }
    }

    pub(super) async fn probe_realtime_membership_access_with_room(
        &self,
        room: &synctv_core::models::Room,
        user_id: &UserId,
    ) -> synctv_core::Result<RealtimeMembershipAccess> {
        self.room_service
            .realtime_membership_access_with_room(room, user_id)
            .await
    }

    pub(super) async fn probe_realtime_membership_access(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> synctv_core::Result<RealtimeMembershipAccess> {
        let room = self.room_service.get_room(room_id).await?;
        self.probe_realtime_membership_access_with_room(&room, user_id)
            .await
    }

    pub(super) async fn realtime_membership_denial_reason(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> synctv_core::Result<Option<String>> {
        match self
            .probe_realtime_membership_access(room_id, user_id)
            .await
        {
            Ok(RealtimeMembershipAccess::Allowed(_)) => Ok(None),
            Ok(RealtimeMembershipAccess::Denied(reason))
            | Err(synctv_core::Error::Authorization(reason)) => Ok(Some(reason)),
            Err(error) => Err(error),
        }
    }
}
