//! Adapter bridging room service events to runtime realtime delivery.

use synctv_realtime::sync::RealtimeEvent;

fn system_user_id() -> synctv_core::models::UserId {
    synctv_core::models::UserId::MAX
}

fn bridge_user_id(user_id: Option<&synctv_core::models::UserId>) -> synctv_core::models::UserId {
    user_id.copied().unwrap_or_else(system_user_id)
}

#[must_use]
pub fn room_event_to_realtime_event(
    room_id: &synctv_core::models::RoomId,
    event: &synctv_core::service::RoomEvent,
) -> Option<RealtimeEvent> {
    let timestamp = synctv_core::SystemClock.now();
    match event {
        synctv_core::service::RoomEvent::MediaAdded {
            user_id,
            username,
            media_id,
            title,
            ..
        } => Some(RealtimeEvent::MediaAdded {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: *user_id,
            username: username.clone(),
            media_id: *media_id,
            media_title: title.clone(),
            timestamp,
        }),
        synctv_core::service::RoomEvent::MediaRemoved {
            user_id,
            username,
            media_id,
        } => Some(RealtimeEvent::MediaRemoved {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: bridge_user_id(user_id.as_ref()),
            username: username.clone(),
            media_id: *media_id,
            timestamp,
        }),
        synctv_core::service::RoomEvent::MediaUpdated {
            user_id,
            username,
            media_id,
            title,
            ..
        } => Some(RealtimeEvent::MediaUpdated {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: *user_id,
            username: username.clone(),
            media_id: *media_id,
            media_title: title.clone(),
            timestamp,
        }),
        synctv_core::service::RoomEvent::PlaylistReordered {
            user_id,
            username,
            media_ids,
        } => Some(RealtimeEvent::PlaylistReordered {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: bridge_user_id(user_id.as_ref()),
            username: username.clone(),
            media_ids: media_ids.clone(),
            timestamp,
        }),
        synctv_core::service::RoomEvent::PlaylistDeleted {
            user_id,
            username,
            playlist_id,
        } => Some(RealtimeEvent::PlaylistDeleted {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: bridge_user_id(user_id.as_ref()),
            username: username.clone(),
            playlist_id: *playlist_id,
            timestamp,
        }),
        synctv_core::service::RoomEvent::UserJoined { user_id, username } => {
            Some(RealtimeEvent::UserJoined {
                event_id: synctv_common::snanoid!(16),
                room_id: *room_id,
                user_id: *user_id,
                username: username.clone(),
                remark_name: String::new(),
                display_tag: String::new(),
                permissions: synctv_core::models::RoomPermissionSet::default_member(),
                role: synctv_core::models::RoomRole::Member,
                added_permissions: synctv_core::models::RoomPermissionSet(0),
                removed_permissions: synctv_core::models::RoomPermissionSet(0),
                admin_added_permissions: synctv_core::models::RoomPermissionSet(0),
                admin_removed_permissions: synctv_core::models::RoomPermissionSet(0),
                joined_at: timestamp,
                timestamp,
            })
        }
        synctv_core::service::RoomEvent::UserLeft { user_id, username } => {
            Some(RealtimeEvent::UserLeft {
                event_id: synctv_common::snanoid!(16),
                room_id: *room_id,
                user_id: *user_id,
                username: username.clone(),
                remark_name: String::new(),
                display_tag: String::new(),
                role: synctv_core::models::RoomRole::Member,
                timestamp,
            })
        }
        synctv_core::service::RoomEvent::PermissionChanged {
            user_id,
            role,
            effective_permissions,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
            updated_by_user_id,
            updated_by_username,
        } => Some(RealtimeEvent::PermissionChanged {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            target_user_id: *user_id,
            target_username: String::new(),
            target_remark_name: String::new(),
            target_display_tag: String::new(),
            changed_by: *updated_by_user_id,
            changed_by_username: updated_by_username.clone(),
            role_changed: true,
            new_permissions: synctv_core::models::RoomPermissionSet(*effective_permissions),
            role: *role,
            added_permissions: synctv_core::models::RoomPermissionSet(*added_permissions),
            removed_permissions: synctv_core::models::RoomPermissionSet(*removed_permissions),
            admin_added_permissions: synctv_core::models::RoomPermissionSet(
                *admin_added_permissions,
            ),
            admin_removed_permissions: synctv_core::models::RoomPermissionSet(
                *admin_removed_permissions,
            ),
            target_is_online: false,
            target_connection_count: 0,
            timestamp,
        }),
        synctv_core::service::RoomEvent::SettingsUpdated {
            settings,
            version,
            user_id,
            username,
        } => Some(RealtimeEvent::RoomSettingsChanged {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: bridge_user_id(user_id.as_ref()),
            username: username.clone(),
            settings: settings.clone(),
            version: *version,
            timestamp,
        }),
        synctv_core::service::RoomEvent::RoomDeleted => Some(RealtimeEvent::RoomDeleted {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            deleted_by: system_user_id(),
            timestamp,
        }),
        synctv_core::service::RoomEvent::ChatMessage { .. }
        | synctv_core::service::RoomEvent::MemberKicked { .. }
        | synctv_core::service::RoomEvent::GuestKicked { .. }
        | synctv_core::service::RoomEvent::StreamStarted { .. }
        | synctv_core::service::RoomEvent::StreamStopped { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::room_event_to_realtime_event;
    use synctv_core::models::{MediaId, PlaylistId, RoomId, UserId};
    use synctv_realtime::sync::RealtimeEvent;

    #[test]
    fn test_room_event_to_realtime_event_maps_room_deleted() {
        let room_id = RoomId::expect_positive(120_007);

        let event =
            room_event_to_realtime_event(&room_id, &synctv_core::service::RoomEvent::RoomDeleted)
                .expect("RoomDeleted should bridge to RealtimeEvent");

        match event {
            RealtimeEvent::RoomDeleted { room_id, .. } => {
                assert_eq!(room_id, RoomId::expect_positive(120_007));
            }
            other => panic!("expected RoomDeleted, got {other:?}"),
        }
    }

    #[test]
    fn test_room_event_to_realtime_event_maps_user_left() {
        let room_id = RoomId::expect_positive(120_008);
        let user_id = UserId::expect_positive(120_009);

        let event = room_event_to_realtime_event(
            &room_id,
            &synctv_core::service::RoomEvent::UserLeft {
                user_id,
                username: "left-user".to_string(),
            },
        )
        .expect("UserLeft should bridge to RealtimeEvent");

        match event {
            RealtimeEvent::UserLeft {
                room_id,
                user_id,
                username,
                ..
            } => {
                assert_eq!(room_id, RoomId::expect_positive(120_008));
                assert_eq!(user_id, UserId::expect_positive(120_009));
                assert_eq!(username, "left-user");
            }
            other => panic!("expected UserLeft, got {other:?}"),
        }
    }

    #[test]
    fn test_room_event_to_realtime_event_maps_playlist_deleted() {
        let room_id = RoomId::expect_positive(120_010);
        let user_id = UserId::expect_positive(120_011);

        let event = room_event_to_realtime_event(
            &room_id,
            &synctv_core::service::RoomEvent::PlaylistDeleted {
                user_id: Some(user_id),
                username: "tester".to_string(),
                playlist_id: PlaylistId::expect_positive(120_012),
            },
        )
        .expect("PlaylistDeleted should bridge to RealtimeEvent");

        match event {
            RealtimeEvent::PlaylistDeleted {
                room_id,
                user_id,
                username,
                playlist_id,
                ..
            } => {
                assert_eq!(room_id, RoomId::expect_positive(120_010));
                assert_eq!(user_id, UserId::expect_positive(120_011));
                assert_eq!(username, "tester");
                assert_eq!(playlist_id, PlaylistId::expect_positive(120_012));
            }
            other => panic!("expected PlaylistDeleted, got {other:?}"),
        }
    }

    #[test]
    fn test_room_event_to_realtime_event_maps_user_joined() {
        let room_id = RoomId::expect_positive(120_013);
        let user_id = UserId::expect_positive(120_014);

        let event = room_event_to_realtime_event(
            &room_id,
            &synctv_core::service::RoomEvent::UserJoined {
                user_id,
                username: "joiner".to_string(),
            },
        )
        .expect("UserJoined should bridge to RealtimeEvent");

        match event {
            RealtimeEvent::UserJoined {
                room_id,
                user_id,
                username,
                role,
                ..
            } => {
                assert_eq!(room_id, RoomId::expect_positive(120_013));
                assert_eq!(user_id, UserId::expect_positive(120_014));
                assert_eq!(username, "joiner");
                assert_eq!(role, synctv_core::models::RoomRole::Member);
            }
            other => panic!("expected UserJoined, got {other:?}"),
        }
    }

    #[test]
    fn test_room_event_bridge_keeps_direct_realtime_events_explicitly_unmapped() {
        let room_id = RoomId::expect_positive(120_013);
        let user_id = UserId::expect_positive(120_014);

        let events = [synctv_core::service::RoomEvent::ChatMessage {
            message_id: "chat-1".to_string(),
            user_id,
            username: "chat-user".to_string(),
            content: "hello".to_string(),
            timestamp: synctv_core::SystemClock.now(),
        }];

        for event in events {
            assert!(
                room_event_to_realtime_event(&room_id, &event).is_none(),
                "{} has a direct realtime broadcaster and must not be bridged twice",
                event.event_type()
            );
        }
    }

    #[test]
    fn test_room_event_bridge_keeps_non_protocol_events_explicitly_unmapped() {
        let room_id = RoomId::expect_positive(120_016);
        let user_id = UserId::expect_positive(120_017);

        let events = [
            synctv_core::service::RoomEvent::MemberKicked { user_id },
            synctv_core::service::RoomEvent::GuestKicked {
                reason: synctv_core::service::GuestKickReason::AdminKick,
                message: "guest removed".to_string(),
            },
            synctv_core::service::RoomEvent::StreamStarted {
                media_id: MediaId::expect_positive(120_018),
                user_id,
            },
            synctv_core::service::RoomEvent::StreamStopped {
                media_id: MediaId::expect_positive(120_019),
                user_id,
            },
        ];

        for event in events {
            assert!(
                room_event_to_realtime_event(&room_id, &event).is_none(),
                "{} has no stable ServerMessage protocol mapping yet",
                event.event_type()
            );
        }
    }
}
