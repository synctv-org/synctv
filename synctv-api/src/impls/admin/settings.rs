use synctv_core::{models::UserId, provider::ExecutionControl, Error as CoreError};

use super::{AdminApiImpl, ApiError, RequestContext};

fn parse_raw_setting_value(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
}

impl AdminApiImpl {
    fn effective_settings_by_key(
        &self,
    ) -> Result<std::collections::BTreeMap<String, String>, ApiError> {
        let mut effective = std::collections::BTreeMap::new();
        let mut registered_keys = None;
        let mut visible_keys = None;

        if let Some(registry) = &self.settings_registry {
            let defaults = registry
                .storage
                .registered_defaults()
                .map_err(ApiError::from)?;
            visible_keys = Some(
                defaults
                    .iter()
                    .map(|(key, _)| key.clone())
                    .collect::<std::collections::HashSet<_>>(),
            );
            effective.extend(defaults);
            registered_keys = Some(
                registry
                    .storage
                    .registered_keys()
                    .into_iter()
                    .collect::<std::collections::HashSet<_>>(),
            );
        }

        for setting in self.settings_service.get_all().map_err(ApiError::from)? {
            if let Some(registered_keys) = &registered_keys {
                if visible_keys
                    .as_ref()
                    .is_some_and(|visible_keys| visible_keys.contains(&setting.key))
                {
                    effective.insert(setting.key, setting.value);
                    continue;
                }
                if registered_keys.contains(&setting.key) {
                    continue;
                }
                tracing::warn!(
                    key = %setting.key,
                    group = %setting.group_name,
                    "Ignoring unsupported persisted setting during admin settings projection"
                );
                continue;
            }
            effective.insert(setting.key, setting.value);
        }

        Ok(effective)
    }

    fn serialize_admin_settings_group(
        name: String,
        object: serde_json::Map<String, serde_json::Value>,
    ) -> Result<synctv_proto::admin::SettingsGroup, ApiError> {
        let settings = serde_json::to_vec(&serde_json::Value::Object(object)).map_err(|error| {
            ApiError::Internal(format!("Failed to encode settings group: {error}"))
        })?;
        Ok(synctv_proto::admin::SettingsGroup { name, settings })
    }

    fn project_settings_groups(
        effective: std::collections::BTreeMap<String, String>,
    ) -> Result<Vec<synctv_proto::admin::SettingsGroup>, ApiError> {
        let mut groups =
            std::collections::BTreeMap::<String, serde_json::Map<String, serde_json::Value>>::new();

        for (key, raw_value) in effective {
            let Some((group_name, setting_name)) = key.split_once('.') else {
                tracing::warn!(
                    key = %key,
                    "Skipping unsupported setting key without group prefix during admin projection"
                );
                continue;
            };
            groups.entry(group_name.to_string()).or_default().insert(
                setting_name.to_string(),
                parse_raw_setting_value(&raw_value),
            );
        }

        groups
            .into_iter()
            .map(|(name, object)| Self::serialize_admin_settings_group(name, object))
            .collect()
    }

    fn fully_qualified_setting_updates(
        group_name: &str,
        settings: std::collections::HashMap<String, String>,
    ) -> Result<Vec<(String, String)>, ApiError> {
        if group_name.trim().is_empty() {
            return Err(ApiError::InvalidInput(
                "settings group must not be empty".to_string(),
            ));
        }
        if settings.is_empty() {
            return Err(ApiError::InvalidInput(
                "settings update must contain at least one entry".to_string(),
            ));
        }

        let mut updates = Vec::with_capacity(settings.len());
        for (setting_name, value) in settings {
            let setting_name = setting_name.trim();
            if setting_name.is_empty() {
                return Err(ApiError::InvalidInput(
                    "settings key must not be empty".to_string(),
                ));
            }
            if setting_name.contains('.') {
                return Err(ApiError::InvalidInput(format!(
                    "settings key '{setting_name}' must not contain '.'"
                )));
            }
            updates.push((format!("{group_name}.{setting_name}"), value));
        }
        updates.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(updates)
    }

    pub async fn get_settings(
        &self,
        _req: synctv_proto::admin::GetSettingsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::GetSettingsResponse, ApiError> {
        let group_list = Self::project_settings_groups(self.effective_settings_by_key()?)?;
        let group_names: Vec<String> = group_list.iter().map(|g| g.name.clone()).collect();

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::SettingsViewed,
            synctv_core::models::AuditTargetType::Settings,
            None,
            serde_json::json!({
                "group_count": group_names.len(),
                "groups": group_names,
            }),
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::GetSettingsResponse { groups: group_list })
    }

    pub async fn get_settings_group(
        &self,
        req: synctv_proto::admin::GetSettingsGroupRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::GetSettingsGroupResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let requested_group = req.group.trim();

        let group = Self::project_settings_groups(self.effective_settings_by_key()?)?
            .into_iter()
            .find(|group| group.name == requested_group)
            .ok_or_else(|| {
                ApiError::NotFound(format!("Settings group '{requested_group}' not found"))
            })?;

        let group_name = group.name.clone();

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::SettingsGroupViewed,
            synctv_core::models::AuditTargetType::Settings,
            None,
            serde_json::json!({ "group": group_name }),
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::GetSettingsGroupResponse { group: Some(group) })
    }

    pub async fn update_settings(
        &self,
        req: synctv_proto::admin::UpdateSettingsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::UpdateSettingsResponse, ApiError> {
        let group_name = req.group.trim().to_string();
        let updates = Self::fully_qualified_setting_updates(&group_name, req.settings)?;
        let changed_keys: Vec<String> = updates.iter().map(|(key, _)| key.clone()).collect();

        self.settings_service
            .update_batch(updates)
            .await
            .map_err(ApiError::from)?;

        if !self.room_cache_fanout.try_publish_all_invalidation().await {
            tracing::warn!(
                group = %group_name,
                changed_keys = ?changed_keys,
                "Failed to publish global room cache invalidation after settings update"
            );
        }

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::SettingsUpdated,
            synctv_core::models::AuditTargetType::Settings,
            None,
            serde_json::json!({ "changed_keys": changed_keys }),
            ctx,
        )
        .await;

        let group = Self::project_settings_groups(self.effective_settings_by_key()?)?
            .into_iter()
            .find(|group| group.name == group_name)
            .ok_or_else(|| {
                ApiError::NotFound(format!("Settings group '{group_name}' not found"))
            })?;

        Ok(synctv_proto::admin::UpdateSettingsResponse { group: Some(group) })
    }

    pub(in crate::impls::admin) fn map_send_test_email_result(
        to: &str,
        result: Result<(), CoreError>,
    ) -> synctv_proto::admin::SendTestEmailResponse {
        match result {
            Ok(()) => synctv_proto::admin::SendTestEmailResponse {
                message: format!("Test email sent successfully to {to}"),
                success: true,
            },
            Err(error) => {
                tracing::error!(email = %to, error = %error, "Failed to send test email");
                synctv_proto::admin::SendTestEmailResponse {
                    message: "Failed to send test email. Please verify the email configuration and try again.".to_string(),
                    success: false,
                }
            }
        }
    }

    pub async fn send_test_email(
        &self,
        req: synctv_proto::admin::SendTestEmailRequest,
    ) -> Result<synctv_proto::admin::SendTestEmailResponse, ApiError> {
        self.send_test_email_with_control(req, None).await
    }

    pub async fn send_test_email_with_control(
        &self,
        req: synctv_proto::admin::SendTestEmailRequest,
        control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::admin::SendTestEmailResponse, ApiError> {
        Ok(Self::map_send_test_email_result(
            &req.to,
            self.email_service
                .send_test_email_with_control(&req.to, control)
                .await,
        ))
    }

    pub async fn get_room_settings(
        &self,
        req: synctv_proto::admin::GetRoomSettingsRequest,
    ) -> Result<synctv_proto::admin::GetRoomSettingsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = crate::impls::proto_validated_room_id(req.room_id, &self.public_id_codec)?;
        let (settings, version) = self
            .room_service
            .get_room_settings_with_version(&rid)
            .await
            .map_err(ApiError::from)?;
        let settings_json = serde_json::to_vec(&settings).map_err(ApiError::from)?;

        Ok(synctv_proto::admin::GetRoomSettingsResponse {
            settings: settings_json,
            version,
        })
    }

    pub async fn update_room_settings(
        &self,
        req: synctv_proto::admin::UpdateRoomSettingsRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::admin::UpdateRoomSettingsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid =
            crate::impls::proto_validated_room_id(req.room_id.clone(), &self.public_id_codec)?;
        let settings: synctv_core::models::RoomSettings = serde_json::from_slice(&req.settings)
            .map_err(|e| ApiError::InvalidInput(format!("Invalid settings JSON: {e}")))?;
        let admin_actor = self.require_admin_actor(admin_user_id).await?;
        let admin_username = admin_actor.username;
        let (_, current_version) = self
            .room_service
            .get_room_settings_with_version(&rid)
            .await
            .map_err(ApiError::from)?;
        let settings_json = serde_json::to_vec(&settings).map_err(ApiError::from)?;
        let prepared_settings_fanout = self.room_settings_fanout.prepare_settings_changed(
            &rid,
            admin_user_id,
            &admin_username,
            settings_json,
            current_version + 1,
        )?;
        let snapshot = self
            .room_service
            .set_room_settings_with_outbox(
                &rid,
                &settings,
                Some(prepared_settings_fanout.settings_outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;

        self.room_settings_fanout
            .publish_prepared_after_outbox_commit(
                prepared_settings_fanout.with_version(snapshot.version)?,
            );
        self.publish_room_cache_invalidation(&rid);

        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;
        Ok(synctv_proto::admin::UpdateRoomSettingsResponse {
            room: Some(
                self.load_admin_room_proto(&room, Some(&snapshot.settings))
                    .await?,
            ),
        })
    }

    pub async fn reset_room_settings(
        &self,
        req: synctv_proto::admin::ResetRoomSettingsRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::admin::ResetRoomSettingsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = crate::impls::proto_validated_room_id(req.room_id, &self.public_id_codec)?;
        let default_settings = synctv_core::models::RoomSettings::default();
        let admin_actor = self.require_admin_actor(admin_user_id).await?;
        let admin_username = admin_actor.username;
        let (_, current_version) = self
            .room_service
            .get_room_settings_with_version(&rid)
            .await
            .map_err(ApiError::from)?;
        let settings_json = serde_json::to_vec(&default_settings).map_err(ApiError::from)?;
        let prepared_settings_fanout = self.room_settings_fanout.prepare_settings_changed(
            &rid,
            admin_user_id,
            &admin_username,
            settings_json,
            current_version + 1,
        )?;
        let snapshot = self
            .room_service
            .set_room_settings_with_outbox(
                &rid,
                &default_settings,
                Some(prepared_settings_fanout.settings_outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;

        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;
        self.room_settings_fanout
            .publish_prepared_after_outbox_commit(
                prepared_settings_fanout.with_version(snapshot.version)?,
            );
        self.publish_room_cache_invalidation(&rid);

        Ok(synctv_proto::admin::ResetRoomSettingsResponse {
            room: Some(
                self.load_admin_room_proto(&room, Some(&snapshot.settings))
                    .await?,
            ),
        })
    }
}
