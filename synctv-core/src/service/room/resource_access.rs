use std::collections::{HashMap, HashSet};

use crate::{
    models::{Media, MediaId, Playlist, PlaylistId, Room, RoomId, UserId},
    repository::{media::MediaListItem, playlist::PlaylistListItem},
    service::RoomService,
    Error, Result,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientResourceAvailability {
    Available,
    CreatorInactive,
}

impl ClientResourceAvailability {
    #[must_use]
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }
}

impl RoomService {
    fn playlist_client_availability(
        playlist: &Playlist,
        active_creators: &HashSet<UserId>,
    ) -> ClientResourceAvailability {
        match playlist.creator_id.as_ref() {
            Some(creator_id) if !active_creators.contains(creator_id) => {
                ClientResourceAvailability::CreatorInactive
            }
            _ => ClientResourceAvailability::Available,
        }
    }

    fn media_client_availability(
        media: &Media,
        active_creators: &HashSet<UserId>,
    ) -> ClientResourceAvailability {
        match media.creator_id.as_ref() {
            Some(creator_id) if !active_creators.contains(creator_id) => {
                ClientResourceAvailability::CreatorInactive
            }
            _ => ClientResourceAvailability::Available,
        }
    }

    fn room_client_availability(
        room: &Room,
        active_creators: &HashSet<UserId>,
    ) -> ClientResourceAvailability {
        if active_creators.contains(&room.created_by) {
            ClientResourceAvailability::Available
        } else {
            ClientResourceAvailability::CreatorInactive
        }
    }

    async fn load_active_creators<'a, I>(&self, creator_ids: I) -> Result<HashSet<UserId>>
    where
        I: IntoIterator<Item = &'a UserId>,
    {
        let unique_ids: Vec<UserId> = creator_ids
            .into_iter()
            .copied()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        if unique_ids.is_empty() {
            return Ok(HashSet::new());
        }

        Ok(self
            .user_service
            .get_users_by_ids(&unique_ids)
            .await?
            .into_iter()
            .filter(|user| user.status.is_active() && !user.is_banned)
            .map(|user| user.id)
            .collect())
    }

    async fn load_active_creators_eventually_consistent<'a, I>(
        &self,
        creator_ids: I,
    ) -> Result<HashSet<UserId>>
    where
        I: IntoIterator<Item = &'a UserId>,
    {
        let unique_ids: Vec<UserId> = creator_ids
            .into_iter()
            .copied()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        if unique_ids.is_empty() {
            return Ok(HashSet::new());
        }

        let mut creators = self
            .user_service
            .get_users_by_ids_eventually_consistent(&unique_ids)
            .await?;
        let loaded_ids = creators.iter().map(|user| user.id).collect::<HashSet<_>>();
        let missing_ids = unique_ids
            .iter()
            .copied()
            .filter(|user_id| !loaded_ids.contains(user_id))
            .collect::<Vec<_>>();
        if !missing_ids.is_empty() {
            creators.extend(self.user_service.get_users_by_ids(&missing_ids).await?);
        }

        Ok(creators
            .into_iter()
            .filter(|user| user.status.is_active() && !user.is_banned)
            .map(|user| user.id)
            .collect())
    }

    async fn ensure_resource_creator_is_active_for_client_access(
        &self,
        creator_id: Option<&UserId>,
        resource_kind: &'static str,
    ) -> Result<()> {
        let Some(creator_id) = creator_id else {
            return Ok(());
        };

        match self.user_service.get_user(creator_id).await {
            Ok(user) if user.status.is_active() && !user.is_banned => Ok(()),
            Ok(_) | Err(Error::NotFound(_)) => Err(Error::Authorization(format!(
                "{resource_kind} is unavailable because its creator is not active"
            ))),
            Err(error) => Err(error),
        }
    }

    pub(super) async fn ensure_room_creator_is_active_for_access(
        &self,
        room: &Room,
        actor_user_id: &UserId,
    ) -> Result<()> {
        let actor = self.user_service.get_user(actor_user_id).await?;
        if actor.role.is_admin_or_above() {
            return Ok(());
        }

        let creator = self.user_service.get_user(&room.created_by).await;
        match creator {
            Ok(user) if user.status.is_active() && !user.is_banned => Ok(()),
            Ok(_) | Err(Error::NotFound(_)) => Err(Error::Authorization(
                "Room is unavailable because its creator is not active".to_string(),
            )),
            Err(error) => Err(error),
        }
    }

    pub async fn ensure_client_usable_playlist(&self, playlist: &Playlist) -> Result<()> {
        if !playlist.is_dynamic() {
            return Ok(());
        }

        self.ensure_resource_creator_is_active_for_client_access(
            playlist.creator_id.as_ref(),
            "Dynamic playlist",
        )
        .await
    }

    pub async fn playlist_availability(
        &self,
        playlist: &Playlist,
    ) -> Result<ClientResourceAvailability> {
        let active_creators = self
            .load_active_creators(playlist.creator_id.iter())
            .await?;
        Ok(Self::playlist_client_availability(
            playlist,
            &active_creators,
        ))
    }

    pub async fn room_availability(&self, room: &Room) -> Result<ClientResourceAvailability> {
        let active_creators = self
            .load_active_creators(std::iter::once(&room.created_by))
            .await?;
        Ok(Self::room_client_availability(room, &active_creators))
    }

    pub async fn ensure_guest_room_available(&self, room: &Room) -> Result<()> {
        if room.is_banned {
            return Err(Error::Authorization(
                "This room has been banned".to_string(),
            ));
        }
        if room.status.is_closed() {
            return Err(Error::Authorization(
                "This room is closed and not accepting new connections".to_string(),
            ));
        }
        if !self.room_availability(room).await?.is_available() {
            return Err(Error::Authorization(
                "This room is currently unavailable".to_string(),
            ));
        }
        Ok(())
    }

    pub async fn room_availability_batch(
        &self,
        rooms: &[Room],
    ) -> Result<HashMap<RoomId, ClientResourceAvailability>> {
        let active_creators = self
            .load_active_creators(rooms.iter().map(|room| &room.created_by))
            .await?;
        Ok(rooms
            .iter()
            .map(|room| {
                (
                    room.id,
                    Self::room_client_availability(room, &active_creators),
                )
            })
            .collect())
    }

    pub async fn room_availability_batch_eventually_consistent(
        &self,
        rooms: &[Room],
    ) -> Result<HashMap<RoomId, ClientResourceAvailability>> {
        let active_creators = self
            .load_active_creators_eventually_consistent(rooms.iter().map(|room| &room.created_by))
            .await?;
        Ok(rooms
            .iter()
            .map(|room| {
                (
                    room.id,
                    Self::room_client_availability(room, &active_creators),
                )
            })
            .collect())
    }

    pub async fn playlist_availability_map(
        &self,
        playlists: &[Playlist],
    ) -> Result<HashMap<PlaylistId, ClientResourceAvailability>> {
        let active_creators = self
            .load_active_creators(
                playlists
                    .iter()
                    .filter_map(|playlist| playlist.creator_id.as_ref()),
            )
            .await?;

        Ok(playlists
            .iter()
            .map(|playlist| {
                (
                    playlist.id,
                    Self::playlist_client_availability(playlist, &active_creators),
                )
            })
            .collect())
    }

    pub async fn count_client_playlists(
        &self,
        room_id: &RoomId,
        parent_id: Option<&PlaylistId>,
        query: &crate::models::PlaylistListQuery,
    ) -> Result<i64> {
        self.playlist_repo
            .count_filtered_by_parent(room_id, parent_id, query)
            .await
    }

    pub async fn list_client_playlists(
        &self,
        room_id: &RoomId,
        parent_id: Option<&PlaylistId>,
        query: &crate::models::PlaylistListQuery,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<PlaylistListItem>> {
        self.playlist_repo
            .list_filtered_by_parent(room_id, parent_id, query, limit, offset)
            .await
    }

    pub async fn media_availability(&self, media: &Media) -> Result<ClientResourceAvailability> {
        let active_creators = self.load_active_creators(media.creator_id.iter()).await?;
        Ok(Self::media_client_availability(media, &active_creators))
    }

    pub async fn media_availability_map(
        &self,
        media: &[Media],
    ) -> Result<HashMap<MediaId, ClientResourceAvailability>> {
        let active_creators = self
            .load_active_creators(media.iter().filter_map(|item| item.creator_id.as_ref()))
            .await?;

        Ok(media
            .iter()
            .map(|item| {
                (
                    item.id,
                    Self::media_client_availability(item, &active_creators),
                )
            })
            .collect())
    }

    pub async fn count_client_media(
        &self,
        room_id: &RoomId,
        playlist_id: Option<&PlaylistId>,
        query: &crate::models::MediaListQuery,
    ) -> Result<i64> {
        self.media_repo
            .count_filtered_by_scope(room_id, playlist_id, query)
            .await
    }

    pub async fn list_client_media(
        &self,
        room_id: &RoomId,
        playlist_id: Option<&PlaylistId>,
        query: &crate::models::MediaListQuery,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<MediaListItem>> {
        self.media_repo
            .list_filtered_by_scope(room_id, playlist_id, query, limit, offset)
            .await
    }
}
