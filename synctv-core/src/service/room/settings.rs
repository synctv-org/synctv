use crate::{
    cache::CacheDomain,
    models::{
        AuditAction, AuditDetails, AuditTargetType, RoomId, RoomSettings,
        SettingsValidationContext, UserId,
    },
    service::optimistic_retry,
    Error, Result,
};

use super::{RealtimeOutboxSettingsEventFactory, RoomService};

/// Maximum retry attempts for optimistic lock conflicts on settings updates.
const SETTINGS_UPDATE_MAX_RETRIES: u32 = 3;
/// Base backoff in milliseconds (exponential: 5ms, 10ms, 20ms).
const SETTINGS_UPDATE_BACKOFF_BASE_MS: u64 = 5;
/// Total timeout for settings updates with retries.
const SETTINGS_UPDATE_TIMEOUT_SECS: u64 = 5;

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
                crate::models::RoomPermission::MANAGE_ROOM_SETTINGS,
            )
            .await?;

        self.validate_room_settings(&settings)?;

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
                SETTINGS_UPDATE_MAX_RETRIES,
                SETTINGS_UPDATE_BACKOFF_BASE_MS,
                std::time::Duration::from_secs(SETTINGS_UPDATE_TIMEOUT_SECS),
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

    /// Set room settings (replace entire settings object) with optimistic locking.
    pub async fn set_room_settings(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        self.manage_room_settings_with_outbox(room_id, settings, None)
            .await
    }

    pub async fn manage_room_settings_with_outbox(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
        outbox_event_factory: Option<RealtimeOutboxSettingsEventFactory>,
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        SettingsValidationContext::with_strict_policy(|ctx| settings.validate(ctx))?;

        let (previous_settings, updated_settings, updated_version) =
            optimistic_retry::retry_with_optimistic_lock(
                SETTINGS_UPDATE_MAX_RETRIES,
                SETTINGS_UPDATE_BACKOFF_BASE_MS,
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
                        "manage_room_settings_with_outbox",
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
                crate::models::RoomPermission::MANAGE_ROOM_SETTINGS,
            )
            .await?;

        self.validate_room_settings(&settings)?;

        self.room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
        let settings = std::sync::Arc::new(settings);

        let (previous_settings, updated_settings, updated_version) =
            optimistic_retry::retry_with_optimistic_lock_timeout(
                SETTINGS_UPDATE_MAX_RETRIES,
                SETTINGS_UPDATE_BACKOFF_BASE_MS,
                std::time::Duration::from_secs(SETTINGS_UPDATE_TIMEOUT_SECS),
                "Settings update failed after maximum retry attempts",
                || {
                    let settings = settings.clone();
                    let outbox_event_factory = outbox_event_factory.clone();
                    async move {
                        let (current, version) =
                            self.room_settings_repo.get_with_version(&room_id).await?;
                        let updated_settings = (*settings).clone();
                        let mut tx = self.pool.begin().await?;
                        self.ensure_actor_has_room_permission_now_tx(
                            &mut tx,
                            &room_id,
                            &user_id,
                            crate::models::RoomPermission::MANAGE_ROOM_SETTINGS,
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
                crate::models::RoomPermission::MANAGE_ROOM_SETTINGS,
            )
            .await?;

        let default_settings = RoomSettings::default();

        let (previous_settings, updated_settings, updated_version) =
            optimistic_retry::retry_with_optimistic_lock(
                SETTINGS_UPDATE_MAX_RETRIES,
                SETTINGS_UPDATE_BACKOFF_BASE_MS,
                "Settings reset failed after maximum retry attempts",
                || async {
                    let outbox_event_factory = outbox_event_factory.clone();
                    let (current, version) =
                        self.room_settings_repo.get_with_version(room_id).await?;
                    let mut tx = self.pool.begin().await?;
                    self.ensure_actor_has_room_permission_now_tx(
                        &mut tx,
                        room_id,
                        user_id,
                        crate::models::RoomPermission::MANAGE_ROOM_SETTINGS,
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
