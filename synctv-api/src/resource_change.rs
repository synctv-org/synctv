use synctv_core::models::{MediaId, PlaylistId, RoomId, UserId};
use synctv_realtime::sync::{CacheTarget, RealtimeEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceInvalidation {
    PlaybackState,
    PlaybackSnapshot(PlaybackSnapshotInvalidation),
    RoomSettings,
    PlaylistItems,
    RoomMembers,
    ProviderCredential {
        user_id: UserId,
        provider: String,
        server_id: String,
    },
}

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
        | RealtimeEvent::UserLeft { .. }
        | RealtimeEvent::PermissionChanged { .. } => vec![ResourceInvalidation::RoomMembers],
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
    use synctv_core::models::{Playlist, RoomPlaybackState};

    fn room_id() -> RoomId {
        RoomId::expect_positive(101)
    }

    fn user_id() -> UserId {
        UserId::expect_positive(202)
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
}
