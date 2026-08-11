use synctv_core::models::{ChatMessageEvent, ChatPinEvent, MediaId, PlaylistId, RoomId, UserId};
use synctv_realtime::sync::{CacheTarget, RealtimeEvent};

#[derive(Debug, Clone)]
pub enum ResourceInvalidation {
    PlaybackState,
    Playback(PlaybackInvalidation),
    RoomSettings,
    PlaylistItems,
    RoomMemberEvents,
    OnlineCount,
    OnlineEvent,
    ChatEvents {
        event: Box<ChatMessageEvent>,
    },
    /// Chat history changed without a replayable event payload.
    ChatEventsSnapshot,
    ChatPinEvents {
        event: Box<ChatPinEvent>,
    },
    ProviderCredential {
        user_id: UserId,
        provider: String,
        server_id: String,
    },
}

impl PartialEq for ResourceInvalidation {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::PlaybackState, Self::PlaybackState)
            | (Self::RoomSettings, Self::RoomSettings)
            | (Self::PlaylistItems, Self::PlaylistItems)
            | (Self::RoomMemberEvents, Self::RoomMemberEvents)
            | (Self::OnlineCount, Self::OnlineCount)
            | (Self::OnlineEvent, Self::OnlineEvent)
            | (Self::ChatEventsSnapshot, Self::ChatEventsSnapshot) => true,
            (Self::Playback(left), Self::Playback(right)) => left == right,
            (Self::ChatEvents { event: left }, Self::ChatEvents { event: right }) => {
                left.event_id == right.event_id
            }
            (Self::ChatPinEvents { event: left }, Self::ChatPinEvents { event: right }) => {
                left.event_id == right.event_id
            }
            (
                Self::ProviderCredential {
                    user_id: left_user,
                    provider: left_provider,
                    server_id: left_server,
                },
                Self::ProviderCredential {
                    user_id: right_user,
                    provider: right_provider,
                    server_id: right_server,
                },
            ) => {
                left_user == right_user
                    && left_provider == right_provider
                    && left_server == right_server
            }
            _ => false,
        }
    }
}

impl Eq for ResourceInvalidation {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackInvalidation {
    PlaybackStateChanged,
    MediaChanged { media_id: MediaId },
    LiveStreamChanged { media_id: MediaId },
    PlaylistChanged { playlist_id: PlaylistId },
    PlaylistItemsChanged { media_ids: Vec<MediaId> },
    Cache,
}

pub fn resource_invalidations_for_room_event(event: &RealtimeEvent) -> Vec<ResourceInvalidation> {
    match event {
        RealtimeEvent::PlaybackStateChanged { source_changed, .. } => {
            let mut invalidations = vec![ResourceInvalidation::PlaybackState];
            if *source_changed {
                invalidations.push(ResourceInvalidation::Playback(
                    PlaybackInvalidation::PlaybackStateChanged,
                ));
            }
            invalidations
        }
        RealtimeEvent::MediaUpdated { media_id, .. }
        | RealtimeEvent::MediaRemoved { media_id, .. } => vec![
            ResourceInvalidation::PlaylistItems,
            ResourceInvalidation::Playback(PlaybackInvalidation::MediaChanged {
                media_id: *media_id,
            }),
        ],
        RealtimeEvent::LiveStreamChanged { media_id, .. } => vec![ResourceInvalidation::Playback(
            PlaybackInvalidation::LiveStreamChanged {
                media_id: *media_id,
            },
        )],
        RealtimeEvent::PlaylistUpdated { playlist, .. } => vec![
            ResourceInvalidation::PlaylistItems,
            ResourceInvalidation::Playback(PlaybackInvalidation::PlaylistChanged {
                playlist_id: playlist.id,
            }),
        ],
        RealtimeEvent::MediaRemovedBatch { media_ids, .. }
        | RealtimeEvent::PlaylistReordered { media_ids, .. } => vec![
            ResourceInvalidation::PlaylistItems,
            ResourceInvalidation::Playback(PlaybackInvalidation::PlaylistItemsChanged {
                media_ids: media_ids.clone(),
            }),
        ],
        RealtimeEvent::PlaylistDeleted { playlist_id, .. } => vec![
            ResourceInvalidation::PlaylistItems,
            ResourceInvalidation::Playback(PlaybackInvalidation::PlaylistChanged {
                playlist_id: *playlist_id,
            }),
        ],
        RealtimeEvent::MediaAdded { .. } | RealtimeEvent::PlaylistCreated { .. } => {
            vec![ResourceInvalidation::PlaylistItems]
        }
        RealtimeEvent::RoomSettingsChanged { .. } => vec![ResourceInvalidation::RoomSettings],
        RealtimeEvent::UserJoined { .. }
        | RealtimeEvent::GuestJoined { .. }
        | RealtimeEvent::UserLeft { .. }
        | RealtimeEvent::GuestLeft { .. } => {
            vec![
                ResourceInvalidation::RoomMemberEvents,
                ResourceInvalidation::OnlineEvent,
            ]
        }
        RealtimeEvent::PermissionChanged { .. } | RealtimeEvent::KickUserFromRoom { .. } => {
            vec![ResourceInvalidation::RoomMemberEvents]
        }
        RealtimeEvent::ChatMessageEvent { event, .. } => {
            vec![ResourceInvalidation::ChatEvents {
                event: Box::new(event.clone()),
            }]
        }
        RealtimeEvent::ChatPinEvent { event, .. } => {
            vec![ResourceInvalidation::ChatPinEvents {
                event: Box::new(event.clone()),
            }]
        }
        _ => Vec::new(),
    }
}

pub fn resource_invalidations_for_cache_targets(
    targets: &[CacheTarget],
    room_id: RoomId,
    user_id: Option<UserId>,
) -> Vec<ResourceInvalidation> {
    let refresh_all = targets
        .iter()
        .any(|target| matches!(target, CacheTarget::All));
    let refresh_room = targets.iter().any(
        |target| matches!(target, CacheTarget::Room { room_id: target } if *target == room_id),
    );
    let refresh_user = user_id.is_some_and(|user_id| {
        targets.iter().any(|target| {
            matches!(
                target,
                CacheTarget::User { user_id: target } if *target == user_id
            )
        })
    });
    let refresh_username = targets
        .iter()
        .any(|target| matches!(target, CacheTarget::Username { .. }));

    let mut invalidations = Vec::new();
    if refresh_all || refresh_room {
        push_unique(&mut invalidations, ResourceInvalidation::PlaybackState);
        push_unique(
            &mut invalidations,
            ResourceInvalidation::Playback(PlaybackInvalidation::Cache),
        );
        push_unique(&mut invalidations, ResourceInvalidation::RoomSettings);
        push_unique(&mut invalidations, ResourceInvalidation::PlaylistItems);
        push_unique(&mut invalidations, ResourceInvalidation::ChatEventsSnapshot);
        push_unique(&mut invalidations, ResourceInvalidation::RoomMemberEvents);
        push_unique(&mut invalidations, ResourceInvalidation::OnlineCount);
    }
    if refresh_user {
        push_unique(
            &mut invalidations,
            ResourceInvalidation::Playback(PlaybackInvalidation::Cache),
        );
        push_unique(&mut invalidations, ResourceInvalidation::RoomMemberEvents);
    }
    if refresh_username {
        push_unique(&mut invalidations, ResourceInvalidation::RoomMemberEvents);
    }

    invalidations
}

pub fn provider_credential_resource_invalidation(
    user_id: UserId,
    provider: &str,
    server_id: &str,
) -> ResourceInvalidation {
    ResourceInvalidation::ProviderCredential {
        user_id,
        provider: provider.to_string(),
        server_id: server_id.to_string(),
    }
}

fn push_unique(invalidations: &mut Vec<ResourceInvalidation>, invalidation: ResourceInvalidation) {
    if !invalidations.contains(&invalidation) {
        invalidations.push(invalidation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_core::models::{
        ChatEventKind, ChatMessage, ChatMessageStatus, ChatMessageType, ChatMessageWithAttachments,
        Playlist, RoomPlaybackState,
    };

    fn room_id() -> RoomId {
        RoomId::expect_positive(101)
    }

    fn user_id() -> UserId {
        UserId::expect_positive(202)
    }

    fn chat_event() -> ChatMessageEvent {
        let now = synctv_core::SystemClock.now();
        ChatMessageEvent {
            event_id: "chat-event".to_string(),
            sequence: 1,
            room_id: room_id(),
            actor_user_id: user_id(),
            kind: ChatEventKind::Created,
            message: ChatMessageWithAttachments {
                message: ChatMessage {
                    id: 1,
                    room_id: room_id(),
                    user_id: Some(user_id()),
                    client_message_id: Some("client-message".to_string()),
                    content: "hello".to_string(),
                    message_type: ChatMessageType::User,
                    status: ChatMessageStatus::Active,
                    version: 1,
                    reply_to_message_id: None,
                    reply_to_message_created_at: None,
                    metadata: None,
                    edited_at: None,
                    deleted_at: None,
                    deleted_by: None,
                    delete_reason: None,
                    created_at: now,
                },
                attachments: Vec::new(),
                reactions: Vec::new(),
                mentions: Vec::new(),
                pin: None,
            },
            occurred_at: now,
        }
    }

    #[test]
    fn playback_state_event_invalidates_state_only() {
        let event = RealtimeEvent::PlaybackStateChanged {
            event_id: "evt".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "actor".to_string(),
            state: RoomPlaybackState::new(room_id()),
            source_changed: false,
            client_operation_id: None,
            timestamp: synctv_core::SystemClock.now(),
        };

        assert_eq!(
            resource_invalidations_for_room_event(&event),
            vec![ResourceInvalidation::PlaybackState]
        );
    }

    #[test]
    fn playback_source_event_invalidates_state_and_playback() {
        let event = RealtimeEvent::PlaybackStateChanged {
            event_id: "evt".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "actor".to_string(),
            state: RoomPlaybackState::new(room_id()),
            source_changed: true,
            client_operation_id: None,
            timestamp: synctv_core::SystemClock.now(),
        };

        assert_eq!(
            resource_invalidations_for_room_event(&event),
            vec![
                ResourceInvalidation::PlaybackState,
                ResourceInvalidation::Playback(PlaybackInvalidation::PlaybackStateChanged),
            ]
        );
    }

    #[test]
    fn live_stream_event_invalidates_its_playback_snapshot() {
        let media_id = MediaId::expect_positive(404);
        let event = RealtimeEvent::LiveStreamChanged {
            event_id: "evt-live".to_string(),
            room_id: room_id(),
            media_id,
            user_id: user_id(),
            generation_id: "generation-live".to_string(),
            is_live: true,
            timestamp: synctv_core::SystemClock.now(),
        };

        assert_eq!(
            resource_invalidations_for_room_event(&event),
            vec![ResourceInvalidation::Playback(
                PlaybackInvalidation::LiveStreamChanged { media_id }
            )]
        );
    }

    #[test]
    fn chat_message_event_invalidates_chat_events() {
        let event = chat_event();
        let realtime = RealtimeEvent::ChatMessageEvent {
            event_id: event.event_id.clone(),
            room_id: event.room_id,
            actor_user_id: event.actor_user_id,
            event: event.clone(),
            timestamp: event.occurred_at,
        };

        assert_eq!(
            resource_invalidations_for_room_event(&realtime),
            vec![ResourceInvalidation::ChatEvents {
                event: Box::new(event)
            }]
        );
    }

    fn playlist_with_id(playlist_id: PlaylistId) -> Playlist {
        Playlist {
            id: playlist_id,
            room_id: room_id(),
            creator_id: Some(user_id()),
            name: "list".to_string(),
            description: String::new(),
            cover_file_reference_id: None,
            parent_id: None,
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: synctv_core::SystemClock.now(),
            updated_at: synctv_core::SystemClock.now(),
            version: 1,
        }
    }

    #[test]
    fn playlist_update_invalidates_items_and_dependent_snapshot() {
        let playlist_id = PlaylistId::expect_positive(303);
        let event = RealtimeEvent::PlaylistUpdated {
            event_id: "evt".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "actor".to_string(),
            playlist: playlist_with_id(playlist_id),
            timestamp: synctv_core::SystemClock.now(),
        };

        assert_eq!(
            resource_invalidations_for_room_event(&event),
            vec![
                ResourceInvalidation::PlaylistItems,
                ResourceInvalidation::Playback(PlaybackInvalidation::PlaylistChanged {
                    playlist_id
                }),
            ]
        );
    }

    #[test]
    fn media_removed_invalidates_items_and_dependent_snapshot() {
        let media_id = MediaId::expect_positive(404);
        let event = RealtimeEvent::MediaRemoved {
            event_id: "evt".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "actor".to_string(),
            media_id,
            timestamp: synctv_core::SystemClock.now(),
        };

        assert_eq!(
            resource_invalidations_for_room_event(&event),
            vec![
                ResourceInvalidation::PlaylistItems,
                ResourceInvalidation::Playback(PlaybackInvalidation::MediaChanged { media_id }),
            ]
        );
    }

    #[test]
    fn playlist_item_batch_events_invalidate_items_and_dependent_snapshot() {
        let media_ids = vec![MediaId::expect_positive(404), MediaId::expect_positive(405)];
        let removed = RealtimeEvent::MediaRemovedBatch {
            event_id: "evt-remove".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "actor".to_string(),
            media_ids: media_ids.clone(),
            timestamp: synctv_core::SystemClock.now(),
        };
        let reordered = RealtimeEvent::PlaylistReordered {
            event_id: "evt-reorder".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "actor".to_string(),
            media_ids: media_ids.clone(),
            timestamp: synctv_core::SystemClock.now(),
        };
        let expected = vec![
            ResourceInvalidation::PlaylistItems,
            ResourceInvalidation::Playback(PlaybackInvalidation::PlaylistItemsChanged {
                media_ids,
            }),
        ];

        assert_eq!(resource_invalidations_for_room_event(&removed), expected);
        assert_eq!(resource_invalidations_for_room_event(&reordered), expected);
    }

    #[test]
    fn playlist_deleted_invalidates_items_and_dependent_snapshot() {
        let playlist_id = PlaylistId::expect_positive(505);
        let event = RealtimeEvent::PlaylistDeleted {
            event_id: "evt".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "actor".to_string(),
            playlist_id,
            timestamp: synctv_core::SystemClock.now(),
        };

        assert_eq!(
            resource_invalidations_for_room_event(&event),
            vec![
                ResourceInvalidation::PlaylistItems,
                ResourceInvalidation::Playback(PlaybackInvalidation::PlaylistChanged {
                    playlist_id
                }),
            ]
        );
    }

    #[test]
    fn cache_targets_are_reduced_to_connection_scoped_invalidations() {
        assert_eq!(
            resource_invalidations_for_cache_targets(
                &[
                    CacheTarget::Room { room_id: room_id() },
                    CacheTarget::User { user_id: user_id() },
                ],
                room_id(),
                Some(user_id()),
            ),
            vec![
                ResourceInvalidation::PlaybackState,
                ResourceInvalidation::Playback(PlaybackInvalidation::Cache),
                ResourceInvalidation::RoomSettings,
                ResourceInvalidation::PlaylistItems,
                ResourceInvalidation::ChatEventsSnapshot,
                ResourceInvalidation::RoomMemberEvents,
                ResourceInvalidation::OnlineCount,
            ]
        );

        assert!(resource_invalidations_for_cache_targets(
            &[CacheTarget::Room {
                room_id: RoomId::expect_positive(999)
            }],
            room_id(),
            Some(user_id()),
        )
        .is_empty());
    }

    #[test]
    fn username_cache_targets_refresh_member_events_for_all_room_observers() {
        assert_eq!(
            resource_invalidations_for_cache_targets(
                &[CacheTarget::Username {
                    user_id: UserId::expect_positive(999)
                }],
                room_id(),
                Some(user_id()),
            ),
            vec![ResourceInvalidation::RoomMemberEvents]
        );
    }

    #[test]
    fn membership_events_emit_online_events_without_eager_count_refresh() {
        let joined = RealtimeEvent::GuestJoined {
            event_id: "guest-joined".to_string(),
            room_id: room_id(),
            guest_id: "gst_test".to_string(),
            username: "Guest".to_string(),
            permissions: synctv_core::models::RoomPermissionSet::default_guest(),
            role: synctv_proto::common::RoomMemberRole::Guest as i32,
            joined_at: synctv_core::SystemClock.now(),
            timestamp: synctv_core::SystemClock.now(),
        };
        let left = RealtimeEvent::GuestLeft {
            event_id: "guest-left".to_string(),
            room_id: room_id(),
            guest_id: "gst_test".to_string(),
            username: "Guest".to_string(),
            timestamp: synctv_core::SystemClock.now(),
        };
        let kicked = RealtimeEvent::KickUserFromRoom {
            event_id: "kick-user-from-room".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            reason: "removed".to_string(),
            timestamp: synctv_core::SystemClock.now(),
        };
        let user_joined = RealtimeEvent::UserJoined {
            event_id: "user-joined".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "User".to_string(),
            remark_name: String::new(),
            display_tag: String::new(),
            permissions: synctv_core::models::RoomPermissionSet::default_member(),
            role: synctv_proto::common::RoomMemberRole::Member as i32,
            added_permissions: synctv_core::models::RoomPermissionSet(0),
            removed_permissions: synctv_core::models::RoomPermissionSet(0),
            admin_added_permissions: synctv_core::models::RoomPermissionSet(0),
            admin_removed_permissions: synctv_core::models::RoomPermissionSet(0),
            joined_at: synctv_core::SystemClock.now(),
            timestamp: synctv_core::SystemClock.now(),
        };
        let user_left = RealtimeEvent::UserLeft {
            event_id: "user-left".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "User".to_string(),
            remark_name: String::new(),
            display_tag: String::new(),
            role: synctv_proto::common::RoomMemberRole::Member as i32,
            timestamp: synctv_core::SystemClock.now(),
        };

        assert_eq!(
            resource_invalidations_for_room_event(&joined),
            vec![
                ResourceInvalidation::RoomMemberEvents,
                ResourceInvalidation::OnlineEvent,
            ]
        );
        assert_eq!(
            resource_invalidations_for_room_event(&left),
            vec![
                ResourceInvalidation::RoomMemberEvents,
                ResourceInvalidation::OnlineEvent,
            ]
        );
        assert_eq!(
            resource_invalidations_for_room_event(&user_joined),
            vec![
                ResourceInvalidation::RoomMemberEvents,
                ResourceInvalidation::OnlineEvent,
            ]
        );
        assert_eq!(
            resource_invalidations_for_room_event(&user_left),
            vec![
                ResourceInvalidation::RoomMemberEvents,
                ResourceInvalidation::OnlineEvent,
            ]
        );
        assert_eq!(
            resource_invalidations_for_room_event(&kicked),
            vec![ResourceInvalidation::RoomMemberEvents]
        );
    }
}
