use std::net::IpAddr;
use std::time::Duration as StdDuration;

use synctv_common::ExecutionControl;

use crate::{
    models::{OpaquePasswordRecord, Room, RoomId, RoomMember, UserId},
    repository::room_password::RoomPasswordCredentialState,
    service::optimistic_retry,
    Error, Result,
};

use super::{
    join::RoomPasswordJoinProof, RealtimeOutboxPermissionChangedEventFactory,
    RoomOpaqueLoginStartChallenge, RoomOpaquePasswordLoginSession,
    RoomOpaquePasswordRegistrationSession, RoomOpaqueRegistrationStartChallenge, RoomService,
    ROOM_OPAQUE_LOGIN_SESSION_TTL_SECS, ROOM_OPAQUE_REGISTRATION_SESSION_TTL_SECS,
};

impl RoomService {
    pub async fn check_room_password(&self, room_id: &RoomId, password: &str) -> Result<bool> {
        let credential = self
            .room_password_repo
            .get_opaque_credential(room_id)
            .await?;
        match credential {
            Some(stored) if stored.state.enabled => self
                .opaque_password_service
                .verify_password(&stored.record, password),
            Some(_) => Err(Error::InvalidInput("Room password is disabled".to_string())),
            None => Err(Error::InvalidInput("Room has no password set".to_string())),
        }
    }

    pub async fn is_room_password_enabled(&self, room_id: &RoomId) -> Result<bool> {
        Ok(self
            .room_password_repo
            .get_state(room_id)
            .await?
            .is_some_and(|state| state.enabled))
    }

    pub async fn start_room_opaque_password_login_with_control(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        credential_request: Vec<u8>,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<RoomOpaqueLoginStartChallenge> {
        let subject_key = self.room_password_attempts_key(room_id, client_ip);
        if let Some(ref brute_force) = self.brute_force_service {
            brute_force
                .check_subject_key_allowed_with_control(&subject_key, client_ip, control)
                .await?;
        }
        let ctx = self
            .room_repo
            .get_join_context(room_id, user_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
        if !ctx.password_enabled {
            return Err(Error::InvalidInput(
                "Room does not require a password".to_string(),
            ));
        }
        let credential = ctx
            .password_credential
            .ok_or_else(|| Error::Authorization("Invalid password".to_string()))?;
        let password_version = ctx.password_version.ok_or_else(|| {
            tracing::warn!(
                room_id = %room_id,
                "Room requires password but credential version is missing"
            );
            Error::Authorization("Invalid password".to_string())
        })?;
        let login_start = self.opaque_password_service.start_login(
            Some(&credential),
            &credential.credential_identifier,
            &credential_request,
        )?;
        let session_id = synctv_common::snanoid!(48);
        self.opaque_password_login_session_store
            .store(
                &session_id,
                &RoomOpaquePasswordLoginSession {
                    room_id: *room_id,
                    user_id: *user_id,
                    expected_password_version: password_version,
                    server_login_state: login_start.server_login_state,
                    brute_force_subject_key: subject_key,
                },
                StdDuration::from_secs(ROOM_OPAQUE_LOGIN_SESSION_TTL_SECS),
            )
            .await?;
        Ok(RoomOpaqueLoginStartChallenge {
            session_id,
            credential_response: login_start.credential_response,
        })
    }

    pub async fn finish_room_opaque_password_login_with_outbox(
        &self,
        expected_room_id: Option<&RoomId>,
        session_id: &str,
        user_id: &UserId,
        credential_finalization: Vec<u8>,
        client_ip: Option<IpAddr>,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<(Room, RoomMember, Vec<crate::models::RoomMemberWithUser>)> {
        let Some(session) = self
            .opaque_password_login_session_store
            .consume(session_id)
            .await?
        else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };
        if session.user_id != *user_id {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        if expected_room_id.is_some_and(|room_id| session.room_id != *room_id) {
            return Err(Error::InvalidInput(
                "Room password login session does not match room".to_string(),
            ));
        }
        if let Some(ref brute_force) = self.brute_force_service {
            brute_force
                .check_subject_key_allowed_with_control(
                    &session.brute_force_subject_key,
                    client_ip,
                    None,
                )
                .await?;
        }
        let finish_result = self
            .opaque_password_service
            .finish_login(&session.server_login_state, &credential_finalization);
        if finish_result.is_err() {
            if let Some(ref brute_force) = self.brute_force_service {
                brute_force
                    .record_subject_key_failure_with_control(
                        &session.brute_force_subject_key,
                        client_ip,
                        None,
                    )
                    .await?;
            }
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        let current_state = self
            .room_password_repo
            .get_state(&session.room_id)
            .await?
            .ok_or_else(|| Error::Authorization("Invalid password".to_string()))?;
        if !current_state.enabled {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        if current_state.version != session.expected_password_version {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        if let Some(ref brute_force) = self.brute_force_service {
            if let Err(error) = brute_force
                .reset_subject_key_with_control(&session.brute_force_subject_key, None)
                .await
            {
                tracing::warn!(
                    room_id = %session.room_id,
                    error = %error,
                    "Failed to reset room password rate limit counter after successful OPAQUE login"
                );
            }
        }
        self.join_room_with_password_proof(
            session.room_id,
            session.user_id,
            RoomPasswordJoinProof::OpaqueVerified {
                expected_version: session.expected_password_version,
            },
            outbox_event_factory,
        )
        .await
    }

    pub async fn start_room_opaque_password_registration(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        registration_request: Vec<u8>,
    ) -> Result<RoomOpaqueRegistrationStartChallenge> {
        self.permission_service
            .check_permission_no_cache(
                room_id,
                user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;
        let credential_identifier = Self::room_opaque_credential_identifier(room_id);
        let registration_start = self
            .opaque_password_service
            .start_registration(&credential_identifier, &registration_request)?;
        let session_id = synctv_common::snanoid!(48);
        self.opaque_password_registration_session_store
            .store(
                &session_id,
                &RoomOpaquePasswordRegistrationSession {
                    room_id: *room_id,
                    user_id: *user_id,
                    credential_identifier,
                },
                StdDuration::from_secs(ROOM_OPAQUE_REGISTRATION_SESSION_TTL_SECS),
            )
            .await?;
        Ok(RoomOpaqueRegistrationStartChallenge {
            session_id,
            registration_response: registration_start.registration_response,
        })
    }

    pub async fn finish_room_opaque_password_registration(
        &self,
        room_id: &RoomId,
        session_id: &str,
        user_id: &UserId,
        registration_upload: Vec<u8>,
    ) -> Result<RoomPasswordCredentialState> {
        let Some(session) = self
            .opaque_password_registration_session_store
            .consume(session_id)
            .await?
        else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };
        if session.user_id != *user_id {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        if session.room_id != *room_id {
            return Err(Error::InvalidInput(
                "Room password registration session does not match room".to_string(),
            ));
        }
        self.permission_service
            .check_permission_no_cache(
                &session.room_id,
                user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;
        let opaque_record = self
            .opaque_password_service
            .finish_registration(session.credential_identifier, &registration_upload)?;
        self.update_room_password_as(&session.room_id, Some(user_id), Some(opaque_record))
            .await
    }

    pub async fn set_room_password_from_plaintext(
        &self,
        room_id: &RoomId,
        actor_user_id: Option<&UserId>,
        new_password: Option<&str>,
    ) -> Result<RoomPasswordCredentialState> {
        let opaque_record = new_password
            .map(|password| {
                self.opaque_password_service
                    .register_password(&Self::room_opaque_credential_identifier(room_id), password)
            })
            .transpose()?;
        self.update_room_password_as(room_id, actor_user_id, opaque_record)
            .await
    }

    pub async fn check_room_password_with_rate_limit(
        &self,
        room_id: &RoomId,
        password: &str,
        client_ip: Option<IpAddr>,
    ) -> Result<bool> {
        self.check_room_password_with_rate_limit_with_control(room_id, password, client_ip, None)
            .await
    }

    pub async fn check_room_password_with_rate_limit_with_control(
        &self,
        room_id: &RoomId,
        password: &str,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<bool> {
        let subject_key = self.room_password_attempts_key(room_id, client_ip);

        if let Some(ref brute_force) = self.brute_force_service {
            brute_force
                .check_subject_key_allowed_with_control(&subject_key, client_ip, control)
                .await?;
        }

        let is_valid = match self
            .room_password_repo
            .get_opaque_credential(room_id)
            .await?
        {
            Some(stored) if stored.state.enabled => self
                .opaque_password_service
                .verify_password(&stored.record, password),
            Some(_) | None => Ok(false),
        }?;

        // Handle success/failure tracking
        if let Some(ref brute_force) = self.brute_force_service {
            if is_valid {
                // Reset failure counter on successful verification
                if let Err(e) = brute_force
                    .reset_subject_key_with_control(&subject_key, control)
                    .await
                {
                    // Log warning for monitoring
                    tracing::warn!(
                        room_id = %room_id,
                        client_ip = ?client_ip,
                        error = %e,
                        "Failed to reset room password rate limit counter after successful verification"
                    );

                    // Record to audit log for security tracking
                    // This is security-relevant because a persistent counter could lead to
                    // legitimate users being locked out if Redis recovers with stale data
                    if let Some(ref audit) = self.audit_service {
                        let ip_str = client_ip.map(|ip| ip.to_string());
                        if let Err(audit_err) = audit
                            .log_rate_limit_reset_failed(
                                crate::models::AuditTargetType::Room,
                                room_id.to_string(),
                                e.to_string(),
                                ip_str,
                            )
                            .await
                        {
                            tracing::error!(
                                room_id = %room_id,
                                error = %audit_err,
                                "Failed to log rate limit reset failure to audit log"
                            );
                        }
                    }
                }
            } else {
                // Record failure on incorrect password
                brute_force
                    .record_subject_key_failure_with_control(&subject_key, client_ip, control)
                    .await?;
            }
        }

        Ok(is_valid)
    }

    /// Reset the room password rate limit counter.
    ///
    /// This is primarily used for testing to simulate lockout expiry.
    /// In production, counters expire automatically via TTL.
    pub async fn reset_room_password_rate_limit(
        &self,
        room_id: &RoomId,
        client_ip: IpAddr,
    ) -> Result<()> {
        if let Some(ref brute_force) = self.brute_force_service {
            let subject_key = self.room_password_attempts_key(room_id, Some(client_ip));
            brute_force
                .reset_subject_key_with_control(&subject_key, None)
                .await?;
        }
        Ok(())
    }

    fn room_password_attempts_key(&self, room_id: &RoomId, client_ip: Option<IpAddr>) -> String {
        let ip = client_ip.map_or_else(|| "unknown".to_string(), |ip| ip.to_string());
        self.user_service
            .key_builder()
            .room_password_attempts(&room_id.to_string(), &ip)
    }

    /// Update room password
    pub async fn update_room_password(
        &self,
        room_id: &RoomId,
        password: Option<String>,
    ) -> Result<()> {
        let opaque_record = password
            .as_deref()
            .map(|password| {
                self.opaque_password_service
                    .register_password(&Self::room_opaque_credential_identifier(room_id), password)
            })
            .transpose()?;
        self.update_room_password_as(room_id, None, opaque_record)
            .await
            .map(|_| ())
    }

    pub async fn update_room_password_as(
        &self,
        room_id: &RoomId,
        actor_user_id: Option<&UserId>,
        opaque_record: Option<OpaquePasswordRecord>,
    ) -> Result<RoomPasswordCredentialState> {
        let password_was_set = opaque_record.is_some();
        let state = self
            .do_set_room_password_credential(room_id, opaque_record)
            .await?;

        if password_was_set {
            self.revoke_all_guest_access(
                room_id,
                crate::service::notification::GuestKickReason::RoomPasswordAdded,
            )
            .await?;
        }

        self.notify_room_invalidation(room_id).await;
        tracing::debug!(
            room_id = %room_id,
            actor_user_id = ?actor_user_id,
            password_enabled = state.enabled,
            password_version = state.version,
            "Room password state updated"
        );
        Ok(state)
    }

    async fn do_set_room_password_credential(
        &self,
        room_id: &RoomId,
        opaque_record: Option<OpaquePasswordRecord>,
    ) -> Result<RoomPasswordCredentialState> {
        optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "Password update failed after maximum retry attempts",
            || async {
                let mut tx = self.pool.begin().await?;
                let state = if let Some(ref opaque_record) = opaque_record {
                    self.room_password_repo
                        .set_opaque_credential_with_executor(room_id, opaque_record, &mut *tx)
                        .await?
                } else {
                    self.room_password_repo
                        .disable_with_executor(room_id, &mut *tx)
                        .await?
                };

                tx.commit().await?;
                Ok(state)
            },
        )
        .await
    }
}
