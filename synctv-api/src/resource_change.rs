use synctv_core::models::{ChatMessageEvent, MediaId, PlaylistId, RoomId, UserId};
use synctv_realtime::sync::{CacheTarget, RealtimeEvent};

#[derive(Debug, Clone)]
pub enum ResourceInvalidation {
    PlaybackState,
    PlaybackSnapshot(PlaybackSnapshotInvalidation),
    RoomSettings,
    PlaylistItems,
    RoomMembers,
    ChatEvents {
        event: Box<ChatMessageEvent>,
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
            | (Self::RoomMembers, Self::RoomMembers) => true,
            (Self::PlaybackSnapshot(left), Self::PlaybackSnapshot(right)) => left == right,
            (Self::ChatEvents { event: left }, Self::ChatEvents { event: right }) => {
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
pub enum PlaybackSnapshotInvalidation {
    PlaybackStateChanged,
    MediaUpdated { media_id: MediaId },
    PlaylistUpdated { playlist_id: PlaylistId },
    Cache,
}

pub fn resource_invalidations_for_room_event(event: &RealtimeEvent) -> Vec<ResourceInvalidation> {
    match event {
        RealtimeEvent::PlaybackStateChanged { .. } => vec![
            ResourceInvalidation::PlaybackState,
            ResourceInvalidation::PlaybackSnapshot(
                PlaybackSnapshotInvalidation::PlaybackStateChanged,
            ),
        ],
        RealtimeEvent::MediaUpdated { media_id, .. } => vec![
            ResourceInvalidation::PlaylistItems,
            ResourceInvalidation::PlaybackSnapshot(PlaybackSnapshotInvalidation::MediaUpdated {
                media_id: *media_id,
            }),
        ],
        RealtimeEvent::PlaylistUpdated { playlist, .. } => vec![
            ResourceInvalidation::PlaylistItems,
            ResourceInvalidation::PlaybackSnapshot(PlaybackSnapshotInvalidation::PlaylistUpdated {
                playlist_id: playlist.id,
            }),
        ],
        RealtimeEvent::MediaAdded { .. }
        | RealtimeEvent::MediaRemoved { .. }
        | RealtimeEvent::MediaRemovedBatch { .. }
        | RealtimeEvent::PlaylistCreated { .. }
        | RealtimeEvent::PlaylistDeleted { .. }
        | RealtimeEvent::PlaylistReordered { .. } => {
            vec![ResourceInvalidation::PlaylistItems]
        }
        RealtimeEvent::RoomSettingsChanged { .. } => vec![
            ResourceInvalidation::RoomSettings,
            ResourceInvalidation::RoomMembers,
        ],
        RealtimeEvent::UserJoined { .. }
        | RealtimeEvent::GuestJoined { .. }
        | RealtimeEvent::UserLeft { .. }
        | RealtimeEvent::GuestLeft { .. }
        | RealtimeEvent::PermissionChanged { .. } => vec![ResourceInvalidation::RoomMembers],
        RealtimeEvent::ChatMessageEvent { event, .. } => {
            vec![ResourceInvalidation::ChatEvents {
                event: Box::new(event.clone()),
            }]
        }
        _ => Vec::new(),
    }
}

pub fn resource_invalidations_for_cache_targets(
    targets: &[CacheTarget],
    room_id: RoomId,
    user_id: UserId,
) -> Vec<ResourceInvalidation> {
    let refresh_all = targets
        .iter()
        .any(|target| matches!(target, CacheTarget::All));
    let refresh_room = targets.iter().any(
        |target| matches!(target, CacheTarget::Room { room_id: target } if *target == room_id),
    );
    let refresh_user = targets.iter().any(|target| {
        matches!(
            target,
            CacheTarget::User { user_id: target } if *target == user_id
        )
    });
    let refresh_username = targets
        .iter()
        .any(|target| matches!(target, CacheTarget::Username { .. }));

    let mut invalidations = Vec::new();
    if refresh_all || refresh_room {
        push_unique(&mut invalidations, ResourceInvalidation::PlaybackState);
        push_unique(
            &mut invalidations,
            ResourceInvalidation::PlaybackSnapshot(PlaybackSnapshotInvalidation::Cache),
        );
        push_unique(&mut invalidations, ResourceInvalidation::RoomSettings);
        push_unique(&mut invalidations, ResourceInvalidation::PlaylistItems);
        push_unique(&mut invalidations, ResourceInvalidation::RoomMembers);
    }
    if refresh_user {
        push_unique(
            &mut invalidations,
            ResourceInvalidation::PlaybackSnapshot(PlaybackSnapshotInvalidation::Cache),
        );
        push_unique(&mut invalidations, ResourceInvalidation::RoomMembers);
    }
    if refresh_username {
        push_unique(&mut invalidations, ResourceInvalidation::RoomMembers);
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
    use chrono::Utc;
    use synctv_core::models::{
        ChatEventKind, ChatMessage, ChatMessageStatus, ChatMessageType, ChatMessageWithImages,
        Playlist, RoomPlaybackState,
    };

    fn room_id() -> RoomId {
        RoomId::expect_positive(101)
    }

    fn user_id() -> UserId {
        UserId::expect_positive(202)
    }

    fn chat_event() -> ChatMessageEvent {
        let now = Utc::now();
        ChatMessageEvent {
            event_id: "chat-event".to_string(),
            room_id: room_id(),
            actor_user_id: user_id(),
            kind: ChatEventKind::Created,
            message: ChatMessageWithImages {
                message: ChatMessage {
                    id: 1,
                    room_id: room_id(),
                    user_id: Some(user_id()),
                    client_message_id: Some("client-message".to_string()),
                    content: "hello".to_string(),
                    message_type: ChatMessageType::Text,
                    status: ChatMessageStatus::Active,
                    version: 1,
                    reply_to_message_id: None,
                    reply_to_message_created_at: None,
                    metadata: serde_json::Value::Object(Default::default()),
                    edited_at: None,
                    deleted_at: None,
                    deleted_by: None,
                    delete_reason: None,
                    created_at: now,
                },
                images: Vec::new(),
                reactions: Vec::new(),
            },
            occurred_at: now,
        }
    }

    #[test]
    fn playback_state_event_invalidates_state_and_snapshot() {
        let event = RealtimeEvent::PlaybackStateChanged {
            event_id: "evt".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "actor".to_string(),
            state: RoomPlaybackState::new(room_id()),
            timestamp: Utc::now(),
        };

        assert_eq!(
            resource_invalidations_for_room_event(&event),
            vec![
                ResourceInvalidation::PlaybackState,
                ResourceInvalidation::PlaybackSnapshot(
                    PlaybackSnapshotInvalidation::PlaybackStateChanged
                ),
            ]
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

    #[test]
    fn playlist_update_invalidates_items_and_dependent_snapshot() {
        let playlist_id = PlaylistId::expect_positive(303);
        let event = RealtimeEvent::PlaylistUpdated {
            event_id: "evt".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "actor".to_string(),
            playlist: Playlist {
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
                created_at: Utc::now(),
                updated_at: Utc::now(),
                version: 1,
            },
            timestamp: Utc::now(),
        };

        assert_eq!(
            resource_invalidations_for_room_event(&event),
            vec![
                ResourceInvalidation::PlaylistItems,
                ResourceInvalidation::PlaybackSnapshot(
                    PlaybackSnapshotInvalidation::PlaylistUpdated { playlist_id }
                ),
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
                user_id(),
            ),
            vec![
                ResourceInvalidation::PlaybackState,
                ResourceInvalidation::PlaybackSnapshot(PlaybackSnapshotInvalidation::Cache),
                ResourceInvalidation::RoomSettings,
                ResourceInvalidation::PlaylistItems,
                ResourceInvalidation::RoomMembers,
            ]
        );

        assert!(resource_invalidations_for_cache_targets(
            &[CacheTarget::Room {
                room_id: RoomId::expect_positive(999)
            }],
            room_id(),
            user_id(),
        )
        .is_empty());
    }

    #[test]
    fn username_cache_targets_refresh_member_lists_for_all_room_observers() {
        assert_eq!(
            resource_invalidations_for_cache_targets(
                &[CacheTarget::Username {
                    user_id: UserId::expect_positive(999)
                }],
                room_id(),
                user_id(),
            ),
            vec![ResourceInvalidation::RoomMembers]
        );
    }

    #[test]
    fn guest_presence_events_invalidate_room_members() {
        let joined = RealtimeEvent::GuestJoined {
            event_id: "guest-joined".to_string(),
            room_id: room_id(),
            guest_id: "gst_test".to_string(),
            username: "Guest".to_string(),
            permissions: synctv_core::models::RoomPermissionSet::default_guest(),
            role: synctv_proto::common::RoomMemberRole::Guest as i32,
            joined_at: Utc::now(),
            timestamp: Utc::now(),
        };
        let left = RealtimeEvent::GuestLeft {
            event_id: "guest-left".to_string(),
            room_id: room_id(),
            guest_id: "gst_test".to_string(),
            username: "Guest".to_string(),
            timestamp: Utc::now(),
        };

        assert_eq!(
            resource_invalidations_for_room_event(&joined),
            vec![ResourceInvalidation::RoomMembers]
        );
        assert_eq!(
            resource_invalidations_for_room_event(&left),
            vec![ResourceInvalidation::RoomMembers]
        );
    }
}
