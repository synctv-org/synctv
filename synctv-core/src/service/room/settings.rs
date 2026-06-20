use crate::{
    cache::{CacheDomain, ConsistencyCoordinator, VersionFenceReservation},
    models::{AuditAction, AuditTargetType, RoomId, RoomSettings, UserId},
    service::optimistic_retry,
    Error, InternalExt, Result,
};

use super::{
    ensure_actor_has_room_permission_now_tx, merge_json_object_patch,
    RealtimeOutboxSettingsEventFactory, RoomService,
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
            let settings_json = serde_json::to_value(&snapshot.settings)
                .internal_with_err("Failed to serialize settings")?;
            self.write_audit_event(
                &user_id,
                &user_id.to_string(),
                AuditAction::RoomSettingsUpdated,
                AuditTargetType::Room,
                Some(room_id.to_string()),
                settings_json,
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

    /// Patch room settings with optimistic locking.
    ///
    /// The patch is merged into the current stored settings inside each CAS retry,
    /// so concurrent updates to different fields are preserved instead of being
    /// overwritten by a stale pre-merge snapshot.
    pub async fn patch_settings(
        &self,
        room_id: RoomId,
        user_id: UserId,
        patch: serde_json::Value,
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        self.patch_settings_with_outbox(room_id, user_id, patch, None)
            .await
    }

    pub async fn patch_settings_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        patch: serde_json::Value,
        outbox_event_factory: Option<RealtimeOutboxSettingsEventFactory>,
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        self.permission_service
            .check_permission(
                &room_id,
                &user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;

        self.room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
        let patch = std::sync::Arc::new(patch);

        let (previous_settings, updated_settings, updated_version) =
            optimistic_retry::retry_with_optimistic_lock_timeout(
                Self::MAX_RETRIES,
                Self::BACKOFF_BASE_MS,
                std::time::Duration::from_secs(Self::SETTINGS_UPDATE_TIMEOUT_SECS),
                "Settings patch failed after maximum retry attempts",
                || {
                    let patch = patch.clone();
                    let outbox_event_factory = outbox_event_factory.clone();
                    async move {
                        let (current, version) =
                            self.room_settings_repo.get_with_version(&room_id).await?;
                        let mut merged_json = serde_json::to_value(&current)
                            .internal_with_err("Failed to serialize current room settings")?;
                        merge_json_object_patch(&mut merged_json, (*patch).clone())?;
                        let merged_settings: RoomSettings = serde_json::from_value(merged_json)
                            .map_err(|e| {
                                Error::InvalidInput(format!("Invalid settings JSON: {e}"))
                            })?;
                        merged_settings.validate()?;
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
                                    &merged_settings,
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
                                    &merged_settings,
                                    version,
                                    &mut *tx,
                                )
                                .await?
                        };
                        let outbox_event = outbox_event_factory
                            .as_ref()
                            .map(|factory| factory(&merged_settings, new_version))
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
                            "patch_settings_with_outbox",
                        )
                        .await;
                        Ok((current, merged_settings, new_version))
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

        let settings_json = serde_json::to_value(&snapshot.settings)
            .internal_with_err("Failed to serialize settings")?;
        self.write_audit_event(
            &user_id,
            &user_id.to_string(),
            AuditAction::RoomSettingsUpdated,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            settings_json,
        )
        .await?;

        Ok(snapshot)
    }

    /// Update single room setting by key (requires `SET_ROOM_SETTINGS` permission)
    ///
    /// The flow is fully generic -- no per-setting special cases here:
    /// 1. Permission check
    /// 2. Registry validates type + value constraints (incl. macro validators)
    /// 3. CAS (Compare-And-Swap) update with automatic retry on version conflict
    /// 4. Post-apply hooks handle side effects (e.g., kick guests)
    pub async fn update_room_setting(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        key: &str,
        value: &str,
    ) -> Result<String> {
        use crate::models::room_settings::RoomSettingsRegistry;

        // 1. Permission check
        self.permission_service
            .check_permission(
                room_id,
                user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;

        // 2. Validate via registry (type parsing + value constraints from macro validators)
        RoomSettingsRegistry::validate_setting(key, value)?;

        // 3. CAS update with retry
        let (previous_settings, settings, version) = optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "Settings update failed after maximum retry attempts",
            || async {
                let (mut settings, version) =
                    self.room_settings_repo.get_with_version(room_id).await?;
                let current = settings.clone();
                settings.set_by_key(key, value)?;
                settings.validate()?;

                let domain = CacheDomain::RoomSettings { room_id: *room_id };
                let reservation = self.begin_room_settings_write(room_id, version).await?;
                let new_version = if let Some(reservation) = &reservation {
                    match self
                        .room_settings_repo
                        .set_settings_with_exact_version(
                            room_id,
                            &settings,
                            version,
                            reservation.version,
                        )
                        .await
                    {
                        Ok(new_version) => {
                            self.finalize_committed_room_settings_write_best_effort(
                                &domain,
                                Some(reservation),
                                new_version,
                                "update_room_setting",
                            )
                            .await;
                            new_version
                        }
                        Err(error) => {
                            self.abort_room_settings_write(&domain, Some(reservation))
                                .await;
                            return Err(error);
                        }
                    }
                } else {
                    self.room_settings_repo
                        .set_settings_with_version(room_id, &settings, version)
                        .await?
                };
                Ok((current, settings, new_version))
            },
        )
        .await?;

        let snapshot = self
            .finalize_room_settings_update(
                room_id,
                &previous_settings,
                &settings,
                version,
                Some(user_id),
                "",
            )
            .await?;

        serde_json::to_string(&snapshot.settings).internal_with_err("Failed to serialize settings")
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

        let settings_json = serde_json::to_value(updated_settings)
            .internal_with_err("Failed to serialize settings")?;
        let subscriber_count = self.notification_service.notify_settings_updated(
            room_id,
            actor_user_id,
            actor_username,
            settings_json.clone(),
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
