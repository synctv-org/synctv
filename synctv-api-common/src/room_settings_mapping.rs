use std::collections::BTreeSet;

use synctv_proto::client as client_proto;

use crate::ApiError;

pub fn select_room_settings_patch(
    mut source: client_proto::RoomSettingsPatch,
    paths: &[String],
) -> Result<client_proto::RoomSettingsPatch, ApiError> {
    if paths.is_empty() {
        return Err(ApiError::InvalidInput(
            "update_mask.paths must not be empty".to_string(),
        ));
    }

    let defaults = synctv_core::models::RoomSettings::default();
    let mut selected = client_proto::RoomSettingsPatch::default();
    let mut seen = BTreeSet::new();

    macro_rules! select_scalar {
        ($field:ident, $default:expr) => {{
            selected.$field = Some(source.$field.take().unwrap_or($default));
        }};
    }

    for path in paths {
        if path.is_empty() {
            return Err(ApiError::InvalidInput(
                "update_mask paths must not be empty".to_string(),
            ));
        }
        if !seen.insert(path.as_str()) {
            return Err(ApiError::InvalidInput(format!(
                "duplicate update_mask path '{path}'"
            )));
        }

        match path.as_str() {
            "allow_guest_join" => {
                select_scalar!(allow_guest_join, defaults.allow_guest_join.0);
            }
            "max_members" => select_scalar!(max_members, defaults.max_members.0),
            "require_approval" => {
                select_scalar!(require_approval, defaults.require_approval.0);
            }
            "allow_auto_join" => {
                select_scalar!(allow_auto_join, defaults.allow_auto_join.0);
            }
            "chat_enabled" => select_scalar!(chat_enabled, defaults.chat_enabled.0),
            "auto_play.enabled" => {
                let source = source.auto_play.get_or_insert_default();
                selected.auto_play.get_or_insert_default().enabled = Some(
                    source
                        .enabled
                        .take()
                        .unwrap_or(defaults.auto_play.value.enabled),
                );
            }
            "auto_play.mode" => {
                let source = source.auto_play.get_or_insert_default();
                selected.auto_play.get_or_insert_default().mode = Some(
                    source
                        .mode
                        .take()
                        .unwrap_or(default_play_mode(&defaults.auto_play.value.mode)),
                );
            }
            "auto_play.delay" => {
                let source = source.auto_play.get_or_insert_default();
                selected.auto_play.get_or_insert_default().delay = Some(
                    source
                        .delay
                        .take()
                        .unwrap_or(defaults.auto_play.value.delay),
                );
            }
            "admin_added_permissions" => {
                select_scalar!(admin_added_permissions, defaults.admin_added_permissions.0);
            }
            "admin_removed_permissions" => select_scalar!(
                admin_removed_permissions,
                defaults.admin_removed_permissions.0
            ),
            "member_added_permissions" => select_scalar!(
                member_added_permissions,
                defaults.member_added_permissions.0
            ),
            "member_removed_permissions" => select_scalar!(
                member_removed_permissions,
                defaults.member_removed_permissions.0
            ),
            "guest_added_permissions" => {
                select_scalar!(guest_added_permissions, defaults.guest_added_permissions.0);
            }
            "guest_removed_permissions" => select_scalar!(
                guest_removed_permissions,
                defaults.guest_removed_permissions.0
            ),
            _ => {
                return Err(ApiError::InvalidInput(format!(
                    "unsupported update_mask path '{path}'"
                )));
            }
        }
    }

    Ok(selected)
}

fn default_play_mode(mode: &synctv_core::models::PlayMode) -> i32 {
    (match mode {
        synctv_core::models::PlayMode::Sequential => client_proto::PlayMode::Sequential,
        synctv_core::models::PlayMode::RepeatOne => client_proto::PlayMode::RepeatOne,
        synctv_core::models::PlayMode::RepeatAll => client_proto::PlayMode::RepeatAll,
        synctv_core::models::PlayMode::Shuffle => client_proto::PlayMode::Shuffle,
    }) as i32
}

#[cfg(test)]
mod tests {
    use super::select_room_settings_patch;

    #[test]
    fn selects_only_masked_room_settings_leaves() {
        let patch = select_room_settings_patch(
            synctv_proto::client::RoomSettingsPatch {
                require_approval: Some(true),
                chat_enabled: Some(false),
                auto_play: Some(synctv_proto::client::AutoPlaySettingsPatch {
                    mode: Some(synctv_proto::client::PlayMode::Shuffle as i32),
                    ..Default::default()
                }),
                ..Default::default()
            },
            &["require_approval".to_string(), "auto_play.mode".to_string()],
        )
        .expect("valid room settings mask");

        assert_eq!(patch.require_approval, Some(true));
        assert_eq!(patch.chat_enabled, None);
        assert_eq!(
            patch.auto_play.expect("auto play").mode,
            Some(synctv_proto::client::PlayMode::Shuffle as i32)
        );
    }

    #[test]
    fn unset_room_setting_uses_server_default() {
        let patch = select_room_settings_patch(
            synctv_proto::client::RoomSettingsPatch::default(),
            &["allow_auto_join".to_string()],
        )
        .expect("unset room setting");

        assert_eq!(
            patch.allow_auto_join,
            Some(
                synctv_core::models::RoomSettings::default()
                    .allow_auto_join
                    .0
            )
        );
    }

    #[test]
    fn rejects_invalid_room_settings_masks() {
        for paths in [
            Vec::<String>::new(),
            vec!["auto_play".to_string()],
            vec!["chat_enabled".to_string(), "chat_enabled".to_string()],
        ] {
            assert!(select_room_settings_patch(
                synctv_proto::client::RoomSettingsPatch::default(),
                &paths
            )
            .is_err());
        }
    }
}
