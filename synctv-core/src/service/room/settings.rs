use crate::{
    cache::{CacheDomain, ConsistencyCoordinator, VersionFenceReservation},
    models::{AuditAction, AuditDetails, AuditTargetType, RoomId, RoomSettings, UserId},
    service::optimistic_retry,
    Error, Result,
};

use super::{
    ensure_actor_has_room_permission_now_tx, RealtimeOutboxSettingsEventFactory, RoomService,
};

impl RoomService {
    /// Set room settings with optimistic locking (CAS).
    ///
    /// Uses version-based CAS to prevent concurrent overwrites. Retries
    /// automatically on version conflicts with a total timeout limit.
    pub async fn set_settings(
        &self,
        room_id: RoomId,
        user_id: UserId,
        settings: RoomSettings,
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        // Check permission
        self.permission_service
            .check_permission(
                &room_id,
                &user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;

        // Validate permission escalation
        settings.validate()?;

        // Verify room exists
        self.room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        // CAS write with retry and total timeout
        let room_id_clone = room_id;
        let settings_clone = settings.clone();
        let room_settings_repo = self.room_settings_repo.clone();
        let audit_service = self.audit_service.clone();

        let (previous_settings, updated_settings, updated_version) =
            optimistic_retry::retry_with_optimistic_lock_timeout(
                Self::MAX_RETRIES,
                Self::BACKOFF_BASE_MS,
                std::time::Duration::from_secs(Self::SETTINGS_UPDATE_TIMEOUT_SECS),
                "Settings update failed after maximum retry attempts",
                || {
                    let room_id = room_id_clone;
                    let settings = settings_clone.clone();
                    let room_settings_repo = room_settings_repo.clone();
                    let consistency = self.consistency.clone();
                    async move {
                        let (current, version) =
                            room_settings_repo.get_with_version(&room_id).await?;
                        let domain = CacheDomain::RoomSettings { room_id };
                        let reservation =
                            Self::begin_room_settings_write_with(&consistency, &room_id, version)
                                .await?;
                        let new_version = if let Some(reservation) = &reservation {
                            match room_settings_repo
                                .set_settings_with_exact_version(
                                    &room_id,
                                    &settings,
                                    version,
                                    reservation.version,
                                )
                                .await
                            {
                                Ok(new_version) => {
                                    if let Err(error) = Self::commit_room_settings_write_with(
                                        &consistency,
                                        &domain,
                                        Some(reservation),
                                        new_version,
                                    )
                                    .await
                                    {
                                        tracing::warn!(
                                            error = %error,
                                            domain = %domain,
                                            version = new_version,
                                            operation = "set_settings",
                                            "Failed to finalize room settings fence after committed DB write"
                                        );
                                    }
                                    new_version
                                }
                                Err(error) => {
                                    Self::abort_room_settings_write_with(
                                        &consistency,
                                        &domain,
                                        Some(reservation),
                                    )
                                    .await;
                                    return Err(error);
                                }
                            }
                        } else {
                            room_settings_repo
                                .set_settings_with_version(&room_id, &settings, version)
                                .await?
                        };
                        Ok((current, settings, new_version))
                    }
                },
            )
            .await?;

        let snapshot = self
            .finalize_room_settings_update(
                &room_id,
                &previous_settings,
                &updated_settings,
                updated_version,
                Some(&user_id),
                "",
            )
            .await?;

        if audit_service.is_some() {
            self.write_audit_event(
                &user_id,
                &user_id.to_string(),
                AuditAction::RoomSettingsUpdated,
                AuditTargetType::Room,
                Some(room_id.to_string()),
                AuditDetails {
                    room_settings: Some(Box::new(snapshot.settings.clone())),
                    ..Default::default()
                },
            )
            .await?;
        }

        Ok(snapshot)
    }

    async fn begin_room_settings_write_with(
        consistency: &ConsistencyCoordinator,
        room_id: &RoomId,
        db_version: i64,
    ) -> Result<Option<VersionFenceReservation>> {
        let domain = CacheDomain::RoomSettings { room_id: *room_id };
        consistency.begin_observed_write(&domain, db_version).await
    }

    async fn commit_room_settings_write_with(
        consistency: &ConsistencyCoordinator,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
        version: i64,
    ) -> Result<()> {
        consistency
            .commit_reserved_write(domain, reservation, version)
            .await?;
        Ok(())
    }

    async fn abort_room_settings_write_with(
        consistency: &ConsistencyCoordinator,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
    ) {
        consistency.abort_reserved_write(domain, reservation).await;
    }

    async fn begin_room_settings_write(
        &self,
        room_id: &RoomId,
        db_version: i64,
    ) -> Result<Option<VersionFenceReservation>> {
        Self::begin_room_settings_write_with(&self.consistency, room_id, db_version).await
    }

    async fn commit_room_settings_write(
        &self,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
        version: i64,
    ) -> Result<()> {
        Self::commit_room_settings_write_with(&self.consistency, domain, reservation, version).await
    }

    async fn finalize_committed_room_settings_write_best_effort(
        &self,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
        version: i64,
        operation: &'static str,
    ) {
        if let Err(error) = self
            .commit_room_settings_write(domain, reservation, version)
            .await
        {
            tracing::warn!(
                error = %error,
                domain = %domain,
                version,
                operation,
                "Failed to finalize room settings fence after committed DB write"
            );
        }
    }

    async fn abort_room_settings_write(
        &self,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
    ) {
        Self::abort_room_settings_write_with(&self.consistency, domain, reservation).await;
    }

    /// Set room settings (replace entire settings object) with optimistic locking.
    pub async fn set_room_settings(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        self.set_room_settings_with_outbox(room_id, settings, None)
            .await
    }

    pub async fn set_room_settings_with_outbox(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
        outbox_event_factory: Option<RealtimeOutboxSettingsEventFactory>,
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        settings.validate()?;

        let (previous_settings, updated_settings, updated_version) =
            optimistic_retry::retry_with_optimistic_lock(
                Self::MAX_RETRIES,
                Self::BACKOFF_BASE_MS,
                "Settings update failed after maximum retry attempts",
                || async {
                    let outbox_event_factory = outbox_event_factory.clone();
                    let (current, version) =
                        self.room_settings_repo.get_with_version(room_id).await?;
                    let domain = CacheDomain::RoomSettings { room_id: *room_id };
                    let reservation = self.begin_room_settings_write(room_id, version).await?;
                    let mut tx = match self.pool.begin().await {
                        Ok(tx) => tx,
                        Err(error) => {
                            self.abort_room_settings_write(&domain, reservation.as_ref())
                                .await;
                            return Err(error.into());
                        }
                    };
                    let new_version = if let Some(reservation) = &reservation {
                        match self
                            .room_settings_repo
                            .set_settings_with_exact_version_with_executor(
                                room_id,
                                settings,
                                version,
                                reservation.version,
                                &mut *tx,
                            )
                            .await
                        {
                            Ok(new_version) => new_version,
                            Err(error) => {
                                self.abort_room_settings_write(&domain, Some(reservation))
                                    .await;
                                return Err(error);
                            }
                        }
                    } else {
                        self.room_settings_repo
                            .set_settings_with_version_with_executor(
                                room_id, settings, version, &mut *tx,
                            )
                            .await?
                    };
                    let outbox_event = outbox_event_factory
                        .as_ref()
                        .map(|factory| factory(settings, new_version))
                        .transpose()?;
                    if let Err(error) = self
                        .insert_realtime_outbox_tx(&mut tx, outbox_event.as_ref())
                        .await
                    {
                        self.abort_room_settings_write(&domain, reservation.as_ref())
                            .await;
                        return Err(error);
                    }
                    if let Err(error) = tx.commit().await {
                        self.abort_room_settings_write(&domain, reservation.as_ref())
                            .await;
                        return Err(error.into());
                    }
                    self.finalize_committed_room_settings_write_best_effort(
                        &domain,
                        reservation.as_ref(),
                        new_version,
                        "set_room_settings_with_outbox",
                    )
                    .await;
                    Ok((current, settings.clone(), new_version))
                },
            )
            .await?;
        self.finalize_room_settings_update(
            room_id,
            &previous_settings,
            &updated_settings,
            updated_version,
            None,
            "",
        )
        .await
    }

    pub async fn set_settings_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        settings: RoomSettings,
        outbox_event_factory: Option<RealtimeOutboxSettingsEventFactory>,
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        self.permission_service
            .check_permission(
                &room_id,
                &user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;

        settings.validate()?;

        self.room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
        let settings = std::sync::Arc::new(settings);

        let (previous_settings, updated_settings, updated_version) =
            optimistic_retry::retry_with_optimistic_lock_timeout(
                Self::MAX_RETRIES,
                Self::BACKOFF_BASE_MS,
                std::time::Duration::from_secs(Self::SETTINGS_UPDATE_TIMEOUT_SECS),
                "Settings update failed after maximum retry attempts",
                || {
                    let settings = settings.clone();
                    let outbox_event_factory = outbox_event_factory.clone();
                    async move {
                        let (current, version) =
                            self.room_settings_repo.get_with_version(&room_id).await?;
                        let updated_settings = (*settings).clone();
                        let mut tx = self.pool.begin().await?;
                        ensure_actor_has_room_permission_now_tx(
                            &mut tx,
                            &self.permission_service,
                            &room_id,
                            &user_id,
                            crate::models::RoomPermission::SET_ROOM_SETTINGS,
                        )
                        .await?;
                        let domain = CacheDomain::RoomSettings { room_id };
                        let reservation = self.begin_room_settings_write(&room_id, version).await?;
                        let new_version = if let Some(reservation) = &reservation {
                            match self
                                .room_settings_repo
                                .set_settings_with_exact_version_with_executor(
                                    &room_id,
                                    &updated_settings,
                                    version,
                                    reservation.version,
                                    &mut *tx,
                                )
                                .await
                            {
                                Ok(new_version) => new_version,
                                Err(error) => {
                                    self.abort_room_settings_write(&domain, Some(reservation))
                                        .await;
                                    return Err(error);
                                }
                            }
                        } else {
                            self.room_settings_repo
                                .set_settings_with_version_with_executor(
                                    &room_id,
                                    &updated_settings,
                                    version,
                                    &mut *tx,
                                )
                                .await?
                        };
                        let outbox_event = outbox_event_factory
                            .as_ref()
                            .map(|factory| factory(&updated_settings, new_version))
                            .transpose()?;
                        if let Err(error) = self
                            .insert_realtime_outbox_tx(&mut tx, outbox_event.as_ref())
                            .await
                        {
                            self.abort_room_settings_write(&domain, reservation.as_ref())
                                .await;
                            return Err(error);
                        }
                        if let Err(error) = tx.commit().await {
                            self.abort_room_settings_write(&domain, reservation.as_ref())
                                .await;
                            return Err(error.into());
                        }
                        self.finalize_committed_room_settings_write_best_effort(
                            &domain,
                            reservation.as_ref(),
                            new_version,
                            "set_settings_with_outbox",
                        )
                        .await;
                        Ok((current, updated_settings, new_version))
                    }
                },
            )
            .await?;

        let snapshot = self
            .finalize_room_settings_update(
                &room_id,
                &previous_settings,
                &updated_settings,
                updated_version,
                Some(&user_id),
                "",
            )
            .await?;

        self.write_audit_event(
            &user_id,
            &user_id.to_string(),
            AuditAction::RoomSettingsUpdated,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            AuditDetails {
                room_settings: Some(Box::new(snapshot.settings.clone())),
                ..Default::default()
            },
        )
        .await?;

        Ok(snapshot)
    }

    async fn finalize_room_settings_update(
        &self,
        room_id: &RoomId,
        previous_settings: &RoomSettings,
        updated_settings: &RoomSettings,
        version: i64,
        actor_user_id: Option<&UserId>,
        actor_username: &str,
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        self.run_post_apply_hooks_for_settings_update(room_id, previous_settings, updated_settings)
            .await;
        self.room_settings_service.invalidate_local(room_id).await;
        self.permission_service.invalidate_room_cache(room_id).await;
        self.notify_room_invalidation(room_id).await;
        self.notify_room_settings_invalidation(room_id).await;

        let subscriber_count = self.notification_service.notify_settings_updated(
            room_id,
            actor_user_id,
            actor_username,
            updated_settings.clone(),
            version,
        );
        super::outbox::log_if_no_local_subscribers(
            subscriber_count,
            room_id,
            "Room settings updated",
        );

        Ok(crate::cache::RoomSettingsSnapshot {
            settings: updated_settings.clone(),
            version,
        })
    }

    async fn run_post_apply_hooks_for_settings_update(
        &self,
        room_id: &RoomId,
        previous_settings: &RoomSettings,
        updated_settings: &RoomSettings,
    ) {
        use crate::service::notification::GuestKickReason;

        let guest_kick_reason =
            if previous_settings.allow_guest_join.0 && !updated_settings.allow_guest_join.0 {
                Some(GuestKickReason::RoomGuestModeDisabled)
            } else {
                None
            };

        if let Some(reason) = guest_kick_reason {
            if let Err(e) = self.revoke_all_guest_access(room_id, reason).await {
                tracing::warn!(
                    room_id = %room_id,
                    error = %e,
                    "Failed to revoke guest access after settings change"
                );
            }
        }
    }

    /// Reset room settings to default values with optimistic locking.
    pub async fn reset_room_settings(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        self.reset_room_settings_with_outbox(room_id, user_id, None)
            .await
    }

    pub async fn reset_room_settings_with_outbox(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        outbox_event_factory: Option<RealtimeOutboxSettingsEventFactory>,
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        self.permission_service
            .check_permission(
                room_id,
                user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;

        let default_settings = RoomSettings::default();

        let (previous_settings, updated_settings, updated_version) =
            optimistic_retry::retry_with_optimistic_lock(
                Self::MAX_RETRIES,
                Self::BACKOFF_BASE_MS,
                "Settings reset failed after maximum retry attempts",
                || async {
                    let outbox_event_factory = outbox_event_factory.clone();
                    let (current, version) =
                        self.room_settings_repo.get_with_version(room_id).await?;
                    let mut tx = self.pool.begin().await?;
                    ensure_actor_has_room_permission_now_tx(
                        &mut tx,
                        &self.permission_service,
                        room_id,
                        user_id,
                        crate::models::RoomPermission::SET_ROOM_SETTINGS,
                    )
                    .await?;
                    let domain = CacheDomain::RoomSettings { room_id: *room_id };
                    let reservation = self.begin_room_settings_write(room_id, version).await?;
                    let new_version = if let Some(reservation) = &reservation {
                        match self
                            .room_settings_repo
                            .set_settings_with_exact_version_with_executor(
                                room_id,
                                &default_settings,
                                version,
                                reservation.version,
                                &mut *tx,
                            )
                            .await
                        {
                            Ok(new_version) => new_version,
                            Err(error) => {
                                self.abort_room_settings_write(&domain, Some(reservation))
                                    .await;
                                return Err(error);
                            }
                        }
                    } else {
                        self.room_settings_repo
                            .set_settings_with_version_with_executor(
                                room_id,
                                &default_settings,
                                version,
                                &mut *tx,
                            )
                            .await?
                    };
                    let outbox_event = outbox_event_factory
                        .as_ref()
                        .map(|factory| factory(&default_settings, new_version))
                        .transpose()?;
                    if let Err(error) = self
                        .insert_realtime_outbox_tx(&mut tx, outbox_event.as_ref())
                        .await
                    {
                        self.abort_room_settings_write(&domain, reservation.as_ref())
                            .await;
                        return Err(error);
                    }
                    if let Err(error) = tx.commit().await {
                        self.abort_room_settings_write(&domain, reservation.as_ref())
                            .await;
                        return Err(error.into());
                    }
                    self.finalize_committed_room_settings_write_best_effort(
                        &domain,
                        reservation.as_ref(),
                        new_version,
                        "reset_room_settings_with_outbox",
                    )
                    .await;
                    Ok((current, default_settings.clone(), new_version))
                },
            )
            .await?;
        self.finalize_room_settings_update(
            room_id,
            &previous_settings,
            &updated_settings,
            updated_version,
            Some(user_id),
            "",
        )
        .await
    }
}
